//! SQL query templates and limits for Postgres store.

/// Largest page the document list will return.
pub const MAX_LIMIT: u32 = 200;

pub const SELECT_BY_ID: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d.id = $1 AND d."tenantId" = $2 AND d."deletedAt" IS NULL
"#;

/// Same row as `SELECT_BY_ID`, but soft-deleted documents included. Only the
/// hard-delete path looks a document up this way.
pub const SELECT_BY_ID_WITH_DELETED: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d.id = $1 AND d."tenantId" = $2
"#;

pub const SELECT_ALIVE_BY_IDS: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d."tenantId" = $1
       AND d.id = ANY($2::text[])
       AND d."deletedAt" IS NULL
"#;

pub const SELECT_DOCS_TENANT_ONLY: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d."tenantId" = $1 AND d."deletedAt" IS NULL
       AND ($3::text IS NULL OR d."externalRef" = $3)
     ORDER BY d."createdAt" DESC
     LIMIT $2
"#;

pub const SELECT_DOCS_WITH_KB: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d."tenantId" = $1
       AND d."deletedAt" IS NULL
       AND ($4::text IS NULL OR d."externalRef" = $4)
       AND EXISTS (SELECT 1 FROM "DocumentGroup" gf
                    WHERE gf."documentId" = d.id AND gf."knowledgeBaseId" = ANY($2::text[]))
     ORDER BY d."createdAt" DESC
     LIMIT $3
"#;

pub const SELECT_KNOWLEDGE_BASES: &str = r#"
    SELECT k.id, k.name, k.description, k.color,
           (SELECT COUNT(*) FROM "DocumentGroup" g
             JOIN "Document" d ON d.id = g."documentId"
            WHERE g."knowledgeBaseId" = k.id
              AND d."deletedAt" IS NULL) AS document_count
      FROM "KnowledgeBase" k
     WHERE k."tenantId" = $1
     ORDER BY k.name ASC
"#;

pub const SELECT_API_KEY: &str = r#"
    SELECT id, "tenantId", scopes, "knowledgeBaseIds", hash
      FROM "ApiKey"
     WHERE hash = $1
       AND "revokedAt" IS NULL
"#;

pub const UPDATE_API_KEY_LAST_USED: &str = r#"
    UPDATE "ApiKey" SET "lastUsedAt" = now() WHERE id = $1
"#;

/// Row for a minted key. Defaults for `lastUsedAt`, `revokedAt` and
/// `createdAt` are left to the schema, exactly as Prisma's `create` does.
pub const INSERT_API_KEY: &str = r#"
    INSERT INTO "ApiKey" (id, "tenantId", name, hash, scopes, "knowledgeBaseIds")
    VALUES ($1, $2, $3, $4, $5, $6)
"#;

// ─── Migrations ──────────────────────────────────────────────────────────────
// Prisma's own bookkeeping table, so `kb-server migrate` and
// `bunx prisma migrate deploy` agree about what has been applied to the same
// database. Column names, types and the primary key are Prisma's.

/// Created defensively: the table usually exists because Prisma made it, but a
/// database that has only ever seen the Rust binary must still work.
pub const CREATE_MIGRATIONS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS "_prisma_migrations" (
        "id" TEXT NOT NULL,
        "checksum" TEXT NOT NULL,
        "migration_name" TEXT NOT NULL,
        -- Timestamps are `TIMESTAMPTZ`, matching what Prisma's own migration
        -- engine creates. It used to be `TIMESTAMP(3)` here, which is a different
        -- type: the table then depended on which runtime migrated first, and any
        -- code reading these columns had to work for both. Nothing reads them any
        -- more (see `SELECT_APPLIED_MIGRATION`), but a Rust-first database should
        -- still look exactly like a Prisma-first one. `IF NOT EXISTS` means an
        -- existing table is never altered.
        "started_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
        "finished_at" TIMESTAMPTZ,
        "rolled_back_at" TIMESTAMPTZ,
        "logs" TEXT,
        "applied_steps_count" INTEGER NOT NULL DEFAULT 0,
        CONSTRAINT "_prisma_migrations_pkey" PRIMARY KEY ("id")
    )
