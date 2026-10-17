-- Entity graph in plain Postgres tables (docs/architecture-target.md §11,
-- docs/plan-best-multimodal-kb.md step L.3).
--
-- Additive only: three new tables and one new table for the per-tenant version
-- counter. No existing table, column, index or constraint is touched, so `bunx
-- prisma migrate deploy` and `kb-server migrate` can both apply this while Bun
-- and Rust run in the same fleet, and neither can see the other's rows.
--
-- Bun does not read these tables in this round. They are the storage the Rust
-- GraphStore will move onto when ingest and the entity/graph arm are switched
-- over; see docs/rust-cutover.md.

-- CreateTable
CREATE TABLE "KbEntity" (
    "id" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    -- Canonical key. The dedupe identity: two extractions that resolve to the
    -- same key are one entity in the tenant, in every document.
    "key" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "type" TEXT NOT NULL DEFAULT '',
    "aliases" TEXT[] DEFAULT ARRAY[]::TEXT[],
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "KbEntity_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "KbMention" (
    "id" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    "entityId" TEXT NOT NULL,
    "chunkId" TEXT NOT NULL,
    "documentId" TEXT NOT NULL,

    CONSTRAINT "KbMention_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "KbRelation" (
    "id" TEXT NOT NULL,
    "tenantId" TEXT NOT NULL,
    "srcEntityId" TEXT NOT NULL,
    "dstEntityId" TEXT NOT NULL,
    "type" TEXT NOT NULL DEFAULT '',
    "weight" DOUBLE PRECISION NOT NULL DEFAULT 1,
    "chunkId" TEXT,
    "documentId" TEXT NOT NULL,

    CONSTRAINT "KbRelation_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "KbGraphVersion" (
    "tenantId" TEXT NOT NULL,
    "version" BIGINT NOT NULL DEFAULT 0,

    CONSTRAINT "KbGraphVersion_pkey" PRIMARY KEY ("tenantId")
);

-- CreateIndex
CREATE UNIQUE INDEX "KbEntity_tenantId_key_key" ON "KbEntity"("tenantId", "key");

-- CreateIndex
CREATE INDEX "KbEntity_tenantId_name_idx" ON "KbEntity"("tenantId", "name");

-- CreateIndex
CREATE UNIQUE INDEX "KbMention_tenantId_entityId_chunkId_key" ON "KbMention"("tenantId", "entityId", "chunkId");

-- CreateIndex
CREATE INDEX "KbMention_tenantId_chunkId_idx" ON "KbMention"("tenantId", "chunkId");

-- CreateIndex
CREATE INDEX "KbMention_tenantId_documentId_idx" ON "KbMention"("tenantId", "documentId");

-- CreateIndex
CREATE INDEX "KbRelation_tenantId_documentId_idx" ON "KbRelation"("tenantId", "documentId");

-- CreateIndex
CREATE INDEX "KbRelation_tenantId_srcEntityId_idx" ON "KbRelation"("tenantId", "srcEntityId");

-- CreateIndex
CREATE INDEX "KbRelation_tenantId_dstEntityId_idx" ON "KbRelation"("tenantId", "dstEntityId");

-- AddForeignKey
ALTER TABLE "KbMention" ADD CONSTRAINT "KbMention_entityId_fkey" FOREIGN KEY ("entityId") REFERENCES "KbEntity"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "KbRelation" ADD CONSTRAINT "KbRelation_srcEntityId_fkey" FOREIGN KEY ("srcEntityId") REFERENCES "KbEntity"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "KbRelation" ADD CONSTRAINT "KbRelation_dstEntityId_fkey" FOREIGN KEY ("dstEntityId") REFERENCES "KbEntity"("id") ON DELETE CASCADE ON UPDATE CASCADE;
