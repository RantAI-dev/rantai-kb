import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { FakeTable } from "../../helpers/prisma-fake"

/** GET|POST /v1/categories, PATCH|DELETE /v1/categories/:id. */

type CategoryRow = { id: string; tenantId: string; name: string; label: string; color: string | null }

function authedRequest(path: string, init: RequestInit = {}): Request {
  return new Request(`http://localhost${path}`, {
    ...init,
    headers: { authorization: "Bearer rkb_test", ...(init.headers ?? {}) },
  })
}

async function loadHandler(category: FakeTable<CategoryRow>, tenantId = "tenant-a") {
  vi.doMock("@/service/db", () => ({ prisma: { category } }))
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

describe("GET|POST /v1/categories", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("GET only returns the caller's tenant's categories", async () => {
    const category = new FakeTable<CategoryRow>([
      { id: "cat-a", tenantId: "tenant-a", name: "policy", label: "Policy", color: "#fff" },
      { id: "cat-b", tenantId: "tenant-b", name: "policy", label: "Policy", color: "#000" },
    ])
    const handleRequest = await loadHandler(category, "tenant-a")
    const res = await handleRequest(authedRequest("/v1/categories"))
    expect(res.status).toBe(200)
    const body = await res.json()
    expect(body.categories).toEqual([{ id: "cat-a", name: "policy", label: "Policy", color: "#fff" }])
  })

  it("POST creates a category scoped to the caller's tenant", async () => {
    const category = new FakeTable<CategoryRow>([])
    const handleRequest = await loadHandler(category, "tenant-a")
    const res = await handleRequest(
      authedRequest("/v1/categories", {
        method: "POST",
        body: JSON.stringify({ name: "finance", label: "Finance" }),
        headers: { "content-type": "application/json" },
      })
    )
    expect(res.status).toBe(201)
    const body = await res.json()
    expect(body.category).toMatchObject({ name: "finance", label: "Finance" })
    expect(category.rows[0].tenantId).toBe("tenant-a")
  })
})

describe("PATCH|DELETE /v1/categories/:id", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("PATCH returns the same 404 for another tenant's category as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const category = new FakeTable<CategoryRow>([
      { id: "cat-owned", tenantId: "tenant-a", name: "policy", label: "Policy", color: null },
      { id: "cat-other", tenantId: "tenant-b", name: "policy", label: "Policy", color: null },
    ])
    const handleRequest = await loadHandler(category, "tenant-a")
    const body = JSON.stringify({ label: "Renamed" })

    const otherTenantRes = await handleRequest(
      authedRequest("/v1/categories/cat-other", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    const missingRes = await handleRequest(
      authedRequest("/v1/categories/does-not-exist", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    expect(category.rows.find((r) => r.id === "cat-other")!.label).toBe("Policy") // untouched

    const ownerRes = await handleRequest(
      authedRequest("/v1/categories/cat-owned", { method: "PATCH", body, headers: { "content-type": "application/json" } })
    )
    expect(ownerRes.status).toBe(200)
    expect((await ownerRes.json()).category.label).toBe("Renamed")
  })

  it("DELETE returns the same 404 for another tenant's category as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const category = new FakeTable<CategoryRow>([
      { id: "cat-owned", tenantId: "tenant-a", name: "policy", label: "Policy", color: null },
      { id: "cat-other", tenantId: "tenant-b", name: "policy", label: "Policy", color: null },
    ])
    const handleRequest = await loadHandler(category, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/categories/cat-other", { method: "DELETE" }))
    const missingRes = await handleRequest(authedRequest("/v1/categories/does-not-exist", { method: "DELETE" }))
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    expect(category.rows.some((r) => r.id === "cat-other")).toBe(true)

    const ownerRes = await handleRequest(authedRequest("/v1/categories/cat-owned", { method: "DELETE" }))
    expect(ownerRes.status).toBe(200)
    expect(await ownerRes.json()).toEqual({ ok: true })
    expect(category.rows.some((r) => r.id === "cat-owned")).toBe(false)
  })
})
