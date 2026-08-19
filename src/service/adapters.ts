/**
 * Service-side adapters for the KB engine ports.
 *
 * The mirror image of the file that stays behind in the host application: the
 * engine is identical, only the bindings differ. Here they point at the KB
 * service's OWN Postgres, SurrealDB and object store.
 */

import { AsyncLocalStorage } from "node:async_hooks"
import { S3Client, PutObjectCommand, GetObjectCommand, DeleteObjectCommand } from "@aws-sdk/client-s3"
import { prisma } from "./db"
import { getSurrealClient } from "@/lib/surrealdb"
import { emitProgress } from "./events"
import type {
  BlobStore,
  ConfigProvider,
  DocumentStore,
  EndpointResolver,
  JobProcessor,
  JobRecord,
  JobStore,
  KbRuntime,
  ProgressSink,
  VectorStore,
} from "@/lib/kb-runtime/ports"

// ─── Blob ────────────────────────────────────────────────────────────────────

const s3 = new S3Client({
  endpoint: process.env.S3_ENDPOINT || "http://localhost:9000",
  region: process.env.S3_REGION || "us-east-1",
  forcePathStyle: process.env.S3_ENABLE_PATH_STYLE !== "0",
  credentials: {
    accessKeyId: process.env.S3_ACCESS_KEY_ID || "",
    secretAccessKey: process.env.S3_SECRET_ACCESS_KEY || "",
  },
})
const BUCKET = process.env.S3_BUCKET || "rantai-kb"

function sanitizeFilename(name: string): string {
  return name.replace(/[^a-zA-Z0-9._-]/g, "_")
}

async function streamToBuffer(body: unknown): Promise<Buffer> {
  const chunks: Buffer[] = []
  for await (const chunk of body as AsyncIterable<Uint8Array>) chunks.push(Buffer.from(chunk))
  return Buffer.concat(chunks)
}

const blob: BlobStore = {
  async upload(key, body, contentType, meta) {
    await s3.send(
      new PutObjectCommand({ Bucket: BUCKET, Key: key, Body: body, ContentType: contentType, Metadata: meta })
    )
    return { size: body.length }
  },
  async download(key) {
    const res = await s3.send(new GetObjectCommand({ Bucket: BUCKET, Key: key }))
    return streamToBuffer(res.Body)
  },
  async delete(key) {
    await s3.send(new DeleteObjectCommand({ Bucket: BUCKET, Key: key }))
  },
  documentPath: (tenantId, documentId, filename) =>
    `documents/${tenantId || "global"}/${documentId}/${sanitizeFilename(filename)}`,
  assetPath: (tenantId, documentId, filename) =>
    `documents/${tenantId || "global"}/${documentId}/assets/${sanitizeFilename(filename)}`,
}

// ─── Progress ────────────────────────────────────────────────────────────────

const progress: ProgressSink = {
  async emit(tenantId, event, payload) {
    emitProgress(tenantId, event, payload)
  },
}

// ─── Jobs ────────────────────────────────────────────────────────────────────

