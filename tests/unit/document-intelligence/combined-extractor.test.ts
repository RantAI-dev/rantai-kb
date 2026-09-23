import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { CombinedExtractor } from "@/lib/document-intelligence/combined-extractor"

/**
 * CombinedExtractor.parseResponse (private, exercised through extract()).
 *
 * Bug: the old code found the first "{" and sliced to it BEFORE stripping
 * code fences. For "```json\n{...}\n```" that removes the opening "```json"
 * (it was before the "{") but leaves the trailing "```" (it's after the
 * matched JSON body, never touched by the fence-stripping branch because by
 * then `cleaned` no longer starts with "```"). JSON.parse then fails on the
 * dangling fence, the truncated-JSON repair pass also fails against it, and
 * extraction silently resolves to `{ entities: [], relations: [] }` — the
 * same shape a genuinely empty document produces. Reasoning models that
 * fence their JSON (optionally after a <think> block) hit this every time.
 */

const SAMPLE_JSON = JSON.stringify({
  entities: [{ name: "Acme Corp", type: "ORG", description: "A company", confidence: 0.9 }],
  relations: [],
})

function mockChatCompletion(content: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({ choices: [{ message: { content } }] }),
    }))
  )
}

async function extractOne(content: string) {
  mockChatCompletion(content)
  const extractor = new CombinedExtractor({
    apiKey: "test-key",
    maxRetries: 1,
    concurrencyLimit: 1,
    batchDelayMs: 0,
  })
  return extractor.extract("A short document about Acme Corp.", "doc-1")
}

describe("CombinedExtractor — fenced/thinking-model JSON parsing", () => {
  beforeEach(() => {
    vi.stubEnv("ENTITY_EXTRACTION_LLM_BASE_URL", "")
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.unstubAllEnvs()
  })

  it("parses bare JSON (no fence)", async () => {
    const result = await extractOne(SAMPLE_JSON)
    expect(result.entities).toHaveLength(1)
    expect(result.entities[0].name).toBe("Acme Corp")
  })

  it("parses JSON wrapped in a ```json fence", async () => {
    const fenced = "```json\n" + SAMPLE_JSON + "\n```"
    const result = await extractOne(fenced)
    expect(result.entities).toHaveLength(1)
    expect(result.entities[0].name).toBe("Acme Corp")
  })

  it("parses a ```json fence that follows a <think> block", async () => {
    const withThink = "<think>\nLet me work through this...\n</think>\n```json\n" + SAMPLE_JSON + "\n```"
    const result = await extractOne(withThink)
    expect(result.entities).toHaveLength(1)
    expect(result.entities[0].name).toBe("Acme Corp")
  })
})
