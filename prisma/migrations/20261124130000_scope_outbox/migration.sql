-- The scope outbox (SEC-2b).
--
-- Postgres decides which documents a caller may see; Qdrant's
-- `knowledge_base_id` payload is only a fast pre-filter in predicate mode. A
-- document that moves between knowledge bases, or is deleted, leaves that
-- payload stale: the fail-closed verify still hides the document, but recall is
-- lost and the stale points remain. Each write that changes membership or
-- deletes a document inserts one row here IN THE SAME TRANSACTION, so a
-- rolled-back change leaves no repair behind and a committed change cannot be
-- forgotten.
--
-- A worker claims rows with `FOR UPDATE SKIP LOCKED`, increments `attempts`,
-- applies the repair, then deletes the row. A row that keeps failing is left
-- after ten attempts and logged at error level: an operator can still see the
-- document id, the error and how old it is.
--
-- Additive only: nothing in the Bun schema is touched, so a mixed fleet during
-- cutover can keep running against this database.

-- CreateTable
CREATE TABLE "ScopeOutbox" (
    "id" BIGSERIAL NOT NULL,
    "documentId" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    "op" TEXT NOT NULL,
    "createdAt" TIMESTAMPTZ NOT NULL DEFAULT now(),
    "attempts" INTEGER NOT NULL DEFAULT 0,
    "lastError" TEXT,

    CONSTRAINT "ScopeOutbox_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
-- The claim takes the oldest rows first; this index also answers the health
-- field's "oldest pending" lookup without a sequential scan.
CREATE INDEX "ScopeOutbox_createdAt_idx" ON "ScopeOutbox"("createdAt");

-- Add the two documented operation values as a check constraint so a future
-- adapter cannot write a typo that a worker would silently skip.
ALTER TABLE "ScopeOutbox" ADD CONSTRAINT "ScopeOutbox_op_check"
    CHECK ("op" IN ('kb_changed', 'deleted'));
