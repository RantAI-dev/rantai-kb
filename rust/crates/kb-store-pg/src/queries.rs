//! SQL query templates and limits for Postgres store.

/// Largest page the document list will return.
///
/// One number, shared with the HTTP layer, so the route and the store cannot
/// disagree about what a page is.
pub const MAX_LIMIT: u32 = kb_core::pagination::MAX_PAGE_SIZE;

pub const SELECT_BY_ID: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
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
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d.id = $1 AND d."tenantId" = $2
"#;

/// `SELECT_BY_ID` with the SEC-3 access-list predicate. Every document-addressed
/// route uses this: `$3` is the caller's asserted principals, and an ACL'd
/// document is returned only when it overlaps that list. The same clause as
/// `scope_sql_inner`; an empty `$3` hides every ACL'd document.
pub const SELECT_VISIBLE_BY_ID: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d.id = $1 AND d."tenantId" = $2 AND d."deletedAt" IS NULL
       AND (cardinality(d."aclPrincipals") = 0 OR d."aclPrincipals" && $3::text[])
 "#;

pub const SELECT_ALIVE_BY_IDS: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
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

/// Section outlines for the tree arm (DEC-4).
///
/// Tenant and the caller's visible document ids are both bound: `$1` is the
/// tenant, `$2` the scope's document list. A document with no outline is
/// filtered out rather than returned as a JSON `null`, so the arm can tell "no
/// structure" from "never extracted". The whole contract is one statement, so
/// there is no second read a scope bug could hide in.
pub const SELECT_OUTLINES: &str = r#"
    SELECT d.id,
           d.metadata -> 'outline' AS outline
      FROM "Document" d
     WHERE d."tenantId" = $1
       AND d.id = ANY($2::text[])
       AND d."deletedAt" IS NULL
       AND jsonb_typeof(d.metadata -> 'outline') = 'object'
"#;

