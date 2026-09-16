import { randomUUID } from "node:crypto"
import { prisma } from "./db"
import { withTenant } from "./adapters"
import { authenticate, hasScope, restrictKnowledgeBases, type AuthContext } from "./auth"
import { subscribe } from "./events"
import { kb } from "@/lib/kb-runtime/runtime"
import { createIngestJob } from "@/lib/ingest/job"
import { parseFigureMode } from "@/lib/ingest/pipeline-policy"
import { KB_ACCEPTED_EXTENSIONS, KB_MAX_FILE_BYTES } from "@/lib/files/mime-types"
import { smartRetrieve, smartHybridRetrieve } from "@/lib/rag"
import { searchByDocumentIds } from "@/lib/rag/vector-store"

/**
 * The KB service HTTP API (`/v1`).
 *
 * Every route authenticates to a tenant and runs inside `withTenant`, so the
 * engine can never read across tenants even if a caller omits a filter.
 */

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", "access-control-allow-origin": "*" },
  })

const error = (message: string, status: number, extra?: Record<string, unknown>) =>
  json({ error: message, ...extra }, status)

const CORS_HEADERS: Record<string, string> = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,PATCH,DELETE,OPTIONS",
  "access-control-allow-headers": "authorization,content-type,x-api-key",
  "access-control-max-age": "86400",
}

// ─── Routes ──────────────────────────────────────────────────────────────────

/** POST /v1/documents — multipart upload; returns 202 + job id. */
async function ingestDocument(request: Request, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)

  const form = await request.formData()
  const file = form.get("file")
  if (!(file instanceof File)) return error("No file provided (field: file)", 400)

  const ext = file.name.slice(file.name.lastIndexOf(".")).toLowerCase()
  if (!KB_ACCEPTED_EXTENSIONS.includes(ext)) {
    return error(`"${file.name}" has an unsupported type. See GET /v1/formats for the accepted list.`, 415)
  }
  if (file.size > KB_MAX_FILE_BYTES) {
    const maxMB = Math.round(KB_MAX_FILE_BYTES / (1024 * 1024))
    return error(`"${file.name}" is ${(file.size / (1024 * 1024)).toFixed(1)}MB — the limit is ${maxMB}MB.`, 413)
  }

  const title = (form.get("title") as string | null) || file.name.replace(/\.[^/.]+$/, "")
  const categories = parseList(form.get("categories"))
  const subcategory = (form.get("subcategory") as string | null) || null
  const knowledgeBaseIds = restrictKnowledgeBases(auth, parseList(form.get("knowledgeBaseIds"))) ?? []
  const figureMode = parseFigureMode(form.get("figures"))
  const documentType = (form.get("documentType") as string | null) || undefined

  return withTenant(auth.tenantId, async () => {
    const documentId = randomUUID()
    const buffer = Buffer.from(await file.arrayBuffer())
    const s3Key = kb("blob").documentPath(auth.tenantId, documentId, file.name)

    try {
      await kb("blob").upload(s3Key, buffer, file.type || "application/octet-stream", { documentId })
    } catch (err) {
      console.error("[kb] upload failed:", err)
      return error("Failed to store the uploaded file. Please try again.", 502)
    }

    await prisma.document.create({
      data: {
        id: documentId,
        tenantId: auth.tenantId,
        title,
        content: "",
        categories,
        subcategory,
        s3Key,
        fileSize: buffer.length,
        mimeType: file.type || null,
        status: "processing",
        externalRef: (form.get("externalRef") as string | null) || null,
        groups: knowledgeBaseIds.length
          ? { create: knowledgeBaseIds.map((knowledgeBaseId) => ({ knowledgeBaseId })) }
          : undefined,
      },
    })

    const jobId = await createIngestJob({
      organizationId: auth.tenantId,
      userId: (form.get("externalRef") as string | null) || null,
      filename: file.name,
      fileSize: buffer.length,
      mimeType: file.type || null,
      s3Key,
      documentId,
      params: { title, categories, subcategory, figureMode, documentType, useCombined: true },
    })

    return json({ id: documentId, jobId, status: "processing", title, knowledgeBaseIds }, 202)
  })
}