const jobs: JobStore = {
  async create(input) {
    try {
      const job = await prisma.ingestJob.create({
        data: {
          tenantId: input.organizationId ?? "",
          externalRef: input.userId,
          filename: input.filename,
          fileSize: input.fileSize,
          mimeType: input.mimeType,
          s3Key: input.s3Key,
          documentId: input.documentId,
          status: "pending",
          step: "queued",
          params: input.params as object,
        },
        select: { id: true },
      })
      return job.id
    } catch (err) {
      console.warn("[kb] job create failed:", err)
      return null
    }
  },

  async claimNextPending(): Promise<JobRecord | null> {
    const rows = await prisma.$queryRaw<
      Array<{
        id: string
        tenantId: string
        externalRef: string | null
        documentId: string | null
        s3Key: string | null
        filename: string
        mimeType: string | null
        attempt: number
        params: Record<string, unknown> | null
      }>
    >`
      UPDATE "IngestJob"
         SET status = 'processing', "startedAt" = now(), "updatedAt" = now(), step = 'queued', progress = 0
       WHERE id = (
         SELECT id FROM "IngestJob"
          WHERE status = 'pending'
          ORDER BY "createdAt" ASC
          LIMIT 1
          FOR UPDATE SKIP LOCKED
       )
      RETURNING id, "tenantId", "externalRef", "documentId", "s3Key", filename, "mimeType", attempt, params
    `
    const row = rows[0]
    if (!row) return null
    // The engine's JobRecord still calls the tenant scope organizationId.
    return {
      id: row.id,
      organizationId: row.tenantId,
      userId: row.externalRef,
      documentId: row.documentId,
      s3Key: row.s3Key,
      filename: row.filename,
      mimeType: row.mimeType,
      attempt: row.attempt,
      params: row.params,
    }
  },

  async updateProgress(jobId, data) {
    await prisma.ingestJob
      .update({ where: { id: jobId }, data })
      .catch((err: unknown) => console.warn("[kb] progress update failed:", err))
  },

  async finish(jobId, data) {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        await prisma.ingestJob.update({ where: { id: jobId }, data })
        return
      } catch (err) {
        if (attempt === 0) {
          await new Promise((r) => setTimeout(r, 500))
          continue
        }
        console.warn("[kb] terminal job update failed:", err)
      }
    }
  },

  async touch(jobId) {
    await prisma.ingestJob.update({ where: { id: jobId }, data: { updatedAt: new Date() } }).catch(() => {})
  },

  async reclaimStale(staleMs, maxAttempts) {
    const cutoff = new Date(Date.now() - staleMs)
    const stale = await prisma.ingestJob.findMany({
      where: { status: "processing", updatedAt: { lt: cutoff } },
      select: { id: true, attempt: true, documentId: true },
    })
    for (const job of stale) {
      if (job.attempt >= maxAttempts) {
        await prisma.ingestJob
          .update({ where: { id: job.id }, data: { status: "failed", error: "ingest stalled (max attempts reached)" } })
          .catch(() => {})
        if (job.documentId) {
          await prisma.document.update({ where: { id: job.documentId }, data: { status: "failed" } }).catch(() => {})
        }
      } else {
        await prisma.ingestJob
          .update({
            where: { id: job.id },
            data: { status: "pending", attempt: { increment: 1 }, startedAt: null, step: "queued", progress: 0 },
          })
          .catch(() => {})
      }
    }
    return stale.length
  },

  async listReapable(maxAgeDays, limit) {
    const cutoff = new Date(Date.now() - maxAgeDays * 86_400_000)
    return prisma.ingestJob.findMany({
      where: { status: "failed", updatedAt: { lt: cutoff }, s3Key: { not: null } },
      select: { id: true, s3Key: true },
      take: limit,
    })
  },

  async clearS3Key(jobId) {
    await prisma.ingestJob.update({ where: { id: jobId }, data: { s3Key: null } })
  },
}

// ─── Documents ───────────────────────────────────────────────────────────────

const META_SELECT = { id: true, title: true, categories: true, subcategory: true } as const

/**
 * Tenant scoping for retrieval.
 *
 * The engine's DocumentStore interface has no tenant parameter — retrieval
 * resolves allowed document ids first and the vector store is filtered by
 * those ids. To make that safe for a shared service, every request and every
 * job runs inside `withTenant`, and this adapter refuses to answer outside
 * one. AsyncLocalStorage (not a module variable) is what keeps concurrent
 * requests from reading each other's scope.
 */
const tenantContext = new AsyncLocalStorage<string>()

export function withTenant<T>(tenantId: string, fn: () => Promise<T>): Promise<T> {
  if (!tenantId) throw new Error("[kb] withTenant requires a tenant id")
  return tenantContext.run(tenantId, fn)
}

export function activeTenant(): string | null {
  return tenantContext.getStore() ?? null
}

function tenantScope(): { tenantId: string } {
  const tenantId = tenantContext.getStore()
  if (!tenantId) {
    throw new Error("[kb] no active tenant — every KB operation must run inside withTenant()")
  }
  return { tenantId }
}