/// One page of a tenant's documents, newest first.
///
/// The order is total — `(createdAt, id)` — so the cursor predicate
/// `(createdAt, id) < ($4, $5)` can start a page strictly after the last row
/// of the previous one without repeating or skipping a row. `$4` is the
/// cursor's timestamp and `$5` its id; both are NULL when the page is the
/// first one, and the predicate is then true for every row.
///
/// `created_at_ts` is the raw `TIMESTAMP(3)` column: the cursor is built from
/// it, so the walk continues from the *same* value the ordering uses, whatever
/// the database session's time zone is.
pub const SELECT_DOCS_TENANT_ONLY: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           d."createdAt" AS created_at_ts,
           to_char(d."updatedAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at,
           COALESCE((SELECT array_agg(g2."knowledgeBaseId" ORDER BY g2."knowledgeBaseId")
                       FROM "DocumentGroup" g2 WHERE g2."documentId" = d.id), '{}') AS knowledge_base_ids,
           0::bigint AS chunk_count
      FROM "Document" d
     WHERE d."tenantId" = $1 AND d."deletedAt" IS NULL
       AND ($3::text IS NULL OR d."externalRef" = $3)
       AND ($4::timestamp IS NULL OR (d."createdAt", d.id) < ($4::timestamp, $5::text))
        AND (cardinality(d."aclPrincipals") = 0 OR d."aclPrincipals" && $6::text[])
     ORDER BY d."createdAt" DESC, d.id DESC
     LIMIT $2
"#;

/// The same page, restricted to documents in one of the caller's knowledge
/// bases. The parameters are the tenant, the knowledge bases, the limit, the
/// external reference and the cursor.
pub const SELECT_DOCS_WITH_KB: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."externalRef" AS external_ref,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
           d."s3Key" AS s3_key,
           d."sha256" AS sha256,
           d."docKey" AS doc_key,
           d."version" AS version,
           d."supersedesDocumentId" AS supersedes_document_id,
           to_char(d."effectiveFrom" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_from,
           to_char(d."effectiveTo" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS effective_to,
           d."isLatest" AS is_latest,
           d."aclPrincipals" AS acl_principals,
           d.metadata,
           to_char(d."createdAt" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
           d."createdAt" AS created_at_ts,
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
       AND ($5::timestamp IS NULL OR (d."createdAt", d.id) < ($5::timestamp, $6::text))
        AND (cardinality(d."aclPrincipals") = 0 OR d."aclPrincipals" && $7::text[])
     ORDER BY d."createdAt" DESC, d.id DESC
     LIMIT $3
"#;

/// How many documents match the filter — the whole result set, not the page.
///
/// The cursor is not applied: `total` is the size of the set a caller walks
/// through, so it is the same on every page.
pub const COUNT_DOCS_TENANT_ONLY: &str = r#"
    SELECT COUNT(*) FROM "Document" d
     WHERE d."tenantId" = $1 AND d."deletedAt" IS NULL
       AND ($2::text IS NULL OR d."externalRef" = $2)
       AND (cardinality(d."aclPrincipals") = 0 OR d."aclPrincipals" && $3::text[])
"#;

/// The same count, restricted to documents in one of the caller's knowledge
/// bases.
pub const COUNT_DOCS_WITH_KB: &str = r#"
    SELECT COUNT(*) FROM "Document" d
     WHERE d."tenantId" = $1
       AND d."deletedAt" IS NULL
       AND ($3::text IS NULL OR d."externalRef" = $3)
       AND EXISTS (SELECT 1 FROM "DocumentGroup" gf
                    WHERE gf."documentId" = d.id AND gf."knowledgeBaseId" = ANY($2::text[]))
       AND (cardinality(d."aclPrincipals") = 0 OR d."aclPrincipals" && $4::text[])
"#;

/// The document that already holds these bytes, if any.
///
/// The upload dedupe (`KB_UPLOAD_DEDUPE`) runs this before it stores
/// anything. Every clause is a scope clause: the tenant, the hash, and the
/// knowledge bases. A soft-deleted document and a failed one do not block an
/// upload — a document that failed to ingest is exactly the one a caller
/// wants to try again.
///
/// `$3` is the list of knowledge bases the upload is filed under. An empty
/// list means the whole tenant: the same bytes anywhere in the tenant block
/// the upload. A non-empty list means the existing document has to be filed
/// under one of them.
pub const SELECT_DUPLICATE_BY_SHA256: &str = r#"
    SELECT d.id
      FROM "Document" d
     WHERE d."tenantId" = $1
       AND d."sha256" = $2
       AND d."deletedAt" IS NULL
       AND d.status <> 'failed'
       AND (
           cardinality($3::text[]) = 0
           OR EXISTS (SELECT 1 FROM "DocumentGroup" g
                       WHERE g."documentId" = d.id
                         AND g."knowledgeBaseId" = ANY($3::text[]))
       )
     ORDER BY d."createdAt" DESC, d.id DESC
     LIMIT 1
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
    RETURNING id, "tenantId", "externalRef", "documentId", "s3Key", filename, "mimeType", attempt, params,
              (EXTRACT(EPOCH FROM ("createdAt" AT TIME ZONE 'UTC')) * 1000)::bigint AS created_at_ms
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

/// Park a job whose backend was unavailable (`#36`).
///
/// The status is `waiting_for_ocr` — a state the queue does not claim — and
/// `resumeAt` says when the job may be tried again. The attempt counter goes
/// up, so a job that keeps meeting an unavailable backend eventually fails
/// for good instead of waiting forever.
pub const PARK_JOB: &str = r#"
    UPDATE "IngestJob"
       SET status = 'waiting_for_ocr', error = $2,
           "resumeAt" = to_timestamp($3::double precision / 1000.0),
           attempt = attempt + 1, "updatedAt" = now()
     WHERE id = $1
       AND status = 'processing'
"#;

/// Move the parked jobs whose wait is over back to `pending` (`#36`).
///
/// The worker calls this before it claims a job, so a job that waited for an
/// OCR provider comes back on its own. The attempt counter is *not* touched:
/// it went up when the job was parked, and it is what stops a job from
/// waiting forever.
pub const RESUME_PARKED_JOBS: &str = r#"
    UPDATE "IngestJob"
       SET status = 'pending', "resumeAt" = NULL, error = NULL,
           step = 'queued', progress = 0, "stepCurrent" = NULL, "stepTotal" = NULL,
           "etaSeconds" = NULL, "startedAt" = NULL, "updatedAt" = now()
     WHERE status = 'waiting_for_ocr'
       AND "resumeAt" IS NOT NULL
       AND "resumeAt" <= to_timestamp($1::double precision / 1000.0)
    RETURNING id
"#;

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
           "resumeAt" = NULL, step = 'queued', progress = 0, "updatedAt" = now()
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
        "s3Key", "fileSize", "mimeType", status, "externalRef", metadata,
        sha256, "docKey", "version", "effectiveFrom", "effectiveTo",
        "supersedesDocumentId", "isLatest", "aclPrincipals", "createdAt", "updatedAt"
    ) VALUES (
        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
        $16::timestamptz, $17::timestamptz, $18, true, $19::text[], now(), now()
    )
"#;

/// The current version of one `docKey` in this tenant, locked for the whole
/// create transaction (`FOR UPDATE`) so two concurrent uploads cannot both
/// believe they are the latest. Undated and unversioned rows are unaffected.
pub const SELECT_LATEST_BY_DOC_KEY: &str = r#"
    SELECT id FROM "Document"
     WHERE "tenantId" = $1 AND "docKey" = $2
       AND "isLatest" = true AND "deletedAt" IS NULL
     ORDER BY "createdAt" DESC, id DESC
     LIMIT 1 FOR UPDATE
"#;

/// Retire the previous version in the same transaction as the insert. Its
/// effective interval closes at the new version's `effectiveFrom` (or now when
/// the caller gave no date), but only when it had no `effectiveTo` already:
/// a caller-set end date must not be overwritten by an upload.
pub const RETIRE_PREVIOUS_VERSION: &str = r#"
    UPDATE "Document"
       SET "isLatest" = false,
           "effectiveTo" = COALESCE("effectiveTo", COALESCE($2::timestamptz, now())),
           "updatedAt" = now()
     WHERE id = $1 AND "tenantId" = $3
"#;

/// PATCH for the RET-16 columns. Each value is guarded by a presence boolean,
/// so an absent field leaves the column exactly as it was. The tenant is
/// checked in the WHERE clause as well as by the route's existence check.
pub const UPDATE_DOCUMENT_VERSION_PATCH: &str = r#"
    UPDATE "Document"
       SET "docKey" = CASE WHEN $2::boolean THEN $3 ELSE "docKey" END,
           "version" = CASE WHEN $4::boolean THEN $5 ELSE "version" END,
           "effectiveFrom" = CASE WHEN $6::boolean THEN $7::timestamptz ELSE "effectiveFrom" END,
           "effectiveTo" = CASE WHEN $8::boolean THEN $9::timestamptz ELSE "effectiveTo" END,
           "updatedAt" = now()
     WHERE id = $1 AND "tenantId" = $10
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
           "aclPrincipals" = CASE WHEN $8::boolean THEN $9::text[] ELSE "aclPrincipals" END,
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
           "startedAt" = NULL, "resumeAt" = NULL, "updatedAt" = now()
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

// ─── Scope outbox (SEC-2b) ───────────────────────────────────────────────────

/// Written by the document write paths inside their own transaction.
pub const INSERT_SCOPE_OUTBOX: &str = r#"
    INSERT INTO "ScopeOutbox" ("documentId", "tenantId", op)
    VALUES ($1, $2, $3)
"#;

/// Documents whose membership changes when a knowledge base is deleted. The
/// rows are selected before the links are removed, still inside the same
/// transaction, so every affected document gets exactly one repair row.
pub const SELECT_KB_MEMBER_DOCUMENTS: &str = r#"
    SELECT DISTINCT "documentId" FROM "DocumentGroup" WHERE "knowledgeBaseId" = $1
"#;

/// Claim the oldest claimable rows. `$1` is the retry ceiling, `$2` the backoff
/// step in seconds and `$3` the batch size.
///
/// `attempts` is incremented by the claim itself, not by `fail`: a worker that
/// dies after claiming a row must not hold it forever. Rows already taken by
/// another worker are skipped, not waited on, so several replicas can drain
/// one table. The backoff grows linearly with the attempt count — with no extra
/// column, `createdAt` is the only clock the schema has.
pub const CLAIM_SCOPE_OUTBOX: &str = r#"
    UPDATE "ScopeOutbox" SET "attempts" = "attempts" + 1
     WHERE "id" IN (
         SELECT "id" FROM "ScopeOutbox"
          WHERE "attempts" < $1::integer
            AND "createdAt" <= now() - ("attempts" * make_interval(secs => $2::double precision))
          ORDER BY "createdAt", "id"
          LIMIT $3::integer
            FOR UPDATE SKIP LOCKED
     )
    RETURNING "id", "documentId", "tenantId", "op", "attempts"
"#;

pub const COMPLETE_SCOPE_OUTBOX: &str = r#"
    DELETE FROM "ScopeOutbox" WHERE "id" = ANY($1::bigint[])
"#;

pub const FAIL_SCOPE_OUTBOX: &str = r#"
    UPDATE "ScopeOutbox" SET "lastError" = $2 WHERE "id" = $1
"#;

pub const SCOPE_OUTBOX_LAG: &str = r#"
    SELECT count(*)::bigint AS pending,
           extract(epoch FROM (now() - min("createdAt")))::double precision AS oldest_age
      FROM "ScopeOutbox"
"#;

/// Every tenant with documents, for `kb-server scope backfill` without
/// `--tenant`. Soft-deleted rows are excluded: there is nothing to repair for
/// a document that no longer exists.
pub const SELECT_BACKFILL_TENANTS: &str = r#"
    SELECT DISTINCT "tenantId" FROM "Document" WHERE "deletedAt" IS NULL ORDER BY "tenantId"
"#;
