import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { configureKb } from "@/lib/kb-runtime/runtime"
import type { VectorQueryResult, VectorStore } from "@/lib/kb-runtime/ports"
import { FakeTable, fakeTransaction } from "../../helpers/prisma-fake"

/**
 * GET/PATCH/DELETE /v1/documents/:id + GET /v1/documents/:id/raw.
 *
 * Same pattern as document-intelligence-route.test.ts: `@/service/db` and
 * `@/service/auth` are mocked before `@/service/api` is imported so the real
 * PrismaClient never gets instantiated. Unlike that file, `FakeTable` here
 * actually evaluates the route's WHERE clause (see tests/helpers/prisma-fake.ts)
 * so "another tenant's document" and "the owner's document" are genuinely
 * different lookups, not two calls to the same canned mock — which is what
 * lets the tenancy tests fail if the route's `tenantId` filter is ever lost.
 */

type DocRow = {
  id: string
  tenantId: string
  title: string
  categories: string[]
  subcategory: string | null
  fileType: string | null
  fileSize: number | null
  mimeType: string | null
  status: string
  s3Key: string | null
  createdAt: Date
  updatedAt: Date
  deletedAt: Date | null
  groups: Array<{ knowledgeBaseId: string }>
}

function makeDoc(overrides: Partial<DocRow> = {}): DocRow {
  return {
    id: "doc-1",
    tenantId: "tenant-a",
    title: "Handbook",
    categories: ["policy"],
    subcategory: null,
    fileType: "pdf",
    fileSize: 1024,
    mimeType: "application/pdf",
    status: "ready",
    s3Key: "documents/tenant-a/doc-1/handbook.pdf",
    createdAt: new Date("2026-01-01"),
    updatedAt: new Date("2026-01-01"),
    deletedAt: null,
    groups: [],
    ...overrides,
  }
}

function asVectorQuery(
  fn: (sql: string, vars?: Record<string, unknown>) => Promise<VectorQueryResult<unknown>[]>
): VectorStore["query"] {
  return fn as VectorStore["query"]
}

function authedRequest(path: string, init: RequestInit = {}): Request {
  return new Request(`http://localhost${path}`, {
    ...init,
    headers: { authorization: "Bearer rkb_test", ...(init.headers ?? {}) },
  })
}

async function loadHandler(document: FakeTable<DocRow>, tenantId = "tenant-a", extraPrisma: Record<string, unknown> = {}) {
  vi.doMock("@/service/db", () => ({
    prisma: {
      document,
      $transaction: vi.fn(fakeTransaction),
      ...extraPrisma,
    },
  }))
  vi.doMock("@/service/auth", async () => {
    const actual = await vi.importActual<typeof import("@/service/auth")>("@/service/auth")
    return {
      ...actual,
      authenticate: vi.fn(async () => ({ tenantId, scopes: [] as string[], knowledgeBaseIds: [] as string[], keyId: "key-1" })),
    }
  })
  const { handleRequest } = await import("@/service/api")
  return handleRequest
}

/** Chunks returned already in `chunk_index` order — that ordering is done by
 *  the `ORDER BY chunk_index ASC` in the query SurrealDB actually runs (see
 *  fetchOrderedChunks in service/api.ts), not by any re-sort in the route, so
 *  the fake mimics what a real ordered result set looks like rather than
 *  reversing it and expecting the route to fix it up. */
function surrealChunks(surrealQuery: ReturnType<typeof vi.fn>) {
  return async (sql: string): Promise<VectorQueryResult<unknown>[]> => {
    if (sql.includes("FROM document_chunk")) {
      expect(sql).toContain("ORDER BY chunk_index ASC")
      return [
        {
          result: [
            { id: "doc-1_0", content: "first", chunk_index: 0, metadata: { section: "A", chunkType: "text", page: 1 } },
            { id: "doc-1_1", content: "second", chunk_index: 1, metadata: { section: "B", chunkType: "text", page: 2 } },
          ],
        },
      ]
    }
    return surrealQuery(sql)
  }
}