/** POST /v1/search — retrieval. The reason this service exists. */
async function search(request: Request, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)

  const body = (await request.json().catch(() => ({}))) as {
    query?: string
    maxChunks?: number
    knowledgeBaseIds?: string[]
    documentIds?: string[]
    category?: string
    hybrid?: boolean
    format?: "chunks" | "context"
  }
  if (!body.query || typeof body.query !== "string") return error("Body must include a 'query' string", 400)

  const knowledgeBaseIds = restrictKnowledgeBases(auth, body.knowledgeBaseIds)
  if (knowledgeBaseIds && knowledgeBaseIds.length === 0) {
    return json({ chunks: [], context: "", note: "API key is not bound to any of the requested knowledge bases" })
  }

  const query = body.query

  return withTenant(auth.tenantId, async () => {
    const options = {
      maxChunks: Math.min(Math.max(body.maxChunks ?? 8, 1), 50),
      categoryFilter: body.category,
      groupIds: knowledgeBaseIds,
    }

    if (body.documentIds?.length) {
      // Narrow to specific documents, but only ones this tenant owns.
      const allowed = await kb("documents").filterVisibleIds(body.documentIds, auth.tenantId)
      if (allowed.length === 0) return json({ chunks: [], context: "" })
      const chunks = await searchByDocumentIds(query, allowed, options.maxChunks)
      return json({
        chunks,
        context:
          body.format === "context"
            ? chunks.map((c, i) => `[${i + 1}] ${c.content}`).join("\n\n")
            : undefined,
      })
    }

    const result = body.hybrid
      ? await smartHybridRetrieve(query, options)
      : await smartRetrieve(query, options)

    return json(result)
  })
}

/** GET /v1/documents */
async function listDocuments(request: Request, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)
  const url = new URL(request.url)
  const limit = Math.min(Math.max(Number(url.searchParams.get("limit") ?? 100), 1), 500)
  const kbFilter = restrictKnowledgeBases(auth, parseList(url.searchParams.get("knowledgeBaseIds")))

  const documents = await prisma.document.findMany({
    where: {
      tenantId: auth.tenantId,
      deletedAt: null,
      ...(kbFilter?.length ? { groups: { some: { knowledgeBaseId: { in: kbFilter } } } } : {}),
    },
    select: {
      id: true,
      title: true,
      categories: true,
      subcategory: true,
      fileType: true,
      fileSize: true,
      status: true,
      createdAt: true,
      groups: { select: { knowledgeBaseId: true } },
    },
    orderBy: { createdAt: "desc" },
    take: limit,
  })

  return json({
    documents: documents.map((d: (typeof documents)[number]) => ({
      ...d,
      groups: undefined,
      knowledgeBaseIds: d.groups.map((g: { knowledgeBaseId: string }) => g.knowledgeBaseId),
    })),
  })
}

const DOCUMENT_DETAIL_SELECT = {
  id: true,
  title: true,
  categories: true,
  subcategory: true,
  fileType: true,
  fileSize: true,
  mimeType: true,
  status: true,
  createdAt: true,
  updatedAt: true,
  s3Key: true,
  groups: { select: { knowledgeBaseId: true } },
} as const

/** Row shape read back from SurrealDB's `document_chunk` — see storeChunks() in
 *  src/lib/rag/vector-store.ts for what's actually written (chunk_index is its
 *  own column; section/page/chunkType live inside metadata). */
type SurrealChunkRow = {
  id: unknown
  content: string
  chunk_index: number
  metadata?: { section?: string; page?: number; chunkType?: string } | null
}

