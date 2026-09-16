import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { FakeTable, fakeTransaction } from "../../helpers/prisma-fake"

/**
 * PATCH/DELETE /v1/knowledge-bases/:id.
 *
 * The DELETE contract is specific: it removes the base and its DocumentGroup
 * links, and MUST NOT touch the Document rows themselves (Wave 8 table:
 * "removes the base and its links, never documents"). Prisma expresses this
 * as `onDelete: Cascade` on DocumentGroup -> KnowledgeBase in the schema; the
 * fake has no real FK cascade, so the route's own deleteMany/delete calls are
 * what the "documents survive" test is actually exercising.
 */

type KbRow = { id: string; tenantId: string; name: string; description: string | null; color: string | null }
type DocRow = { id: string; tenantId: string; title: string }
type GroupRow = { id: string; documentId: string; knowledgeBaseId: string }

function authedRequest(path: string, init: RequestInit = {}): Request {
  return new Request(`http://localhost${path}`, {
    ...init,
    headers: { authorization: "Bearer rkb_test", ...(init.headers ?? {}) },
  })
}

async function loadHandler(
  knowledgeBase: FakeTable<KbRow>,
  document: FakeTable<DocRow>,
  documentGroup: FakeTable<GroupRow>,
  tenantId = "tenant-a"
) {
  vi.doMock("@/service/db", () => ({
    prisma: { knowledgeBase, document, documentGroup, $transaction: vi.fn(fakeTransaction) },
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

describe("PATCH /v1/knowledge-bases/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's base as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const knowledgeBase = new FakeTable<KbRow>([
      { id: "kb-owned", tenantId: "tenant-a", name: "Mine", description: null, color: null },
      { id: "kb-other", tenantId: "tenant-b", name: "Theirs", description: null, color: null },
    ])
    const document = new FakeTable<DocRow>([])
    const documentGroup = new FakeTable<GroupRow>([])
    const handleRequest = await loadHandler(knowledgeBase, document, documentGroup, "tenant-a")

    const body = JSON.stringify({ name: "Renamed" })
    const otherTenantRes = await handleRequest(
      authedRequest("/v1/knowledge-bases/kb-other", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    const missingRes = await handleRequest(
      authedRequest("/v1/knowledge-bases/does-not-exist", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    expect(knowledgeBase.rows.find((r) => r.id === "kb-other")!.name).toBe("Theirs") // untouched

    const ownerRes = await handleRequest(
      authedRequest("/v1/knowledge-bases/kb-owned", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    expect(ownerRes.status).toBe(200)
    expect((await ownerRes.json()).knowledgeBase.name).toBe("Renamed")
  })
})

describe("DELETE /v1/knowledge-bases/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's base as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const knowledgeBase = new FakeTable<KbRow>([
      { id: "kb-owned", tenantId: "tenant-a", name: "Mine", description: null, color: null },
      { id: "kb-other", tenantId: "tenant-b", name: "Theirs", description: null, color: null },
    ])
    const document = new FakeTable<DocRow>([])
    const documentGroup = new FakeTable<GroupRow>([])
    const handleRequest = await loadHandler(knowledgeBase, document, documentGroup, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/knowledge-bases/kb-other", { method: "DELETE" }))
    const missingRes = await handleRequest(authedRequest("/v1/knowledge-bases/does-not-exist", { method: "DELETE" }))
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    expect(knowledgeBase.rows.some((r) => r.id === "kb-other")).toBe(true) // must survive the cross-tenant attempt

    const ownerRes = await handleRequest(authedRequest("/v1/knowledge-bases/kb-owned", { method: "DELETE" }))
    expect(ownerRes.status).toBe(200)
    expect(await ownerRes.json()).toEqual({ ok: true })
    expect(knowledgeBase.rows.some((r) => r.id === "kb-owned")).toBe(false)
  })

  it("removes the base and its document links, but leaves the documents themselves intact", async () => {
    const knowledgeBase = new FakeTable<KbRow>([{ id: "kb-1", tenantId: "tenant-a", name: "Handbooks", description: null, color: null }])
    const document = new FakeTable<DocRow>([{ id: "doc-1", tenantId: "tenant-a", title: "Handbook" }])
    const documentGroup = new FakeTable<GroupRow>([{ id: "g1", documentId: "doc-1", knowledgeBaseId: "kb-1" }])
    const handleRequest = await loadHandler(knowledgeBase, document, documentGroup, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/knowledge-bases/kb-1", { method: "DELETE" }))
    expect(res.status).toBe(200)

    expect(knowledgeBase.rows.length).toBe(0)
    expect(documentGroup.rows.length).toBe(0) // the link is gone
    // The document row must still exist, byte-for-byte untouched.
    expect(document.rows).toEqual([{ id: "doc-1", tenantId: "tenant-a", title: "Handbook" }])
  })
})
