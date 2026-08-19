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

/** DELETE /v1/documents/:id */
async function deleteDocument(id: string, auth: AuthContext): Promise<Response> {
  if (!hasScope(auth, "kb:write")) return error("API key lacks scope kb:write", 403)
  return withTenant(auth.tenantId, async () => {
    const doc = await prisma.document.findFirst({ where: { id, tenantId: auth.tenantId } })
    if (!doc) return error("Not found", 404)

    const { deleteChunksByDocumentId } = await import("@/lib/rag")
    await deleteChunksByDocumentId(id).catch((err) => console.warn("[kb] chunk delete failed:", err))
    if (doc.s3Key) await kb("blob").delete(doc.s3Key).catch(() => {})
    await prisma.document.delete({ where: { id } })
    return json({ ok: true })
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
    if (path === "/v1/events" && request.method === "GET") return events(auth)

    const docMatch = path.match(/^\/v1\/documents\/([^/]+)$/)
    if (docMatch && request.method === "DELETE") return await deleteDocument(docMatch[1], auth)

    const jobMatch = path.match(/^\/v1\/jobs\/([^/]+)$/)
    if (jobMatch && request.method === "GET") return await getJob(jobMatch[1], auth)

    return error("Not found", 404)
  } catch (err) {
    console.error(`[kb] ${request.method} ${path} failed:`, err)
    return error("Internal error", 500)
  }
}