describe("GET /v1/documents/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's document as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({ id: "doc-owned", tenantId: "tenant-a" }),
      makeDoc({ id: "doc-other", tenantId: "tenant-b" }),
    ])
    const surrealQuery = vi.fn(async (): Promise<VectorQueryResult<unknown>[]> => [{ result: [] }])
    configureKb({
      vectors: {
        query: asVectorQuery(surrealChunks(surrealQuery)),
        relate: vi.fn(async () => {}),
        cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })),
        healthCheck: vi.fn(async () => true),
      },
    })
    const handleRequest = await loadHandler(document, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/documents/doc-other"))
    const missingRes = await handleRequest(authedRequest("/v1/documents/does-not-exist"))
    const ownerRes = await handleRequest(authedRequest("/v1/documents/doc-owned"))

    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    const [otherBody, missingBody] = await Promise.all([otherTenantRes.json(), missingRes.json()])
    expect(otherBody).toEqual({ error: "Not found" })
    expect(otherBody).toEqual(missingBody)

    // The owner control: same key, its own document, must succeed.
    expect(ownerRes.status).toBe(200)
    const ownerBody = await ownerRes.json()
    expect(ownerBody.document.id).toBe("doc-owned")
    // chunks ordered by chunkIndex regardless of the order SurrealDB handed back
    expect(ownerBody.chunks.map((c: { chunkIndex: number }) => c.chunkIndex)).toEqual([0, 1])
    expect(ownerBody.document.chunkCount).toBe(2)
  })

  it("returns 404 for a soft-deleted document even for its owner", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", deletedAt: new Date("2026-02-01") })])
    configureKb({
      vectors: {
        query: asVectorQuery(async () => [{ result: [] }]),
        relate: vi.fn(async () => {}),
        cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })),
        healthCheck: vi.fn(async () => true),
      },
    })
    const handleRequest = await loadHandler(document, "tenant-a")
    const res = await handleRequest(authedRequest("/v1/documents/doc-1"))
    expect(res.status).toBe(404)
  })
})

describe("PATCH /v1/documents/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's document as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({ id: "doc-owned", tenantId: "tenant-a" }),
      makeDoc({ id: "doc-other", tenantId: "tenant-b" }),
    ])
    const knowledgeBase = new FakeTable<{ id: string; tenantId: string }>([])
    const documentGroup = new FakeTable<{ id: string; documentId: string; knowledgeBaseId: string }>([])
    const handleRequest = await loadHandler(document, "tenant-a", { knowledgeBase, documentGroup })

    const body = JSON.stringify({ title: "New title" })
    const otherTenantRes = await handleRequest(
      authedRequest("/v1/documents/doc-other", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    const missingRes = await handleRequest(
      authedRequest("/v1/documents/does-not-exist", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    const ownerRes = await handleRequest(
      authedRequest("/v1/documents/doc-owned", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )

    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())

    expect(ownerRes.status).toBe(200)
    const ownerBody = await ownerRes.json()
    expect(ownerBody.document.title).toBe("New title")
    // The other tenant's document must be untouched.
    expect(document.rows.find((r) => r.id === "doc-other")!.title).not.toBe("New title")
  })

  it("knowledgeBaseIds: absent leaves links untouched, [] clears them, a list replaces them (dropping ids the tenant doesn't own)", async () => {
    const documentGroup = new FakeTable<{ id: string; documentId: string; knowledgeBaseId: string }>([
      { id: "g1", documentId: "doc-1", knowledgeBaseId: "kb-mine" },
    ])
    const doc = makeDoc({ id: "doc-1", tenantId: "tenant-a" })
    // `groups` on a real Prisma read is a live relational join against
    // DocumentGroup; the fake has no joins, so wire it to the same backing
    // table PATCH writes through, instead of a snapshot that would go stale
    // the moment the route mutates documentGroup.
    Object.defineProperty(doc, "groups", {
      get: () => documentGroup.rows.filter((g) => g.documentId === doc.id).map((g) => ({ knowledgeBaseId: g.knowledgeBaseId })),
      enumerable: true,
    })
    const document = new FakeTable<DocRow>([doc])
    const knowledgeBase = new FakeTable<{ id: string; tenantId: string }>([
      { id: "kb-mine", tenantId: "tenant-a" },
      { id: "kb-theirs", tenantId: "tenant-b" },
    ])
    const handleRequest = await loadHandler(document, "tenant-a", { knowledgeBase, documentGroup })

    // absent -> untouched
    const untouchedRes = await handleRequest(
      authedRequest("/v1/documents/doc-1", {
        method: "PATCH",
        body: JSON.stringify({ title: "Renamed" }),
        headers: { "content-type": "application/json" },
      })
    )
    expect((await untouchedRes.json()).document.knowledgeBaseIds).toEqual(["kb-mine"])

    // list with a foreign id -> only the owned id is linked
    const replaceRes = await handleRequest(
      authedRequest("/v1/documents/doc-1", {
        method: "PATCH",
        body: JSON.stringify({ knowledgeBaseIds: ["kb-mine", "kb-theirs"] }),
        headers: { "content-type": "application/json" },
      })
    )
    expect((await replaceRes.json()).document.knowledgeBaseIds).toEqual(["kb-mine"])

    // [] -> cleared
    const clearRes = await handleRequest(
      authedRequest("/v1/documents/doc-1", {
        method: "PATCH",
        body: JSON.stringify({ knowledgeBaseIds: [] }),
        headers: { "content-type": "application/json" },
      })
    )
    expect((await clearRes.json()).document.knowledgeBaseIds).toEqual([])
  })
})

