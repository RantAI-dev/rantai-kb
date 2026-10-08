//! SQL query templates and limits for Postgres store.

pub const MAX_LIMIT: u32 = 200;

pub const SELECT_BY_ID: &str = r#"
    SELECT d.id, d.title, d.categories, d."subcategory", d.status,
           d."fileType" AS file_type, d."fileSize" AS file_size, d."mimeType" AS mime_type,
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
     ORDER BY k."createdAt" DESC
"#;

pub const SELECT_API_KEY: &str = r#"
    SELECT id, "tenantId", scopes, "knowledgeBaseIds"
      FROM "ApiKey"
     WHERE hash = $1
       AND "revokedAt" IS NULL
"#;
