-- Graph community summaries for the global retrieval arm (`DEC-5`).
--
-- Additive only: one new table and two new indexes. No existing table, column,
-- index or constraint is touched, so `bunx prisma migrate deploy` and
-- `kb-server migrate` can both apply this while Bun and Rust run in the same
-- fleet, and neither can see the other's rows.
--
-- Rows are written only by `kb-server communities build`, an operator command:
-- a summary pass is a paid LLM batch and AGENTS rule 7 forbids starting one
-- automatically. The retrieval arm reads them when `KB_GLOBAL_ARM=1` and the
-- query is aggregative; with the flag off nothing reads this table.
--
-- "documentIds" and "chunkIds" are the community's members. The retrieval read
-- filters `"documentIds" && <scope>` and narrows both arrays to the caller's
-- scope, because a summary written from documents the caller cannot see must
-- never influence their ranking. "entityKeys" is the community's identity for
-- diagnostics; "version" is bumped by every rebuild, which replaces the
-- tenant's whole set in one transaction.

-- CreateTable
CREATE TABLE "KbCommunity" (
    "id" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    "version" BIGINT NOT NULL,
    "summary" TEXT NOT NULL,
    "entityKeys" TEXT[] DEFAULT ARRAY[]::TEXT[],
    "documentIds" TEXT[] DEFAULT ARRAY[]::TEXT[],
    "chunkIds" TEXT[] DEFAULT ARRAY[]::TEXT[],
    "model" TEXT NOT NULL DEFAULT '',
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "KbCommunity_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE INDEX "KbCommunity_tenantId_idx" ON "KbCommunity"("tenantId");

-- CreateIndex
CREATE INDEX "KbCommunity_tenantId_version_idx" ON "KbCommunity"("tenantId", "version");
