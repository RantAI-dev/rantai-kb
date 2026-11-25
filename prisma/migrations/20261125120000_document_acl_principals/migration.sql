-- Per-document access lists (SEC-3).
--
-- An empty array means "no ACL": the document is visible exactly as it was
-- before this migration. A non-empty array is visible only to a request that
-- asserts one of the listed principals, and is invisible (default-deny) to a
-- request that asserts none. The default keeps every existing row on the old
-- behaviour, so no backfill is needed.
--
-- The GIN index serves the `&&` overlap predicate in the scope query, with the
-- tenant still filtered first by the surrounding statement.

-- AlterTable
ALTER TABLE "Document"
    ADD COLUMN "aclPrincipals" TEXT[] NOT NULL DEFAULT '{}';

-- Index
CREATE INDEX "Document_aclPrincipals_idx" ON "Document" USING GIN ("aclPrincipals");
