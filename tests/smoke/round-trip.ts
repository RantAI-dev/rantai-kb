#!/usr/bin/env bun
/**
 * End-to-end smoke test: boot the service, ingest a document, retrieve it.
 *
 * This is the test that would have caught every integration break during the
 * extraction — ports unbound, tenant scope missing, vector schema wrong,
 * worker not claiming. It runs against real Postgres + SurrealDB + MinIO in CI
 * with a stub embedding endpoint, so it needs no API credits.
 */
import { configureKb } from "../../src/lib/kb-runtime/runtime"
import { serviceKbRuntime } from "../../src/service/adapters"
import { handleRequest } from "../../src/service/api"
import { prisma } from "../../src/service/db"
import { generateApiKey } from "../../src/service/auth"

const DIM = Number(process.env.KB_EMBEDDING_DIM || 8)

// ── Stub embeddings: deterministic, no network, no credits ──────────────────
const embedServer = Bun.serve({
  port: 0,
  fetch: async (req) => {
    const body = (await req.json()) as { input?: unknown }
    const input = Array.isArray(body.input) ? body.input : [body.input]
    return Response.json({
      data: input.map((text, index) => ({
        object: "embedding",
        index,
        // Deterministic per text so the same query lands near the same chunk.
        embedding: Array.from({ length: DIM }, (_, i) => {
          const s = String(text)
          return ((s.charCodeAt(i % Math.max(s.length, 1)) || 1) % 100) / 100
        }),
      })),
      model: "stub",
      usage: { prompt_tokens: 1, total_tokens: 1 },
    })
  },
})
process.env.KB_EMBEDDING_BASE_URL = `http://127.0.0.1:${embedServer.port}/embeddings`
process.env.KB_EMBEDDING_API_KEY = "stub"

configureKb(serviceKbRuntime())

const TENANT = `smoke-${Date.now()}`
let failures = 0

function check(label: string, ok: boolean, detail?: unknown) {
  if (ok) {
    console.log(`  ✓ ${label}`)
  } else {
    failures++
    console.error(`  ✗ ${label}`, detail ?? "")
  }
}

async function call(path: string, init: RequestInit & { key?: string } = {}): Promise<Response> {
  const headers = new Headers(init.headers)
  if (init.key) headers.set("authorization", `Bearer ${init.key}`)
  return handleRequest(new Request(`http://kb.test${path}`, { ...init, headers }))
}

console.log("\nKB service smoke test\n")

// ── Health (unauthenticated) ───────────────────────────────────────────────
const health = await call("/health")
const healthBody = (await health.json()) as { ok: boolean; checks: Record<string, boolean> }
check("health reports postgres up", healthBody.checks.postgres === true, healthBody)
check("health reports vector store up", healthBody.checks.vectors === true, healthBody)

// ── Auth is mandatory ──────────────────────────────────────────────────────
check("search without a key is 401", (await call("/v1/search", { method: "POST" })).status === 401)

// ── Mint a key ─────────────────────────────────────────────────────────────
const { plaintext, hash } = generateApiKey()
await prisma.apiKey.create({ data: { tenantId: TENANT, name: "smoke", hash } })
check("api key minted", plaintext.startsWith("rkb_"))

// ── Ingest ─────────────────────────────────────────────────────────────────
const csv = "Kota,Pengunjung,Bulan\n" + Array.from({ length: 40 }, (_, i) => `Kota${i},${1000 + i * 37},2026-0${(i % 9) + 1}`).join("\n")
const form = new FormData()
form.set("file", new File([csv], "pengunjung.csv", { type: "text/csv" }))
form.set("title", "Data Pengunjung")

const ingest = await call("/v1/documents", { method: "POST", body: form, key: plaintext })
const ingestBody = (await ingest.json()) as { id: string; jobId: string }
check("ingest accepted with 202", ingest.status === 202, ingestBody)
check("ingest returned a job id", Boolean(ingestBody.jobId), ingestBody)

// ── Run the job (worker path, without the poll loop) ───────────────────────
const { claimNextPendingJob } = await import("../../src/lib/ingest/job")
const { processIngestJob } = await import("../../src/service/ingest-runner")
const job = await claimNextPendingJob()
check("worker claimed the job", job?.id === ingestBody.jobId, job)
const outcome = job ? await processIngestJob(job) : "failed"
check("ingest completed", outcome === "ready", outcome)

const jobStatus = (await (await call(`/v1/jobs/${ingestBody.jobId}`, { key: plaintext })).json()) as {
  status: string
  error: string | null
}
check("job row says success", jobStatus.status === "success", jobStatus)

// ── Retrieve ───────────────────────────────────────────────────────────────
const search = await call("/v1/search", {
  method: "POST",
  key: plaintext,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ query: "Kota pengunjung", maxChunks: 5 }),
})
const searchBody = (await search.json()) as { chunks?: Array<{ content: string }> }
check("search returns chunks", (searchBody.chunks?.length ?? 0) > 0, searchBody)
check(
  "chunk content survived ingest intact",
  Boolean(searchBody.chunks?.some((c) => c.content.includes("Kota0"))),
  searchBody.chunks?.[0]?.content?.slice(0, 120)
)

// ── Tenant isolation: another tenant's key must not see the document ───────
const other = generateApiKey()
await prisma.apiKey.create({ data: { tenantId: `${TENANT}-other`, name: "smoke-other", hash: other.hash } })
const crossList = (await (await call("/v1/documents", { key: other.plaintext })).json()) as {
  documents: unknown[]
}
check("a second tenant sees no documents", crossList.documents.length === 0, crossList)

const crossSearch = (await (
  await call("/v1/search", {
    method: "POST",
    key: other.plaintext,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query: "Kota pengunjung" }),
  })
).json()) as { chunks?: unknown[] }
check("a second tenant retrieves nothing", (crossSearch.chunks?.length ?? 0) === 0, crossSearch)

// ── Cleanup ────────────────────────────────────────────────────────────────
await call(`/v1/documents/${ingestBody.id}`, { method: "DELETE", key: plaintext })
await prisma.apiKey.deleteMany({ where: { tenantId: { startsWith: "smoke-" } } })
await prisma.document.deleteMany({ where: { tenantId: { startsWith: "smoke-" } } })
await prisma.ingestJob.deleteMany({ where: { tenantId: { startsWith: "smoke-" } } })

embedServer.stop()

console.log("")
if (failures > 0) {
  console.error(`${failures} check(s) failed\n`)
  process.exit(1)
}
console.log("all checks passed\n")
process.exit(0)
