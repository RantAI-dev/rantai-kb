import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { configureKb } from "@/lib/kb-runtime/runtime"
import type { VectorQueryResult, VectorStore } from "@/lib/kb-runtime/ports"
import { FakeTable, fakeTransaction } from "../../helpers/prisma-fake"

/**
 * GET /v1/documents/:id/assets?key=<assetKey> — streams a figure crop through
 * the service.
 *
 * The security property under test: an assetKey is only ever served when it
 * (a) belongs to a document this tenant owns, resolved through Postgres
 * BEFORE the blob store is ever touched (same pattern as getDocumentRaw /
 * getDocumentIntelligence in src/service/api.ts), AND (b) sits inside THAT
 * document's own asset namespace (documents/{tenantId}/{id}/assets/…) — never
 * another tenant's, never a sibling document's, never reached via a ".."
 * escape. Every failure mode returns the identical `{"error":"Not found"}`
 * 404 a nonexistent id gets — never 403, never a distinguishable message, so
 * a caller can't use the response shape to probe for valid keys.
 *
 * Also covers GET /v1/documents/:id gaining a `figures` field read from
 * Document.metadata.figures (see src/lib/rag/figure-assets.ts's FigureAsset
 * and index-document.ts's storeFiguresAsChunks write path).
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
  metadata: unknown
  groups: Array<{ knowledgeBaseId: string }>
}

function makeDoc(overrides: Partial<DocRow> = {}): DocRow {
  return {
    id: "doc-1",
    tenantId: "tenant-a",
    title: "Handbook",
    categories: [],
    subcategory: null,
    fileType: "pdf",
    fileSize: 1024,
    mimeType: "application/pdf",
    status: "ready",
    s3Key: "documents/tenant-a/doc-1/handbook.pdf",
    createdAt: new Date("2026-01-01"),
    updatedAt: new Date("2026-01-01"),
    deletedAt: null,
    metadata: null,
    groups: [],
    ...overrides,
  }
}

function asVectorQuery(
  fn: (sql: string, vars?: Record<string, unknown>) => Promise<VectorQueryResult<unknown>[]>
): VectorStore["query"] {
  return fn as VectorStore["query"]
}

function authedRequest(path: string): Request {
  return new Request(`http://localhost${path}`, {
    headers: { authorization: "Bearer rkb_test" },
  })
}

async function loadHandler(document: FakeTable<DocRow>, tenantId = "tenant-a") {
  vi.doMock("@/service/db", () => ({
    prisma: { document, $transaction: vi.fn(fakeTransaction) },
  }))
  vi.doMock("@/service/auth", async () => {
    const actual = await vi.importActual<typeof import("@/service/auth")>("@/service/auth")
    return {
      ...actual,
      authenticate: vi.fn(async () => ({
        tenantId,
        scopes: [] as string[],
        knowledgeBaseIds: [] as string[],
        keyId: "key-1",
      })),
    }
  })
  const { handleRequest } = await import("@/service/api")
  return handleRequest
}

function configureBlob(download: ReturnType<typeof vi.fn>) {
  configureKb({
    blob: {
      upload: vi.fn(async () => ({ size: 0 })),
      download,
      delete: vi.fn(async () => {}),
      documentPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/${f}`,
      assetPath: (org: string | null, doc: string, f: string) => `documents/${org}/${doc}/assets/${f}`,
    },
  })
}

describe("GET /v1/documents/:id/assets", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("owner gets 200 with the right bytes and content type (positive control)", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a" })])
    const download = vi.fn(async () => Buffer.from("fake-png-bytes"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const key = "documents/tenant-a/doc-1/assets/fig-p1-1.png"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))

    expect(res.status).toBe(200)
    expect(res.headers.get("content-type")).toBe("image/png")
    const bytes = Buffer.from(await res.arrayBuffer())
    expect(bytes.toString()).toBe("fake-png-bytes")
    expect(download).toHaveBeenCalledWith(key)
  })

  it("another tenant's document -> 404, blob store never touched", async () => {
    // The document exists, but is owned by tenant-b — auth here is tenant-a.
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-b" })])
    const download = vi.fn(async () => Buffer.from("x"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const key = "documents/tenant-b/doc-1/assets/fig-p1-1.png"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
    expect(download).not.toHaveBeenCalled()
  })

  it("a document id owned by another tenant, probed with a key forged under the caller's own tenant prefix -> 404 (isolates the ownership check from the prefix check)", async () => {
    // This is the case the prefix check alone CANNOT catch: the key is
    // syntactically valid for tenant-a's own namespace (same auth.tenantId,
    // same :id path param) — only the Postgres ownership lookup knows doc-1
    // actually belongs to tenant-b. Without that lookup scoped by tenantId,
    // this request would sail through the prefix check and reach the blob
    // store. `download` here answers ANY key (it doesn't validate that a
    // real object exists at it) specifically so this test can tell the two
    // checks apart — a real S3 bucket would separately 404 on the forged key
    // too, but this endpoint must not depend on that as its only defense.
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-b" })])
    const download = vi.fn(async () => Buffer.from("leaked"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const key = "documents/tenant-a/doc-1/assets/fig-p1-1.png"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
    expect(download).not.toHaveBeenCalled()
  })

  it("a key for a different document of the same tenant -> 404, blob store never touched", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({ id: "doc-1", tenantId: "tenant-a" }),
      makeDoc({ id: "doc-2", tenantId: "tenant-a" }),
    ])
    const download = vi.fn(async () => Buffer.from("x"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    // doc-1 is owned and exists, but the key points into doc-2's namespace.
    const key = "documents/tenant-a/doc-2/assets/fig-p1-1.png"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
    expect(download).not.toHaveBeenCalled()
  })

  it("a key containing '..' -> 404, even though it string-prefix-matches the document's namespace", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a" })])
    const download = vi.fn(async () => Buffer.from("x"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    // This literally starts with "documents/tenant-a/doc-1/assets/" as a
    // string (startsWith is not path resolution) — the ".." check is the
    // only thing that catches it.
    const key = "documents/tenant-a/doc-1/assets/../../../etc/passwd"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
    expect(download).not.toHaveBeenCalled()
  })

  it("a nonexistent document id gets the identical 404 body a cross-tenant document gets", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-b" })])
    const download = vi.fn(async () => Buffer.from("x"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const crossTenantRes = await handleRequest(
      authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent("documents/tenant-b/doc-1/assets/f.png")}`)
    )
    const missingRes = await handleRequest(
      authedRequest(
        `/v1/documents/does-not-exist/assets?key=${encodeURIComponent("documents/tenant-a/does-not-exist/assets/f.png")}`
      )
    )
    expect(crossTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    const [crossTenantBody, missingBody] = await Promise.all([crossTenantRes.json(), missingRes.json()])
    expect(crossTenantBody).toEqual({ error: "Not found" })
    expect(crossTenantBody).toEqual(missingBody)
  })

  it("a missing object in the blob store -> the same 404", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a" })])
    const download = vi.fn(async () => {
      throw new Error("NoSuchKey")
    })
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const key = "documents/tenant-a/doc-1/assets/missing.png"
    const res = await handleRequest(authedRequest(`/v1/documents/doc-1/assets?key=${encodeURIComponent(key)}`))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
  })

  it("no key at all -> 404", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a" })])
    const download = vi.fn(async () => Buffer.from("x"))
    configureBlob(download)
    const handleRequest = await loadHandler(document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/documents/doc-1/assets"))
    expect(res.status).toBe(404)
    expect(await res.json()).toEqual({ error: "Not found" })
    expect(download).not.toHaveBeenCalled()
  })
})

describe("GET /v1/documents/:id — figures", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  function configureEmptyChunks() {
    configureKb({
      vectors: {
        query: asVectorQuery(async (): Promise<VectorQueryResult<unknown>[]> => [{ result: [] }]),
        relate: vi.fn(async () => {}),
        cleanupDocumentIntelligence: vi.fn(async () => ({
          deletedRelationTables: 0,
          entitiesDeleted: false,
          chunksDeleted: false,
        })),
        healthCheck: vi.fn(async () => true),
      },
    })
  }

  it("returns figures read from Document.metadata.figures", async () => {
    const document = new FakeTable<DocRow>([
      makeDoc({
        id: "doc-1",
        tenantId: "tenant-a",
        metadata: {
          figures: [
            {
              assetKey: "documents/tenant-a/doc-1/assets/fig-p1-1.png",
              page: 1,
              caption: "Gambar 1: Kincir angin",
              bbox: [0, 0, 1, 1],
              type: "image",
            },
          ],
        },
      }),
    ])
    configureEmptyChunks()
    const handleRequest = await loadHandler(document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/documents/doc-1"))
    expect(res.status).toBe(200)
    const body = await res.json()
    expect(body.document.figures).toEqual([
      {
        assetKey: "documents/tenant-a/doc-1/assets/fig-p1-1.png",
        page: 1,
        caption: "Gambar 1: Kincir angin",
        kind: "image",
      },
    ])
  })

  it("returns an empty array when the document has no figures", async () => {
    const document = new FakeTable<DocRow>([makeDoc({ id: "doc-1", tenantId: "tenant-a", metadata: null })])
    configureEmptyChunks()
    const handleRequest = await loadHandler(document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/documents/doc-1"))
    expect(res.status).toBe(200)
    const body = await res.json()
    expect(body.document.figures).toEqual([])
  })
})