describe("DELETE /v1/documents/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("soft-deletes by default, leaving the row but flipping deletedAt — and the document then disappears from GET list and GET detail", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({ id: "doc-1", tenantId: "tenant-a" }),
      makeDoc({ id: "doc-other", tenantId: "tenant-b" }),
    ])
    configureKb({
      vectors: {
        query: asVectorQuery(async () => [{ result: [] }]),
        relate: vi.fn(async () => {}),
        cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })),
        healthCheck: vi.fn(async () => true),
      },
      blob: {
        upload: vi.fn(async () => ({ size: 0 })),
        download: vi.fn(async () => Buffer.from("")),
        delete: vi.fn(async () => {}),
        documentPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/${f}`,
        assetPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/assets/${f}`,
      },
    })
    const handleRequest = await loadHandler(document, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/documents/doc-other", { method: "DELETE" }))
    expect(otherTenantRes.status).toBe(404)
    expect(document.rows.find((r) => r.id === "doc-other")!.deletedAt).toBeNull() // cross-tenant delete must not touch the row

    const res = await handleRequest(authedRequest("/v1/documents/doc-1", { method: "DELETE" }))
    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({ ok: true })

    const row = document.rows.find((r) => r.id === "doc-1")!
    expect(row.deletedAt).not.toBeNull() // row still exists — soft delete, not hard
    expect(document.rows.length).toBe(2) // nothing actually removed

    const listRes = await handleRequest(authedRequest("/v1/documents"))
    const listBody = await listRes.json()
    expect(listBody.documents.map((d: { id: string }) => d.id)).not.toContain("doc-1")

    const detailRes = await handleRequest(authedRequest("/v1/documents/doc-1"))
    expect(detailRes.status).toBe(404)
  })

  it("?hard=true removes the row, its chunks and its stored file", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a", s3Key: "documents/tenant-a/doc-1/f.pdf" })])
    const deleteChunksQuery = vi.fn(async () => [{ result: [] }])
    const blobDelete = vi.fn(async () => {})
    configureKb({
      vectors: {
        query: asVectorQuery(deleteChunksQuery),
        relate: vi.fn(async () => {}),
        cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })),
        healthCheck: vi.fn(async () => true),
      },
      blob: {
        upload: vi.fn(async () => ({ size: 0 })),
        download: vi.fn(async () => Buffer.from("")),
        delete: blobDelete,
        documentPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/${f}`,
        assetPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/assets/${f}`,
      },
    })
    const handleRequest = await loadHandler(document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/documents/doc-1?hard=true", { method: "DELETE" }))
    expect(res.status).toBe(200)
    expect(document.rows.find((r) => r.id === "doc-1")).toBeUndefined()
    expect(blobDelete).toHaveBeenCalledWith("documents/tenant-a/doc-1/f.pdf")
  })
})

describe("GET /v1/documents/:id/raw", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's document as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({ id: "doc-owned", tenantId: "tenant-a", mimeType: "application/pdf", s3Key: "documents/tenant-a/doc-owned/f.pdf" }),
      makeDoc({ id: "doc-other", tenantId: "tenant-b" }),
    ])
    const download = vi.fn(async () => Buffer.from("%PDF-1.4 fake bytes"))
    configureKb({
      blob: {
        upload: vi.fn(async () => ({ size: 0 })),
        download,
        delete: vi.fn(async () => {}),
        documentPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/${f}`,
        assetPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/assets/${f}`,
      },
    })
    const handleRequest = await loadHandler(document, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/documents/doc-other/raw"))
    const missingRes = await handleRequest(authedRequest("/v1/documents/does-not-exist/raw"))
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    expect(download).not.toHaveBeenCalled() // ownership must resolve before the blob store is ever touched

    const ownerRes = await handleRequest(authedRequest("/v1/documents/doc-owned/raw"))
    expect(ownerRes.status).toBe(200)
    expect(ownerRes.headers.get("content-type")).toBe("application/pdf")
    const bytes = Buffer.from(await ownerRes.arrayBuffer())
    expect(bytes.toString()).toBe("%PDF-1.4 fake bytes")
  })
})
