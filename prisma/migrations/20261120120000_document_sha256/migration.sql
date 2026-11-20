-- AlterTable
--
-- The content hash of an uploaded file, so the upload route can refuse a file
-- the tenant already has (`KB_UPLOAD_DEDUPE`). Additive and nullable: a row
-- that predates this migration has no hash, and a row written while the
-- dedupe was off has none either. Nothing existing changes, and every query
-- that does not name the column keeps working.
ALTER TABLE "Document" ADD COLUMN "sha256" TEXT;

-- The dedupe asks "which live documents hold these bytes", so the hash is
-- worth an index. The lookup is tenant-scoped, so the tenant leads.
CREATE INDEX "Document_tenantId_sha256_idx" ON "Document"("tenantId", "sha256");
