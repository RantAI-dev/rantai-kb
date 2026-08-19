import { configureKb } from "@/lib/kb-runtime/runtime"
import { serviceKbRuntime } from "./service/adapters"
import { handleRequest } from "./service/api"

/**
 * RantAI KB service.
 *
 * One process serves the HTTP API and runs the ingest worker. That is
 * deliberate for the first release: the worker claims jobs with
 * `FOR UPDATE SKIP LOCKED`, so scaling out is just running more replicas —
 * no separate worker deployment, no queue broker.
 */

// Composition root: bind the engine's ports before anything can use them.
configureKb(serviceKbRuntime())

const PORT = Number(process.env.PORT || 8080)

const server = Bun.serve({
  port: PORT,
  idleTimeout: 255, // long uploads + SSE
  fetch: handleRequest,
})

console.log(`[kb] listening on http://localhost:${server.port}`)

if (process.env.KB_WORKER_ENABLED !== "false") {
  const { startIngestWorker } = await import("@/lib/ingest/worker")
  startIngestWorker()
} else {
  console.log("[kb] worker disabled (KB_WORKER_ENABLED=false) — API only")
}

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    console.log(`[kb] ${signal} — shutting down`)
    server.stop()
    process.exit(0)
  })
}
