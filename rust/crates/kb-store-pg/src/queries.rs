//! SQL query templates and limits for Postgres store.

/// Largest page the document list will return.
pub const MAX_LIMIT: u32 = 200;

pub const SELECT_BY_ID: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
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

pub const SELECT_ALIVE_BY_IDS: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
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
     ORDER BY d."createdAt" DESC
     LIMIT $2
"#;

pub const SELECT_DOCS_WITH_KB: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
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
    SELECT id, "tenantId", scopes, "knowledgeBaseIds"
      FROM "ApiKey"
     WHERE hash = $1
       AND "revokedAt" IS NULL
"#;

// ─── Ingest jobs ──────────────────────────────────────────────────────────────
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
///
/// The guards make the update idempotent per sweep: a row another sweep already
/// requeued is no longer `processing`, and a job whose worker heartbeated after
/// our SELECT is no longer stale. Either way `attempt` is not incremented twice.
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
