import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { configureKb } from "@/lib/kb-runtime/runtime"
import type { VectorQueryResult, VectorStore } from "@/lib/kb-runtime/ports"

// vi.fn()'s inferred signature is non-generic (it returns VectorQueryResult<unknown>[]
// no matter what T a caller asks for), which is exactly what every fake here
// does — cast once at the call site instead of fighting the generic in each test.
function asVectorQuery(fn: (sql: string, vars?: Record<string, unknown>) => Promise<VectorQueryResult<unknown>[]>): VectorStore["query"] {
  return fn as VectorStore["query"]
}

/**
 * GET /v1/documents/:id/intelligence
 *
 * The two cases the security review called out explicitly:
 *  1. a document that exists, but belongs to a different tenant, must 404
 *     with EXACTLY the body a nonexistent id gets — never a distinguishable
 *     response, or a caller can use this endpoint to enumerate other
 *     tenants' document ids.
 *  2. a document with no extracted graph is a real (200, empty-arrays)
 *     answer, not a 404 and not a failure.
 *
 * `src/service/api.ts` is imported dynamically, after mocking `./db` and
 * `./auth`, following the pattern in tests/unit/rag/store-chunks-*.test.ts —
 * the real PrismaClient must never be instantiated in a unit test.
 */

function authRequest(path: string): Request {
  return new Request(`http://localhost${path}`, {
    method: "GET",
    headers: { authorization: "Bearer rkb_test" },
  })
}

async function loadHandler(findFirst: ReturnType<typeof vi.fn>, tenantId = "tenant-a") {
  vi.doMock("@/service/db", () => ({ prisma: { document: { findFirst } } }))
  vi.doMock("@/service/auth", async () => {
    const actual = await vi.importActual<typeof import("@/service/auth")>("@/service/auth")
    return {
      ...actual,
      authenticate: vi.fn(async () => ({
        tenantId,
        scopes: [] as string[], // empty = full access, matches hasScope's "no scopes configured" case
        knowledgeBaseIds: [] as string[],
        keyId: "key-1",
      })),
    }
  })
  const { handleRequest } = await import("@/service/api")
  return handleRequest
}