"#;

/// A migration counts as applied only once it finished and was not rolled back.
/// One applied migration's checksum, or no row.
///
/// The `IS NOT NULL` / `IS NULL` predicates are in the WHERE clause rather than
/// applied in Rust on purpose: Prisma creates these columns as `timestamptz` and
/// this crate's own CREATE TABLE makes them `TIMESTAMP(3)`, so a timestamp decoded
/// in Rust cannot succeed against both. Null-ness is not a type question, and
/// asking the engine for it keeps the only decoded column a `TEXT` both agree on.
pub const SELECT_APPLIED_MIGRATION: &str = r#"
    SELECT "checksum"
      FROM "_prisma_migrations"
     WHERE "migration_name" = $1
       AND "finished_at" IS NOT NULL
       AND "rolled_back_at" IS NULL
     ORDER BY "started_at" ASC
     LIMIT 1
"#;

pub const INSERT_MIGRATION: &str = r#"
    INSERT INTO "_prisma_migrations" (id, "checksum", "migration_name", "started_at", "applied_steps_count")
    VALUES ($1, $2, $3, now(), 0)
"#;

pub const FINISH_MIGRATION: &str = r#"
    UPDATE "_prisma_migrations"
       SET "finished_at" = now(), "applied_steps_count" = 1, "logs" = NULL
     WHERE id = $1
"#;

// ─── Ingest jobs ─────────────────────────────────────────────────────────────
// Column names and shapes are copied verbatim from the Bun adapter
// (src/service/adapters.ts, `const jobs: JobStore`) so a Rust worker and a Bun
// worker can be pointed at the same queue during cutover.

/// Claim the oldest pending job and flip it to `processing`. The inner
/// `FOR UPDATE SKIP LOCKED` is what keeps two pollers off the same row.
pub const CLAIM_NEXT_PENDING: &str = r#"
    UPDATE "IngestJob"
       SET status = 'processing', "startedAt" = now(), "updatedAt" = now(), step = 'queued', progress = 0
     WHERE id = (
       SELECT id FROM "IngestJob"
        WHERE status = 'pending'
        ORDER BY "createdAt" ASC
        LIMIT 1
        FOR UPDATE SKIP LOCKED
     )
    RETURNING id, "tenantId", "externalRef", "documentId", "s3Key", filename, "mimeType", attempt, params
"#;

/// Persist step, overall progress and per-step counters.
pub const UPDATE_JOB_PROGRESS: &str = r#"
    UPDATE "IngestJob"
       SET step = $2, progress = $3, "stepCurrent" = $4, "stepTotal" = $5, "etaSeconds" = $6, "updatedAt" = now()
     WHERE id = $1
"#;

/// Terminal success. Clears the error and the per-step counters.
pub const FINISH_JOB_SUCCESS: &str = r#"
    UPDATE "IngestJob"
       SET status = 'success', "documentId" = $2, error = NULL, step = 'done', progress = 100,
           "stepCurrent" = NULL, "stepTotal" = NULL, "etaSeconds" = 0, "updatedAt" = now()
     WHERE id = $1
"#;

/// Terminal failure. `error` arrives pre-truncated to 1000 characters.
pub const FINISH_JOB_FAILURE: &str = r#"
    UPDATE "IngestJob"
       SET status = 'failed', error = $2, "updatedAt" = now()
     WHERE id = $1
"#;

/// Heartbeat. A long emit-less step would otherwise trip the stale reclaim.
pub const TOUCH_JOB: &str = r#"UPDATE "IngestJob" SET "updatedAt" = now() WHERE id = $1"#;

/// Jobs whose worker died mid-run. Takes no row locks, exactly like the
/// TypeScript sweep's `findMany`; `REQUEUE_JOB` and `FAIL_JOB_STALLED` carry
/// the guard that the missing lock used to provide.
pub const SELECT_STALE_JOBS: &str = r#"
    SELECT id, attempt, "documentId"
      FROM "IngestJob"
     WHERE status = 'processing' AND "updatedAt" < now() - make_interval(secs => $1::double precision)
     ORDER BY "createdAt" ASC
