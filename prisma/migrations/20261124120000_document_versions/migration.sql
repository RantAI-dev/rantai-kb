-- Document versions and point-in-time search (RET-16).
--
-- A tenant can upload a new version of the same policy under a stable `docKey`.
-- Search defaults to the current version only; `asOf` and `includeHistory` are
-- opt-in. Every column here is additive and nullable except `isLatest`, which
-- has a default so existing rows stay valid with no backfill: a row that
-- predates this migration has no docKey, no dates and is the latest version of
-- nothing, so it stays visible in every search mode.
--
-- Effectivity is a half-open interval: `effectiveFrom <= asOf < effectiveTo`.
-- A NULL bound means "unbounded on that side", so a document with no dates is
-- visible to every `asOf` query.

-- AlterTable
ALTER TABLE "Document"
    ADD COLUMN "docKey" TEXT,
    ADD COLUMN "version" TEXT,
    ADD COLUMN "supersedesDocumentId" TEXT,
    ADD COLUMN "effectiveFrom" TIMESTAMPTZ,
    ADD COLUMN "effectiveTo" TIMESTAMPTZ,
    ADD COLUMN "isLatest" BOOLEAN NOT NULL DEFAULT true;

-- Index
--
-- Superseding an old version looks up the current row by (tenant, docKey), so
-- the tenant leads. The index is also the one the scope query's `isLatest`
-- predicate rides on.
CREATE INDEX "Document_tenantId_docKey_idx" ON "Document"("tenantId", "docKey");