async function fetchOrderedChunks(documentId: string) {
  const store = kb("vectors")
  const result = await store.query<SurrealChunkRow>(
    "SELECT id, content, chunk_index, metadata FROM document_chunk WHERE document_id = $document_id ORDER BY chunk_index ASC",
    { document_id: documentId }
  )
  const rows = result[0]?.result ?? []
  return rows.map((row) => ({
    id: String(row.id),
    chunkIndex: row.chunk_index,
    content: row.content,
    chunkType: row.metadata?.chunkType ?? null,
    section: row.metadata?.section ?? null,
    page: row.metadata?.page ?? null,
  }))
}

/** GET /v1/documents/:id — detail plus its chunks, ordered by chunkIndex. */
async function getDocument(id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)

  return withTenant(auth.tenantId, async () => {
    // Tenant ownership resolves through Postgres FIRST — same reasoning as
    // getDocumentIntelligence below: SurrealDB has no tenant of its own, so a
    // caller-supplied id must never reach it before we know this tenant owns it.
    const doc = await prisma.document.findFirst({
      where: { id, tenantId: auth.tenantId, deletedAt: null },
      select: DOCUMENT_DETAIL_SELECT,
    })
    if (!doc) return error("Not found", 404)

    const chunks = await fetchOrderedChunks(id)

    return json({
      document: {
        id: doc.id,
        title: doc.title,
        categories: doc.categories,
        subcategory: doc.subcategory,
        fileType: doc.fileType,
        fileSize: doc.fileSize,
        mimeType: doc.mimeType,
        status: doc.status,
        createdAt: doc.createdAt,
        updatedAt: doc.updatedAt,
        knowledgeBaseIds: doc.groups.map((g) => g.knowledgeBaseId),
        chunkCount: chunks.length,
      },
      chunks,
    })
  })
}

/** PATCH /v1/documents/:id — metadata edits; knowledgeBaseIds absent = untouched, [] = clear. */
async function patchDocument(request: Request, id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)

  const body = (await request.json().catch(() => ({}))) as {
    title?: string
    categories?: string[]
    subcategory?: string | null
    knowledgeBaseIds?: string[]
  }

  return withTenant(auth.tenantId, async () => {
    const existing = await prisma.document.findFirst({ where: { id, tenantId: auth.tenantId, deletedAt: null } })
    if (!existing) return error("Not found", 404)

    const data: Record<string, unknown> = {}
    if (body.title !== undefined) data.title = body.title
    if (body.categories !== undefined) data.categories = body.categories
    if (body.subcategory !== undefined) data.subcategory = body.subcategory

    if (body.knowledgeBaseIds !== undefined) {
      // Narrow to what this key is allowed to bind AND what this tenant
      // actually owns — a caller-supplied id for another tenant's knowledge
      // base must silently drop rather than create a cross-tenant link.
      const requested = restrictKnowledgeBases(auth, body.knowledgeBaseIds) ?? []
      const owned = requested.length
        ? await prisma.knowledgeBase.findMany({
            where: { tenantId: auth.tenantId, id: { in: requested } },
            select: { id: true },
          })
        : []
      const ownedIds = owned.map((k) => k.id)

      await prisma.$transaction([
        prisma.documentGroup.deleteMany({ where: { documentId: id } }),
        ...(ownedIds.length
          ? [
              prisma.documentGroup.createMany({
                data: ownedIds.map((knowledgeBaseId) => ({ documentId: id, knowledgeBaseId })),
              }),
            ]
          : []),
      ])
    }

    if (Object.keys(data).length > 0) {
      await prisma.document.update({ where: { id }, data })
    }

    // Scoped again although `id` was authorised above: this is the line most
    // likely to be copied into a new handler, and unscoped it would read any
    // tenant's document. Every lookup in this file carries the tenant.
    const updated = await prisma.document.findFirst({
      where: { id, tenantId: auth.tenantId },
      select: DOCUMENT_DETAIL_SELECT,
    })
    if (!updated) return error("Not found", 404)

    return json({
      document: {
        id: updated.id,
        title: updated.title,
        categories: updated.categories,
        subcategory: updated.subcategory,
        fileType: updated.fileType,
        fileSize: updated.fileSize,
        mimeType: updated.mimeType,
        status: updated.status,
        createdAt: updated.createdAt,
        updatedAt: updated.updatedAt,
        knowledgeBaseIds: updated.groups.map((g) => g.knowledgeBaseId),
      },
    })
  })
}

