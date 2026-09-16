import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"

describe("query/passage embedding prefixes", () => {
  const originalFetch = global.fetch
  const originalEnv = { ...process.env }

  beforeEach(() => {
    process.env.OPENROUTER_API_KEY = "test-key"
    process.env.KB_EMBEDDING_MODEL = "intfloat/multilingual-e5-small"
    delete process.env.KB_EMBEDDING_QUERY_PREFIX
    delete process.env.KB_EMBEDDING_PASSAGE_PREFIX
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ data: [{ embedding: new Array(384).fill(0.1) }] }),
    }) as any
    vi.resetModules()
  })

  afterEach(() => {
    global.fetch = originalFetch
    process.env = { ...originalEnv }
  })

  it("sends raw text with no prefix when unconfigured (today's behavior, unchanged)", async () => {
    const { generateEmbedding } = await import("@/lib/rag/embeddings")
    await generateEmbedding("berapa lama cuti melahirkan?")
    const body = JSON.parse((global.fetch as any).mock.calls[0][1].body)
    expect(body.input).toBe("berapa lama cuti melahirkan?")
  })

  it("prepends KB_EMBEDDING_QUERY_PREFIX to query-path text (generateEmbedding)", async () => {
    process.env.KB_EMBEDDING_QUERY_PREFIX = "query: "
    const { generateEmbedding } = await import("@/lib/rag/embeddings")
    await generateEmbedding("berapa lama cuti melahirkan?")
    const body = JSON.parse((global.fetch as any).mock.calls[0][1].body)
    expect(body.input).toBe("query: berapa lama cuti melahirkan?")
  })

  it("prepends KB_EMBEDDING_PASSAGE_PREFIX to storage-path text (generateEmbeddings), not the query prefix", async () => {
    process.env.KB_EMBEDDING_QUERY_PREFIX = "query: "
    process.env.KB_EMBEDDING_PASSAGE_PREFIX = "passage: "
    ;(global.fetch as any).mockResolvedValue({
      ok: true,
      json: async () => ({
        data: [
          { embedding: new Array(384).fill(0) },
          { embedding: new Array(384).fill(0) },
        ],
      }),
    })
    const { generateEmbeddings } = await import("@/lib/rag/embeddings")
    await generateEmbeddings(["cuti tahunan 12 hari", "cuti melahirkan 3 bulan"])
    const body = JSON.parse((global.fetch as any).mock.calls[0][1].body)
    expect(body.input).toEqual([
      "passage: cuti tahunan 12 hari",
      "passage: cuti melahirkan 3 bulan",
    ])
  })

  it("leaves the MiniMax type:'query'/'db' shape intact and prefixes on top of it", async () => {
    process.env.KB_EMBEDDING_BASE_URL = "https://api.minimax.io/v1/embeddings"
    process.env.KB_EMBEDDING_QUERY_PREFIX = "query: "
    const { generateEmbedding } = await import("@/lib/rag/embeddings")
    ;(global.fetch as any).mockResolvedValue({
      ok: true,
      json: async () => ({ vectors: [[0.1, 0.2]], base_resp: { status_code: 0 } }),
    })
    await generateEmbedding("cuti tahunan")
    const body = JSON.parse((global.fetch as any).mock.calls[0][1].body)
    expect(body.type).toBe("query")
    expect(body.texts).toEqual(["query: cuti tahunan"])
  })
})
