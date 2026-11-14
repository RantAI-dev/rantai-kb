-- Stored tables (`TBL-1`): one row per (table, version) and a Postgres record
-- for each document table whose grid lives as Parquet in the blob store.
--
-- Additive only: one new table, its indexes and one foreign key to `Document`.
-- No existing table, column, index or constraint is touched, so
-- `bunx prisma migrate deploy` and `kb-server migrate` can both apply this
-- while Bun and Rust run in the same fleet, and Bun never sees these rows.
--
-- `id` is `{documentId}_t{index}` (document order), so it is stable across a
-- re-ingest. A re-ingest writes a new `version` for the whole document; the
-- previous version is kept — a query that started on it is still reading its
-- grid — and the one before that is deleted together with its Parquet object.
-- That is why the primary key is (id, version), not id.
--
-- `gridKey` points at `tables/{tenantId}/{documentId}/{id}/v{version}.parquet`
-- in the blob store. `manifest` is the `TableManifest` JSON (columns, roles,
-- units, derived rows, confidence); `header` is the column names in order, kept
-- as a separate array because it is what a caller sees first.
--
-- The foreign key cascades on a hard delete: losing the document must not leave
-- queryable tables behind. The route still deletes them explicitly to remove
-- the Parquet objects, which Postgres cannot do.

-- CreateTable
CREATE TABLE "KbTable" (
    "id" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    "documentId" TEXT NOT NULL,
    "idx" INTEGER NOT NULL,
    "version" INTEGER NOT NULL,
    "source" TEXT NOT NULL,
    "sheet" TEXT,
    "pageSpan" JSONB,
    "title" TEXT,
    "caption" TEXT,
    "header" TEXT[] DEFAULT ARRAY[]::TEXT[],
    "manifest" JSONB NOT NULL,
    "gridKey" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "KbTable_pkey" PRIMARY KEY ("id", "version")
);

-- CreateIndex
CREATE INDEX "KbTable_tenantId_idx" ON "KbTable"("tenantId");

-- CreateIndex
CREATE INDEX "KbTable_tenantId_documentId_idx" ON "KbTable"("tenantId", "documentId");

-- CreateIndex
CREATE INDEX "KbTable_tenantId_documentId_version_idx" ON "KbTable"("tenantId", "documentId", "version");

-- AddForeignKey
ALTER TABLE "KbTable" ADD CONSTRAINT "KbTable_documentId_fkey" FOREIGN KEY ("documentId") REFERENCES "Document"("id") ON DELETE CASCADE ON UPDATE CASCADE;