/** GET /v1/documents/:id/raw — stream the stored original bytes through the service. */
async function getDocumentRaw(id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)

  return withTenant(auth.tenantId, async () => {
    const doc = await prisma.document.findFirst({
      where: { id, tenantId: auth.tenantId, deletedAt: null },
      select: { id: true, s3Key: true, mimeType: true, title: true },
    })
    if (!doc) return error("Not found", 404)
    if (!doc.s3Key) return error("Document has no stored file", 404)

    let buffer: Buffer
    try {
      buffer = await kb("blob").download(doc.s3Key)
    } catch (err) {
      console.error(`[kb] raw download failed for ${id}:`, err)
      return error("Failed to load the stored file", 502)
    }

    return new Response(new Uint8Array(buffer), {
      status: 200,
      headers: {
        "content-type": doc.mimeType || "application/octet-stream",
        "content-length": String(buffer.length),
        "access-control-allow-origin": "*",
      },
    })
  })
}

/** DELETE /v1/documents/:id — soft delete by default (deletedAt); ?hard=true removes everything now. */
async function deleteDocument(id: string, auth: AuthContext, hard: boolean): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)
  return withTenant(auth.tenantId, async () => {
    const doc = await prisma.document.findFirst({ where: { id, tenantId: auth.tenantId } })
    if (!doc) return error("Not found", 404)

    if (!hard) {
      // Soft delete: the row stays, deletedAt is what every read path filters
      // on (listDocuments, getDocument, and search's findAliveMetaByIds join —
      // see vector-store.ts). Chunks and the S3 object are left alone; a real
      // hard delete or retention sweep cleans them up later.
      await prisma.document.update({ where: { id }, data: { deletedAt: new Date() } })
      return json({ ok: true })
    }

    const { deleteChunksByDocumentId } = await import("@/lib/rag")
    await deleteChunksByDocumentId(id).catch((err) => console.warn("[kb] chunk delete failed:", err))
    if (doc.s3Key) await kb("blob").delete(doc.s3Key).catch(() => {})
    await prisma.document.delete({ where: { id } })
    return json({ ok: true })
  })
}

// ─── Document intelligence (entities/relations) ─────────────────────────────

/** Shape actually written to SurrealDB's `entity` table — see
 * src/lib/ingest/index-document.ts (the live write path) and
 * src/lib/document-intelligence/pipeline.ts (the standalone pipeline). */
type SurrealEntityRow = {
  id: unknown
  name: string
  type: string
  confidence: number
  metadata?: Record<string, unknown> | null
}

/** Shape of a graph edge row. Relation *type* is the table name the edge
 * lives in (RELATE x->WORKS_FOR->y), not a stored column — index-document.ts
 * never writes a `relation_type` field, only pipeline.ts's unused
 * storeResults() does, so it is read back opportunistically when present. */
type SurrealRelationRow = {
  id: unknown
  in: unknown
  out: unknown
  confidence: number
  relation_type?: string
  context?: string
  metadata?: Record<string, unknown> | null
}