"#;

/// Give a stranded job another attempt.
pub const REQUEUE_JOB: &str = r#"
    UPDATE "IngestJob"
       SET status = 'pending', attempt = attempt + 1, "startedAt" = NULL,
           step = 'queued', progress = 0, "updatedAt" = now()
     WHERE id = $1 AND status = 'processing'
       AND "updatedAt" < now() - make_interval(secs => $2::double precision)
"#;

/// Attempts exhausted: the job and its document both go to `failed`.
pub const FAIL_JOB_STALLED: &str = r#"
    UPDATE "IngestJob"
       SET status = 'failed', error = 'ingest stalled (max attempts reached)', "updatedAt" = now()
     WHERE id = $1 AND status = 'processing'
       AND "updatedAt" < now() - make_interval(secs => $2::double precision)
"#;

/// Document status transition, shared by the store and the reclaim sweep.
pub const SET_DOCUMENT_STATUS: &str = r#"UPDATE "Document" SET status = $2 WHERE id = $1"#;

/// The write the ingest worker performs between extraction and chunking.
pub const UPDATE_EXTRACTED_CONTENT: &str = r#"
    UPDATE "Document"
       SET content = $2, "fileType" = $3, "fileSize" = $4, "updatedAt" = now()
     WHERE id = $1
"#;

/// Merge JSON fields into Document.metadata, matching Bun `kb("documents").updateMetadata`.
pub const UPDATE_METADATA: &str = r#"
    UPDATE "Document"
       SET metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb, "updatedAt" = now()
     WHERE id = $1
"#;

// ─── Document Mutations & Raw Streaming ──────────────────────────────────────

pub const INSERT_DOCUMENT: &str = r#"
    INSERT INTO "Document" (
        id, "tenantId", title, content, categories, "subcategory",
        "s3Key", "fileSize", "mimeType", status, "externalRef",
        "createdAt", "updatedAt"
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now(), now()
    )
"#;

pub const INSERT_DOCUMENT_GROUP: &str = r#"
    INSERT INTO "DocumentGroup" (id, "documentId", "knowledgeBaseId", "createdAt")
    VALUES ($1, $2, $3, now())
    ON CONFLICT ("documentId", "knowledgeBaseId") DO NOTHING
"#;

pub const SELECT_FOR_DOWNLOAD: &str = r#"
    SELECT id, "s3Key" AS s3_key, "mimeType" AS mime_type, title
      FROM "Document"
     WHERE id = $1 AND "tenantId" = $2 AND "deletedAt" IS NULL
"#;

pub const SELECT_EXISTS_FOR_PATCH: &str = r#"
    SELECT id FROM "Document" WHERE id = $1 AND "tenantId" = $2 AND "deletedAt" IS NULL
"#;

pub const UPDATE_DOCUMENT_PATCH: &str = r#"
    UPDATE "Document"
       SET title = CASE WHEN $2::boolean THEN $3 ELSE title END,
           categories = CASE WHEN $4::boolean THEN $5::text[] ELSE categories END,
           "subcategory" = CASE WHEN $6::boolean THEN $7 ELSE "subcategory" END,
           "updatedAt" = now()
     WHERE id = $1
"#;

pub const DELETE_DOCUMENT_GROUPS: &str = r#"
    DELETE FROM "DocumentGroup" WHERE "documentId" = $1
"#;

pub const SELECT_OWNED_KB_IDS: &str = r#"
    SELECT id FROM "KnowledgeBase" WHERE "tenantId" = $1 AND id = ANY($2::text[])
"#;

// ─── KnowledgeBase CRUD ──────────────────────────────────────────────────────