describe("GET /v1/documents/:id/intelligence", () => {
  beforeEach(() => {
    vi.resetModules()
  })
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("returns the same 404 for another tenant's document as for a nonexistent one", async () => {
    // The route's Postgres lookup is scoped by tenantId in the WHERE clause,
    // so from the fake's point of view "wrong tenant" and "doesn't exist"
    // are indistinguishable — findFirst returns null either way. That IS the
    // property under test: the endpoint cannot tell them apart either, so it
    // cannot leak which one happened.
    const findFirst = vi.fn(async () => null)
    const surrealQuery = vi.fn()
    configureKb({ vectors: { query: asVectorQuery(surrealQuery), relate: vi.fn(async () => {}), cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })), healthCheck: vi.fn(async () => true) } })
    const handleRequest = await loadHandler(findFirst)

    const otherTenantRes = await handleRequest(authRequest("/v1/documents/belongs-to-tenant-b/intelligence"))
    const missingRes = await handleRequest(authRequest("/v1/documents/does-not-exist/intelligence"))

    expect(otherTenantRes.status).toBe(404)
    expect(missingRes.status).toBe(404)
    const [otherBody, missingBody] = await Promise.all([otherTenantRes.json(), missingRes.json()])
    expect(otherBody).toEqual({ error: "Not found" })
    expect(otherBody).toEqual(missingBody)

    // The graph store must never be touched before ownership is established.
    expect(surrealQuery).not.toHaveBeenCalled()
  })

  it("returns 200 with empty arrays for a document with no extracted graph", async () => {
    const findFirst = vi.fn(async () => ({ id: "doc-1", status: "ready" }))
    const surrealQuery = vi.fn(async (sql: string): Promise<VectorQueryResult<unknown>[]> => {
      if (sql.includes("FROM entity")) return [{ result: [] }]
      if (sql.includes("INFO FOR DB")) return [{ result: [{ tables: { entity: "...", document_chunk: "..." } }] }]
      return [{ result: [] }]
    })
    configureKb({ vectors: { query: asVectorQuery(surrealQuery), relate: vi.fn(async () => {}), cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })), healthCheck: vi.fn(async () => true) } })
    const handleRequest = await loadHandler(findFirst)

    const res = await handleRequest(authRequest("/v1/documents/doc-1/intelligence"))
    expect(res.status).toBe(200)
    expect(await res.json()).toEqual({
      entities: [],
      relations: [],
      status: "completed",
      stats: { totalEntities: 0, totalRelations: 0, entityTypes: 0, relationTypes: 0 },
    })
  })

  it("maps entities and relations from SurrealDB into the documented shape", async () => {
    const findFirst = vi.fn(async () => ({ id: "doc-1", status: "ready" }))
    const surrealQuery = vi.fn(async (sql: string): Promise<VectorQueryResult<unknown>[]> => {
      if (sql.includes("FROM entity")) {
        return [
          {
            result: [
              {
                id: "entity:doc-1_acme",
                name: "Acme",
                type: "Organization",
                confidence: 0.9,
                metadata: { pattern: "llm", description: "A company" },
              },
            ],
          },
        ]
      }
      if (sql.includes("INFO FOR DB")) {
        return [{ result: [{ tables: { entity: "...", document_chunk: "...", WORKS_FOR: "..." } }] }]
      }
      if (sql.includes("FROM WORKS_FOR")) {
        return [
          {
            result: [
              {
                id: "WORKS_FOR:xyz",
                in: "entity:doc-1_jane",
                out: "entity:doc-1_acme",
                confidence: 0.8,
                context: "Jane works at Acme",
              },
            ],
          },
        ]
      }
      return [{ result: [] }]
    })
    configureKb({ vectors: { query: asVectorQuery(surrealQuery), relate: vi.fn(async () => {}), cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })), healthCheck: vi.fn(async () => true) } })
    const handleRequest = await loadHandler(findFirst)

    const res = await handleRequest(authRequest("/v1/documents/doc-1/intelligence"))
    expect(res.status).toBe(200)
    const body = await res.json()
    expect(body).toEqual({
      entities: [
        {
          id: "entity:doc-1_acme",
          name: "Acme",
          type: "Organization",
          confidence: 0.9,
          metadata: { pattern: "llm", description: "A company" },
        },
      ],
      relations: [
        {
          id: "WORKS_FOR:xyz",
          in: "entity:doc-1_jane",
          out: "entity:doc-1_acme",
          relation_type: "WORKS_FOR",
          confidence: 0.8,
          metadata: { context: "Jane works at Acme" },
        },
      ],
      status: "completed",
      stats: { totalEntities: 1, totalRelations: 1, entityTypes: 1, relationTypes: 1 },
    })
  })

  it("reports a document still processing as processing, and a failed one as failed", async () => {
    const surrealQuery = vi.fn(async (): Promise<VectorQueryResult<unknown>[]> => [{ result: [] }])
    configureKb({ vectors: { query: asVectorQuery(surrealQuery), relate: vi.fn(async () => {}), cleanupDocumentIntelligence: vi.fn(async () => ({ deletedRelationTables: 0, entitiesDeleted: false, chunksDeleted: false })), healthCheck: vi.fn(async () => true) } })

    const findFirstProcessing = vi.fn(async () => ({ id: "doc-2", status: "processing" }))
    const handleRequestA = await loadHandler(findFirstProcessing)
    const resA = await handleRequestA(authRequest("/v1/documents/doc-2/intelligence"))
    expect((await resA.json()).status).toBe("processing")

    vi.resetModules()
    const findFirstFailed = vi.fn(async () => ({ id: "doc-3", status: "failed" }))
    const handleRequestB = await loadHandler(findFirstFailed)
    const resB = await handleRequestB(authRequest("/v1/documents/doc-3/intelligence"))
    expect((await resB.json()).status).toBe("failed")
  })
})