/**
 * Document.status is only ever ready | processing | failed (prisma/schema.prisma) —
 * there is no independent status for entity/relation extraction. It runs (or
 * is skipped by ingest policy) as one internal step of the same job that does
 * chunking and embedding, and its own failures are caught and swallowed as
 * non-fatal (see index-document.ts). So "ready" becomes "completed": the job
 * reached its terminal state, which holds whether or not any entities were
 * found — an empty graph is a legitimate result, not a failure.
 *
 * "pending" is never returned: a document's status is already "processing"
 * from the instant it's created (see ingestDocument above), before any job
 * has been claimed, so there is no observable state that means "not started
 * yet" distinct from "processing".
 */
function deriveIntelligenceStatus(documentStatus: string): "pending" | "processing" | "completed" | "failed" {
  if (documentStatus === "processing") return "processing"
  if (documentStatus === "failed") return "failed"
  return "completed"
}

/** GET /v1/documents/:id/intelligence — entities + relations extracted for a document. */
async function getDocumentIntelligence(id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)

  return withTenant(auth.tenantId, async () => {
    // SECURITY: SurrealDB has no tenant concept of its own — entity and
    // relation rows carry only a `document_id` (see pipeline.ts / index-
    // document.ts), so nothing stops a query keyed on a caller-supplied id
    // from crossing tenants. Resolving the document through the tenant-
    // scoped Postgres store FIRST, and returning the exact same 404 a
    // nonexistent id gets, is the only thing standing between this endpoint
    // and a caller enumerating another tenant's document ids to read their
    // graph. Do not remove this as a "redundant" lookup before touching
    // SurrealDB below — it is the tenant check.
    const doc = await prisma.document.findFirst({
      where: { id, tenantId: auth.tenantId, deletedAt: null },
      select: { id: true, status: true },
    })
    if (!doc) return error("Not found", 404)

    const store = kb("vectors")

    const entityResult = await store.query<SurrealEntityRow>("SELECT * FROM entity WHERE document_id = $id", { id })
    const entityRows = entityResult[0]?.result ?? []

    // Relations live in dynamic tables named after their relation type
    // (RELATE x->WORKS_FOR->y creates table `WORKS_FOR`), not one shared
    // table — the same reason cleanupDocumentIntelligence() in
    // src/lib/surrealdb/client.ts has to enumerate them via `INFO FOR DB`
    // before it can delete them.
    const relationRows: Array<SurrealRelationRow & { table: string }> = []
    try {
      const dbInfo = await store.query<{ tables?: Record<string, unknown> }>("INFO FOR DB")
      // Every statement's row lands in result[0] (SurrealDBClient.normalizeQueryResult
      // wraps a single non-recordset value as `{ result: [value] }`) — INFO FOR
      // DB is one statement returning one info object.
      const tables = dbInfo[0]?.result?.[0]?.tables
      const relationTables = tables ? Object.keys(tables).filter((t) => t !== "entity" && t !== "document_chunk") : []

      for (const table of relationTables) {
        try {
          const rel = await store.query<SurrealRelationRow>(`SELECT * FROM ${table} WHERE document_id = $id`, { id })
          for (const row of rel[0]?.result ?? []) relationRows.push({ ...row, table })
        } catch (err) {
          // A relation table can still be listed in schema metadata after its
          // rows are gone (SurrealDB doesn't drop empty dynamic tables), or
          // fail transiently — skip it rather than fail the whole read.
          console.warn(`[kb] intelligence: relation table "${table}" query failed:`, err)
        }
      }
    } catch (err) {
      console.warn("[kb] intelligence: relation table discovery failed:", err)
    }

    const entities = entityRows.map((row) => ({
      id: String(row.id),
      name: row.name,
      type: row.type,
      confidence: Number(row.confidence ?? 0),
      metadata: row.metadata && typeof row.metadata === "object" ? row.metadata : {},
    }))

    const relations = relationRows.map((row) => ({
      id: String(row.id),
      in: String(row.in),
      out: String(row.out),
      relation_type: row.relation_type ?? row.table,
      confidence: Number(row.confidence ?? 0),
      metadata: {
        ...(row.metadata && typeof row.metadata === "object" ? row.metadata : {}),
        // index-document.ts (the live write path) stores context as a flat
        // column, not nested under metadata — surface it either way.
        context: row.context ?? (row.metadata as Record<string, unknown> | undefined)?.context,
      },
    }))

    return json({
      entities,
      relations,
      status: deriveIntelligenceStatus(doc.status),
      stats: {
        totalEntities: entities.length,
        totalRelations: relations.length,
        entityTypes: new Set(entities.map((e) => e.type)).size,
        relationTypes: new Set(relations.map((r) => r.relation_type)).size,
      },
    })
  })
}