const documents: DocumentStore = {
  async findAliveIdsByFilter(filter) {
    const where: Record<string, unknown> = { ...tenantScope(), deletedAt: null }
    if (filter.category) where.categories = { has: filter.category }
    if (filter.groupIds && filter.groupIds.length > 0) {
      where.groups = { some: { knowledgeBaseId: { in: filter.groupIds } } }
    }
    const rows = await prisma.document.findMany({ where, select: { id: true } })
    return rows.map((r: { id: string }) => r.id)
  },

  async findAliveMetaByIds(ids) {
    if (ids.length === 0) return []
    return prisma.document.findMany({
      where: { ...tenantScope(), id: { in: ids }, deletedAt: null },
      select: META_SELECT,
    })
  },

  async findById(id) {
    return prisma.document.findFirst({
      where: { ...tenantScope(), id },
      select: { id: true, title: true, deletedAt: true },
    })
  },

  async filterVisibleIds(ids) {
    if (ids.length === 0) return []
    const rows = await prisma.document.findMany({
      where: { ...tenantScope(), id: { in: ids }, deletedAt: null },
      select: { id: true },
    })
    return rows.map((r: { id: string }) => r.id)
  },

  async listAll() {
    return prisma.document.findMany({
      where: { ...tenantScope(), deletedAt: null },
      select: { ...META_SELECT, createdAt: true },
    })
  },

  async deleteById(id) {
    await prisma.document.deleteMany({ where: { ...tenantScope(), id } })
  },

  async deleteAll() {
    await prisma.document.deleteMany({ where: tenantScope() })
  },

  async setStatus(documentId, status) {
    await prisma.document
      .update({ where: { id: documentId }, data: { status } })
      .catch((err: unknown) => console.error(`[kb] setStatus failed for ${documentId}:`, err))
  },

  async updateMetadata(documentId, patch) {
    const existing = await prisma.document.findUnique({ where: { id: documentId }, select: { metadata: true } })
    const merged = { ...((existing?.metadata as Record<string, unknown>) ?? {}), ...patch }
    await prisma.document.update({ where: { id: documentId }, data: { metadata: merged as object } })
  },

  async setMetadataFlag(documentId, key, value) {
    try {
      await prisma.$executeRaw`
        UPDATE "Document"
        SET "metadata" = jsonb_set(
          COALESCE("metadata", '{}'::jsonb),
          ${`{${key}}`}::text[],
          to_jsonb(${value}::boolean),
          true
        )
        WHERE "id" = ${documentId}
      `
    } catch (err) {
      console.error("[kb] setMetadataFlag failed:", err)
    }
  },

  async recordRetrievalHits(documentIds) {
    if (documentIds.length === 0) return
    await prisma.document
      .updateMany({
        where: { id: { in: documentIds } },
        data: { retrievalCount: { increment: 1 }, lastRetrievedAt: new Date() },
      })
      .catch(() => {})
  },
}

// ─── Vector store ────────────────────────────────────────────────────────────

const vectors: VectorStore = {
  async query<T = unknown>(sql: string, vars?: Record<string, unknown>) {
    const client = await getSurrealClient()
    return client.query<T>(sql, vars)
  },
  async relate(from, relation, to, props) {
    const client = await getSurrealClient()
    await client.relate(from, relation, to, props)
  },
  async cleanupDocumentIntelligence(documentId) {
    const client = await getSurrealClient()
    return client.cleanupDocumentIntelligence(documentId)
  },
  async healthCheck() {
    const client = await getSurrealClient()
    return client.healthCheck()
  },
}

// ─── Config + endpoints ──────────────────────────────────────────────────────

/**
 * The service takes its configuration from the environment only — there is no
 * admin-settings table to override it. Deployments change env, not rows.
 */
const config: ConfigProvider = {
  async readKbSetting() {
    return null
  },
  async resolveProvider() {
    return null
  },
}

const endpoints: EndpointResolver = {
  resolveModel() {
    return null
  },
}

// ─── Job execution ───────────────────────────────────────────────────────────

const processor: JobProcessor = {
  async process(job, onProgress) {
    const { processIngestJob } = await import("./ingest-runner")
    return processIngestJob(job, onProgress)
  },
}

export function serviceKbRuntime(): KbRuntime {
  return { blob, progress, jobs, documents, vectors, config, endpoints, processor }
}
