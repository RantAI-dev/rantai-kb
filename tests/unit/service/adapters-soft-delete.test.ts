import { describe, it, expect, vi, beforeEach, afterEach } from "vitest"
import { FakeTable } from "../../helpers/prisma-fake"

/**
 * `documents.findAliveMetaByIds` (service/adapters.ts) is the join every
 * retrieval path uses to turn a SurrealDB chunk hit into a citable source —
 * searchSimilar / searchByDocumentIds / searchByVector (src/lib/rag/vector-store.ts)
 * all call it and drop any chunk whose document doesn't come back. So "does
 * search respect a soft delete" reduces to "does this function exclude
 * deletedAt rows", which is what these tests pin down directly against the
 * real adapter code (not a fake standing in for it).
 *
 * Verified pre-existing: this file predates the new HTTP routes — the adapter
 * already filtered `deletedAt: null` for every alive-doc query before this
 * wave added a way to actually set deletedAt. It is exercised here rather
 * than "fixed", per the task: confirm, and fix only if it doesn't hold.
 */

type DocRow = {
  id: string
  tenantId: string
  title: string
  categories: string[]
  subcategory: string | null
  deletedAt: Date | null
}

async function loadDocumentsAdapter(rows: DocRow[]) {
  const document = new FakeTable<DocRow>(rows)
  vi.doMock("@/service/db", () => ({ prisma: { document } }))
  const { serviceKbRuntime, withTenant } = await import("@/service/adapters")
  return { documents: serviceKbRuntime().documents, withTenant, document }
}

describe("documents adapter — soft delete + tenant scoping", () => {
  beforeEach(() => {
    vi.resetModules()
  })
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("findAliveMetaByIds excludes a soft-deleted document", async () => {
    const rows: DocRow[] = [
      { id: "doc-1", tenantId: "tenant-a", title: "Alive", categories: [], subcategory: null, deletedAt: null },
      {
        id: "doc-2",
        tenantId: "tenant-a",
        title: "Soft-deleted",
        categories: [],
        subcategory: null,
        deletedAt: new Date("2026-01-01"),
      },
    ]
    const { documents, withTenant } = await loadDocumentsAdapter(rows)

    const result = await withTenant("tenant-a", () => documents.findAliveMetaByIds(["doc-1", "doc-2"]))
    expect(result.map((d) => d.id)).toEqual(["doc-1"])
  })

  it("findAliveMetaByIds excludes another tenant's document (owner control included)", async () => {
    const rows: DocRow[] = [
      { id: "doc-1", tenantId: "tenant-a", title: "Mine", categories: [], subcategory: null, deletedAt: null },
      { id: "doc-2", tenantId: "tenant-b", title: "Theirs", categories: [], subcategory: null, deletedAt: null },
    ]
    const { documents, withTenant } = await loadDocumentsAdapter(rows)

    const asTenantA = await withTenant("tenant-a", () => documents.findAliveMetaByIds(["doc-1", "doc-2"]))
    expect(asTenantA.map((d) => d.id)).toEqual(["doc-1"]) // owner control: tenant-a's own doc comes back
  })

  it("findAliveIdsByFilter also excludes soft-deleted rows", async () => {
    const rows: DocRow[] = [
      { id: "doc-1", tenantId: "tenant-a", title: "Alive", categories: ["policy"], subcategory: null, deletedAt: null },
      {
        id: "doc-2",
        tenantId: "tenant-a",
        title: "Soft-deleted",
        categories: ["policy"],
        subcategory: null,
        deletedAt: new Date(),
      },
    ]
    const { documents, withTenant } = await loadDocumentsAdapter(rows)

    const ids = await withTenant("tenant-a", () => documents.findAliveIdsByFilter({ category: "policy" }))
    expect(ids).toEqual(["doc-1"])
  })
})