/** GET /v1/jobs/:id */
async function getJob(id: string, auth: AuthContext): Promise<Response> {
  const job = await prisma.ingestJob.findFirst({ where: { id, tenantId: auth.tenantId } })
  if (!job) return error("Not found", 404)
  return json({
    id: job.id,
    status: job.status,
    step: job.step,
    progress: job.progress,
    stepCurrent: job.stepCurrent,
    stepTotal: job.stepTotal,
    etaSeconds: job.etaSeconds,
    attempt: job.attempt,
    error: job.error,
    documentId: job.documentId,
  })
}

/**
 * POST /v1/jobs/:id/retry — only a failed job can be retried (409 otherwise).
 * Resets the row back to "pending" with the same shape claimNextPendingJob()
 * expects (see JobStore.claimNextPending in service/adapters.ts) so the
 * existing worker picks it back up on its next poll exactly like a fresh job.
 */
async function retryJob(id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)

  return withTenant(auth.tenantId, async () => {
    const job = await prisma.ingestJob.findFirst({ where: { id, tenantId: auth.tenantId } })
    if (!job) return error("Not found", 404)
    if (job.status !== "failed") return error("Only a failed job can be retried", 409)

    await prisma.ingestJob.update({
      where: { id: job.id },
      data: {
        status: "pending",
        attempt: { increment: 1 },
        error: null,
        step: "queued",
        progress: 0,
        stepCurrent: null,
        stepTotal: null,
        etaSeconds: null,
        startedAt: null,
      },
    })
    if (job.documentId) {
      await prisma.document.update({ where: { id: job.documentId }, data: { status: "processing" } }).catch(() => {})
    }

    return json({ jobId: job.id })
  })
}

/** GET /v1/knowledge-bases + POST /v1/knowledge-bases */
async function knowledgeBases(request: Request, auth: AuthContext): Promise<Response> {
  if (request.method === "POST") {
    if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)
    const body = (await request.json().catch(() => ({}))) as { name?: string; description?: string; color?: string }
    if (!body.name) return error("Body must include 'name'", 400)
    const created = await prisma.knowledgeBase.create({
      data: { tenantId: auth.tenantId, name: body.name, description: body.description, color: body.color },
    })
    return json(created, 201)
  }

  const rows = await prisma.knowledgeBase.findMany({
    where: {
      tenantId: auth.tenantId,
      ...(auth.knowledgeBaseIds.length ? { id: { in: auth.knowledgeBaseIds } } : {}),
    },
    select: { id: true, name: true, description: true, color: true, _count: { select: { documents: true } } },
    orderBy: { name: "asc" },
  })
  return json({
    knowledgeBases: rows.map((r: (typeof rows)[number]) => ({
      id: r.id,
      name: r.name,
      description: r.description,
      color: r.color,
      documentCount: r._count.documents,
    })),
  })
}