pub const INSERT_KNOWLEDGE_BASE: &str = r#"
    INSERT INTO "KnowledgeBase" (id, "tenantId", name, description, color, "createdAt", "updatedAt")
    VALUES ($1, $2, $3, $4, $5, now(), now())
    RETURNING id, "tenantId", name, description, color,
              to_char("createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
              to_char("updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const SELECT_KB_BY_ID_TENANT: &str = r#"
    SELECT id, "tenantId", name, description, color,
           to_char("createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char("updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
      FROM "KnowledgeBase"
     WHERE id = $1 AND "tenantId" = $2
"#;

pub const UPDATE_KNOWLEDGE_BASE: &str = r#"
    UPDATE "KnowledgeBase"
       SET name = CASE WHEN $3::boolean THEN $4 ELSE name END,
           description = CASE WHEN $5::boolean THEN $6 ELSE description END,
           color = CASE WHEN $7::boolean THEN $8 ELSE color END,
           "updatedAt" = now()
     WHERE id = $1 AND "tenantId" = $2
     RETURNING id, "tenantId", name, description, color,
               to_char("createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
               to_char("updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
"#;

pub const DELETE_KB_GROUPS: &str = r#"
    DELETE FROM "DocumentGroup" WHERE "knowledgeBaseId" = $1
"#;

pub const DELETE_KNOWLEDGE_BASE: &str = r#"
    DELETE FROM "KnowledgeBase" WHERE id = $1 AND "tenantId" = $2
"#;

// ─── Category CRUD ───────────────────────────────────────────────────────────

pub const SELECT_CATEGORIES: &str = r#"
    SELECT id, name, label, color
      FROM "Category"
     WHERE "tenantId" = $1
     ORDER BY name ASC
"#;

pub const INSERT_CATEGORY: &str = r#"
    INSERT INTO "Category" (id, "tenantId", name, label, color, "createdAt", "updatedAt")
    VALUES ($1, $2, $3, $4, $5, now(), now())
    RETURNING id, name, label, color
"#;

pub const SELECT_CATEGORY_BY_ID_TENANT: &str = r#"
    SELECT id, name, label, color
      FROM "Category"
     WHERE id = $1 AND "tenantId" = $2
"#;

pub const UPDATE_CATEGORY: &str = r#"
    UPDATE "Category"
       SET name = CASE WHEN $3::boolean THEN $4 ELSE name END,
           label = CASE WHEN $5::boolean THEN $6 ELSE label END,
           color = CASE WHEN $7::boolean THEN $8 ELSE color END,
           "updatedAt" = now()
     WHERE id = $1 AND "tenantId" = $2
     RETURNING id, name, label, color
"#;

pub const DELETE_CATEGORY: &str = r#"
    DELETE FROM "Category" WHERE id = $1 AND "tenantId" = $2
"#;

// ─── IngestJob Queries ───────────────────────────────────────────────────────

pub const INSERT_INGEST_JOB: &str = r#"
    INSERT INTO "IngestJob" (
        id, "tenantId", "externalRef", filename, "fileSize", "mimeType",
        "s3Key", "documentId", status, step, params, "createdAt", "updatedAt"
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, 'pending', 'queued', $9, now(), now()
    )
"#;

pub const SELECT_JOB_BY_ID_TENANT: &str = r#"
    SELECT id, status, step, progress, "stepCurrent", "stepTotal", "etaSeconds",
           attempt, error, "documentId"
      FROM "IngestJob"
     WHERE id = $1 AND "tenantId" = $2
"#;

pub const SELECT_JOB_FOR_RETRY: &str = r#"
    SELECT id, status, "documentId"
      FROM "IngestJob"
     WHERE id = $1 AND "tenantId" = $2
"#;

pub const RESET_JOB_FOR_RETRY: &str = r#"
    UPDATE "IngestJob"
       SET status = 'pending', attempt = attempt + 1, error = NULL, step = 'queued',
           progress = 0, "stepCurrent" = NULL, "stepTotal" = NULL, "etaSeconds" = NULL,
           "startedAt" = NULL, "updatedAt" = now()
     WHERE id = $1
"#;

pub const SELECT_REAPABLE_JOBS: &str = r#"
    SELECT id, "s3Key"
      FROM "IngestJob"
     WHERE status = 'failed'
       AND "updatedAt" < now() - make_interval(days => $1::integer)
       AND "s3Key" IS NOT NULL
     LIMIT $2
"#;

pub const CLEAR_JOB_S3_KEY: &str = r#"
    UPDATE "IngestJob" SET "s3Key" = NULL, "updatedAt" = now() WHERE id = $1
"#;
