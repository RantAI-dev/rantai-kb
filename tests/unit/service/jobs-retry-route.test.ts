import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { FakeTable, fakeTransaction } from "../../helpers/prisma-fake"

/** POST /v1/jobs/:id/retry — failed jobs only (409 otherwise), tenant-scoped. */

type JobRow = {
  id: string
  tenantId: string
  documentId: string | null
  status: string
  error: string | null
  attempt: number
  step: string | null
  progress: number
  stepCurrent: number | null
  stepTotal: number | null
  etaSeconds: number | null
  startedAt: Date | null
  s3Key: string | null
  filename: string
}

type DocRow = { id: string; tenantId: string; status: string }

function makeJob(overrides: Partial<JobRow> = {}): JobRow {
  return {
    id: "job-1",
    tenantId: "tenant-a",
    documentId: "doc-1",
    status: "failed",
    error: "boom",
    attempt: 1,
    step: null,
    progress: 0,
    stepCurrent: null,
    stepTotal: null,
    etaSeconds: null,
    startedAt: null,
    s3Key: "documents/tenant-a/doc-1/f.pdf",
    filename: "f.pdf",
    ...overrides,
  }
}

function authedRequest(path: string): Request {
  return new Request(`http://localhost${path}`, { method: "POST", headers: { authorization: "Bearer rkb_test" } })
}

async function loadHandler(ingestJob: FakeTable<JobRow>, document: FakeTable<DocRow>, tenantId = "tenant-a") {
  vi.doMock("@/service/db", () => ({ prisma: { ingestJob, document, $transaction: vi.fn(fakeTransaction) } }))
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

describe("POST /v1/jobs/:id/retry", () => {
  beforeEach(() => vi.resetModules())
  afterEach(() => vi.restoreAllMocks())

  it("returns the same 404 for another tenant's job as for a nonexistent one, paired with an owner-gets-200 control", async () => {
    const ingestJob = new FakeTable<JobRow>([
      makeJob({ id: "job-owned", tenantId: "tenant-a" }),
      makeJob({ id: "job-other", tenantId: "tenant-b" }),
    ])
    const document = new FakeTable<DocRow>([{ id: "doc-1", tenantId: "tenant-a", status: "failed" }])
    const handleRequest = await loadHandler(ingestJob, document, "tenant-a")

    const otherTenantRes = await handleRequest(authedRequest("/v1/jobs/job-other/retry"))
    const missingRes = await handleRequest(authedRequest("/v1/jobs/does-not-exist/retry"))
    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    expect(await otherTenantRes.json()).toEqual(await missingRes.json())
    // must not have touched the other tenant's row
    expect(ingestJob.rows.find((j) => j.id === "job-other")!.status).toBe("failed")

    const ownerRes = await handleRequest(authedRequest("/v1/jobs/job-owned/retry"))
    expect(ownerRes.status).toBe(200)
    expect(await ownerRes.json()).toEqual({ jobId: "job-owned" })
    const row = ingestJob.rows.find((j) => j.id === "job-owned")!
    expect(row.status).toBe("pending")
    expect(row.attempt).toBe(2)
    expect(row.error).toBeNull()
  })

  it("refuses to retry a job that is not failed", async () => {
    const ingestJob = new FakeTable<JobRow>([makeJob({ id: "job-1", status: "processing" })])
    const document = new FakeTable<DocRow>([{ id: "doc-1", tenantId: "tenant-a", status: "processing" }])
    const handleRequest = await loadHandler(ingestJob, document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/jobs/job-1/retry"))
    expect(res.status).toBe(409)
    expect(ingestJob.rows[0].status).toBe("processing") // untouched
  })

  it("a successfully completed job also cannot be retried (409, not 200)", async () => {
    const ingestJob = new FakeTable<JobRow>([makeJob({ id: "job-1", status: "success" })])
    const document = new FakeTable<DocRow>([{ id: "doc-1", tenantId: "tenant-a", status: "ready" }])
    const handleRequest = await loadHandler(ingestJob, document, "tenant-a")

    const res = await handleRequest(authedRequest("/v1/jobs/job-1/retry"))
    expect(res.status).toBe(409)
  })
})