/** PATCH /v1/knowledge-bases/:id + DELETE /v1/knowledge-bases/:id */
async function knowledgeBaseById(request: Request, id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)

  return withTenant(auth.tenantId, async () => {
    const existing = await prisma.knowledgeBase.findFirst({ where: { id, tenantId: auth.tenantId } })
    if (!existing) return error("Not found", 404)

    if (request.method === "DELETE") {
      // DocumentGroup FKs to KnowledgeBase with onDelete: Cascade (see
      // prisma/schema.prisma) so Postgres would clean these up on its own,
      // but the link rows are deleted explicitly here anyway — the contract
      // is "removes the base and its links, never documents", and Document
      // has no relation to KnowledgeBase at all, so there is nothing here
      // that could reach it either way.
      await prisma.$transaction([
        prisma.documentGroup.deleteMany({ where: { knowledgeBaseId: id } }),
        prisma.knowledgeBase.delete({ where: { id } }),
      ])
      return json({ ok: true })
    }

    const body = (await request.json().catch(() => ({}))) as {
      name?: string
      description?: string
      color?: string
    }
    const data: Record<string, unknown> = {}
    if (body.name !== undefined) data.name = body.name
    if (body.description !== undefined) data.description = body.description
    if (body.color !== undefined) data.color = body.color

    const updated = Object.keys(data).length
      ? await prisma.knowledgeBase.update({ where: { id }, data })
      : existing
    return json({ knowledgeBase: updated })
  })
}

/** GET /v1/categories + POST /v1/categories */
async function categories(request: Request, auth: AuthContext): Promise<Response> {
  if (request.method === "POST") {
    if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)
    const body = (await request.json().catch(() => ({}))) as { name?: string; label?: string; color?: string }
    if (!body.name || !body.label) return error("Body must include 'name' and 'label'", 400)
    const created = await prisma.category.create({
      data: { tenantId: auth.tenantId, name: body.name, label: body.label, color: body.color },
    })
    return json({ category: { id: created.id, name: created.name, label: created.label, color: created.color } }, 201)
  }

  if (!hasScope(auth, "kb:read")) return error("API key lacks scope kb:read", 403)
  const rows = await prisma.category.findMany({
    where: { tenantId: auth.tenantId },
    select: { id: true, name: true, label: true, color: true },
    orderBy: { name: "asc" },
  })
  return json({ categories: rows })
}

/** PATCH /v1/categories/:id + DELETE /v1/categories/:id */
async function categoryById(request: Request, id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)

  return withTenant(auth.tenantId, async () => {
    const existing = await prisma.category.findFirst({ where: { id, tenantId: auth.tenantId } })
    if (!existing) return error("Not found", 404)

    if (request.method === "DELETE") {
      await prisma.category.delete({ where: { id } })
      return json({ ok: true })
    }

    const body = (await request.json().catch(() => ({}))) as { name?: string; label?: string; color?: string }
    const data: Record<string, unknown> = {}
    if (body.name !== undefined) data.name = body.name
    if (body.label !== undefined) data.label = body.label
    if (body.color !== undefined) data.color = body.color

    const updated = Object.keys(data).length ? await prisma.category.update({ where: { id }, data }) : existing
    return json({ category: { id: updated.id, name: updated.name, label: updated.label, color: updated.color } })
  })
}

/** GET /v1/events?…  — server-sent ingest progress for the tenant. */
function events(auth: AuthContext): Response {
  const stream = new ReadableStream({
    start(controller) {
      const encoder = new TextEncoder()
      controller.enqueue(encoder.encode(": connected\n\n"))
      const unsubscribe = subscribe(auth.tenantId, (frame) => controller.enqueue(encoder.encode(frame)))
      const keepAlive = setInterval(() => controller.enqueue(encoder.encode(": ping\n\n")), 25_000)
      // @ts-expect-error — attach for cancel()
      controller._cleanup = () => {
        clearInterval(keepAlive)
        unsubscribe()
      }
    },
    cancel(reason) {
      // @ts-expect-error — set in start()
      this._cleanup?.()
      void reason
    },
  })
  return new Response(stream, {
    headers: {
      "content-type": "text/event-stream",
      "cache-control": "no-cache, no-transform",
      connection: "keep-alive",
      "access-control-allow-origin": "*",
    },
  })
}

function parseList(value: FormDataEntryValue | string | null): string[] {
  if (typeof value !== "string" || !value) return []
  try {
    const parsed = JSON.parse(value)
    if (Array.isArray(parsed)) return parsed.filter((x): x is string => typeof x === "string")
  } catch {
    /* fall through to CSV */
  }
  return value.split(",").map((s) => s.trim()).filter(Boolean)
}

// ─── Router ──────────────────────────────────────────────────────────────────

export async function handleRequest(request: Request): Promise<Response> {
  const url = new URL(request.url)
  const path = url.pathname

  if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS_HEADERS })

  // Unauthenticated: liveness/readiness + the static format list.
  if (path === "/health" || path === "/v1/health") {
    const checks: Record<string, boolean> = { postgres: false, vectors: false }
    try {
      await prisma.$queryRaw`SELECT 1`
      checks.postgres = true
    } catch {
      /* reported as false */
    }
    try {
      checks.vectors = await kb("vectors").healthCheck()
    } catch {
      /* reported as false */
    }
    const ok = Object.values(checks).every(Boolean)
    return json({ ok, checks, version: process.env.KB_VERSION ?? "dev" }, ok ? 200 : 503)
  }

  if (path === "/v1/formats") {
    return json({ extensions: KB_ACCEPTED_EXTENSIONS, maxFileBytes: KB_MAX_FILE_BYTES })
  }

  const auth = await authenticate(request)
  if (!auth) return error("Unauthorized — supply a key via Authorization: Bearer or X-Api-Key", 401)

  try {
    if (path === "/v1/documents" && request.method === "POST") return await ingestDocument(request, auth)
    if (path === "/v1/documents" && request.method === "GET") return await listDocuments(request, auth)
    if (path === "/v1/search" && request.method === "POST") return await search(request, auth)
    if (path === "/v1/knowledge-bases") return await knowledgeBases(request, auth)
    if (path === "/v1/categories") return await categories(request, auth)
    if (path === "/v1/events" && request.method === "GET") return events(auth)

    const docRawMatch = path.match(/^\/v1\/documents\/([^/]+)\/raw$/)
    if (docRawMatch && request.method === "GET") return await getDocumentRaw(docRawMatch[1], auth)

    const intelligenceMatch = path.match(/^\/v1\/documents\/([^/]+)\/intelligence$/)
    if (intelligenceMatch && request.method === "GET") return await getDocumentIntelligence(intelligenceMatch[1], auth)

    const docMatch = path.match(/^\/v1\/documents\/([^/]+)$/)
    if (docMatch && request.method === "GET") return await getDocument(docMatch[1], auth)
    if (docMatch && request.method === "PATCH") return await patchDocument(request, docMatch[1], auth)
    if (docMatch && request.method === "DELETE") {
      const hard = url.searchParams.get("hard") === "true"
      return await deleteDocument(docMatch[1], auth, hard)
    }

    const jobRetryMatch = path.match(/^\/v1\/jobs\/([^/]+)\/retry$/)
    if (jobRetryMatch && request.method === "POST") return await retryJob(jobRetryMatch[1], auth)

    const jobMatch = path.match(/^\/v1\/jobs\/([^/]+)$/)
    if (jobMatch && request.method === "GET") return await getJob(jobMatch[1], auth)

    const kbMatch = path.match(/^\/v1\/knowledge-bases\/([^/]+)$/)
    if (kbMatch && (request.method === "PATCH" || request.method === "DELETE")) {
      return await knowledgeBaseById(request, kbMatch[1], auth)
    }

    const categoryMatch = path.match(/^\/v1\/categories\/([^/]+)$/)
    if (categoryMatch && (request.method === "PATCH" || request.method === "DELETE")) {
      return await categoryById(request, categoryMatch[1], auth)
    }

    return error("Not found", 404)
  } catch (err) {
    console.error(`[kb] ${request.method} ${path} failed:`, err)
    return error("Internal error", 500)
  }
}
