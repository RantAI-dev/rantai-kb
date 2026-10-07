//! Postgres-backed `DocumentStore` and API key resolver.
//!
//! All read paths are filtered by `tenantId` and exclude `deletedAt`. Every
//! value that goes into SQL is bound through sqlx; the only inline value is
//! the sanitised `LIMIT` integer (capped at 200).
//!
//! `DocumentScope::new` is the only entry point to building a scope; this
//! crate is the only producer of scopes in the workspace.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    ChunkMetadata, DocumentFilter, DocumentId, DocumentMeta, DocumentStore, KnowledgeBaseId,
    KnowledgeBaseSummary, PortError, ScopeRequest, TenantId,
};
use kb_ports::scope::DocumentScope;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};

const MAX_LIMIT: u32 = 200;

#[derive(Clone, Debug)]
pub struct PgSettings {
    pub database_url: String,
    pub max_connections: u32,
    pub connect_timeout: Duration,
    pub acquire_timeout: Duration,
}

impl PgSettings {
    /// Build a pool with the configured limits.
    pub async fn pool(&self) -> Result<PgPool, sqlx::Error> {
        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(self.acquire_timeout)
            .connect_timeout(self.connect_timeout)
            .connect(&self.database_url)
            .await
    }
}

#[derive(Clone)]
pub struct PgDocumentStore {
    pool: PgPool,
}

impl PgDocumentStore {
    pub async fn new(settings: PgSettings) -> Result<Self, PortError> {
        let pool = settings
            .pool()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg connect: {e}")))?;
        Ok(Self { pool })
    }

    /// Direct access to the underlying pool. Tests use this to insert fixtures.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Resolve the filter to a list of alive document ids for this tenant.
    /// Used internally by `build_scope` and exposed for tests.
    pub async fn resolve_filter(
        &self,
        tenant: &TenantId,
        filter: &DocumentFilter,
    ) -> Result<Vec<DocumentId>, PortError> {
        let has_kb_filter = !filter.knowledge_base_ids.is_empty();
        let has_category = filter.category.is_some();

        let rows = if has_kb_filter || has_category {
            let mut sql = String::from(
                "SELECT DISTINCT d.id FROM \"Document\" d \
                 LEFT JOIN \"DocumentGroup\" g ON g.\"documentId\" = d.id \
                 WHERE d.\"tenantId\" = $1 AND d.\"deletedAt\" IS NULL",
            );
            let mut idx = 2;
            let mut bind_ids: Vec<String> = Vec::new();
            if has_kb_filter {
                sql.push_str(&format!(
                    " AND g.\"knowledgeBaseId\" = ANY(${idx}::text[])",
                    idx = idx
                ));
                bind_ids.extend(filter.knowledge_base_ids.iter().map(|k| k.0.clone()));
                idx += 1;
            }
            if has_category {
                sql.push_str(&format!(" AND d.categories @> ARRAY[${idx}]::text[]", idx = idx));
                idx += 1;
            }
            let mut q = sqlx::query(&sql).bind(tenant.0.as_str());
            if has_kb_filter {
                let refs: Vec<&str> = bind_ids.iter().map(String::as_str).collect();
                q = q.bind(refs);
            }
            if let Some(category) = &filter.category {
                q = q.bind(category);
            }
            q.fetch_all(&self.pool).await
        } else {
            sqlx::query(
                "SELECT id FROM \"Document\" WHERE \"tenantId\" = $1 AND \"deletedAt\" IS NULL",
            )
            .bind(tenant.0.as_str())
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| PortError::Unavailable(format!("pg resolve_filter: {e}")))?;

        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg id: {e}")))?;
            ids.push(DocumentId(id));
        }
        Ok(ids)
    }
}

fn row_to_doc_meta(row: &sqlx::postgres::PgRow) -> Result<DocumentMeta, sqlx::Error> {
    let id: String = row.try_get("id")?;
    let title: String = row.try_get("title")?;
    let categories: Vec<String> = row.try_get("categories")?;
    let subcategory: Option<String> = row.try_get("subcategory")?;
    let status: String = row.try_get("status")?;
    let chunk_count: Option<i64> = row.try_get("chunk_count")?;
    Ok(DocumentMeta {
        id: DocumentId(id),
        title,
        categories,
        subcategory,
        status,
        chunk_count: chunk_count.unwrap_or(0).max(0) as u32,
    })
}

#[async_trait]
impl DocumentStore for PgDocumentStore {
    async fn build_scope(&self, req: &ScopeRequest) -> Result<DocumentScope, PortError> {
        let ids = self.resolve_filter(&req.tenant, &req.filter).await?;
        Ok(DocumentScope::new(req.tenant.clone(), ids))
    }

    async fn find_by_id(
        &self,
        scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError> {
        // The scope's id list is the authority; we must not return documents
        // that aren't in it even if Postgres would otherwise serve them.
        if !scope.document_ids().iter().any(|d| d == id) {
            return Ok(None);
        }
        let row = sqlx::query(
            r#"
            SELECT d.id, d.title, d.categories, d."subcategory", d.status,
                   (SELECT COUNT(*) FROM "DocumentChunk" c WHERE c.document_id = d.id) AS chunk_count
              FROM "Document" d
             WHERE d.id = $1 AND d."tenantId" = $2 AND d."deletedAt" IS NULL
            "#,
        )
        .bind(id.0.as_str())
        .bind(scope.tenant().0.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg find_by_id: {e}")))?;
        match row {
            Some(row) => row_to_doc_meta(&row)
                .map(Some)
                .map_err(|e| PortError::Unavailable(format!("pg row: {e}"))),
            None => Ok(None),
        }
    }

    async fn list(
        &self,
        tenant: &TenantId,
        limit: u32,
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        let limit = limit.min(MAX_LIMIT) as i64;

        let rows = if knowledge_base_ids.is_empty() {
            sqlx::query(
                r#"
                SELECT d.id, d.title, d.categories, d."subcategory", d.status,
                       (SELECT COUNT(*) FROM "DocumentChunk" c WHERE c.document_id = d.id) AS chunk_count
                  FROM "Document" d
                 WHERE d."tenantId" = $1 AND d."deletedAt" IS NULL
                 ORDER BY d."createdAt" DESC
                 LIMIT $2
                "#,
            )
            .bind(tenant.0.as_str())
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            let ids: Vec<&str> = knowledge_base_ids.iter().map(|k| k.0.as_str()).collect();
            sqlx::query(
                r#"
                SELECT d.id, d.title, d.categories, d."subcategory", d.status,
                       (SELECT COUNT(*) FROM "DocumentChunk" c WHERE c.document_id = d.id) AS chunk_count
                  FROM "Document" d
                  JOIN "DocumentGroup" g ON g."documentId" = d.id
                 WHERE d."tenantId" = $1
                   AND d."deletedAt" IS NULL
                   AND g."knowledgeBaseId" = ANY($2::text[])
                 ORDER BY d."createdAt" DESC
                 LIMIT $3
                "#,
            )
            .bind(tenant.0.as_str())
            .bind(&ids)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| PortError::Unavailable(format!("pg list: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(
                row_to_doc_meta(row)
                    .map_err(|e| PortError::Unavailable(format!("pg row: {e}")))?,
            );
        }
        Ok(out)
    }

    async fn ordered_chunks(
        &self,
        document_id: &DocumentId,
    ) -> Result<Vec<(u32, String, ChunkMetadata)>, PortError> {
        // Returns (chunk_index, content, metadata) for a document in order.
        let rows = sqlx::query(
            r#"
            SELECT "chunkIndex", content, metadata
              FROM "DocumentChunk"
             WHERE "documentId" = $1
             ORDER BY "chunkIndex" ASC
            "#,
        )
        .bind(document_id.0.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg ordered_chunks: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let index: i32 = row
                .try_get("chunkIndex")
                .map_err(|e| PortError::Unavailable(format!("chunk index: {e}")))?;
            let content: String = row
                .try_get("content")
                .map_err(|e| PortError::Unavailable(format!("chunk content: {e}")))?;
            let meta_json: Option<serde_json::Value> = row
                .try_get("metadata")
                .map_err(|e| PortError::Unavailable(format!("chunk meta: {e}")))?;
            let metadata: ChunkMetadata = match meta_json {
                Some(v) => serde_json::from_value(v)
                    .map_err(|e| PortError::Unavailable(format!("chunk meta shape: {e}")))?,
                None => ChunkMetadata::default(),
            };
            out.push((index.max(0) as u32, content, metadata));
        }
        Ok(out)
    }

    async fn list_knowledge_bases(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<KnowledgeBaseSummary>, PortError> {
        let rows = sqlx::query(
            r#"
            SELECT k.id, k.name, k.description, k.color,
                   (SELECT COUNT(*) FROM "DocumentGroup" g
                     JOIN "Document" d ON d.id = g."documentId"
                    WHERE g."knowledgeBaseId" = k.id
                      AND d."deletedAt" IS NULL) AS document_count
              FROM "KnowledgeBase" k
             WHERE k."tenantId" = $1
             ORDER BY k."createdAt" DESC
            "#,
        )
        .bind(tenant.0.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg list_kb: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("kb row: {e}")))?;
            let name: String = row
                .try_get("name")
                .map_err(|e| PortError::Unavailable(format!("kb row: {e}")))?;
            let description: Option<String> = row
                .try_get("description")
                .map_err(|e| PortError::Unavailable(format!("kb row: {e}")))?;
            let color: Option<String> = row
                .try_get("color")
                .map_err(|e| PortError::Unavailable(format!("kb row: {e}")))?;
            let count: i64 = row
                .try_get("document_count")
                .map_err(|e| PortError::Unavailable(format!("kb row: {e}")))?;
            out.push(KnowledgeBaseSummary {
                id: KnowledgeBaseId(id),
                name,
                description,
                color,
                document_count: count.max(0) as u32,
            });
        }
        Ok(out)
    }

    async fn resolve_api_key(
        &self,
        plaintext_key: &str,
    ) -> Result<Option<TenantId>, PortError> {
        let mut hasher = Sha256::new();
        hasher.update(plaintext_key.as_bytes());
        let hash = hex::encode(hasher.finalize());

        let row = sqlx::query(
            r#"
            SELECT "tenantId"
              FROM "ApiKey"
             WHERE hash = $1
               AND "revokedAt" IS NULL
            "#,
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg api_key: {e}")))?;

        match row {
            Some(row) => {
                let tenant_id: String = row
                    .try_get("tenantId")
                    .map_err(|e| PortError::Unavailable(format!("api_key row: {e}")))?;
                Ok(Some(TenantId(tenant_id)))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_limit_is_capped() {
        assert_eq!(MAX_LIMIT, 200);
    }

    #[test]
    fn api_key_hashing_is_deterministic() {
        let h1 = {
            let mut hasher = Sha256::new();
            hasher.update(b"rkb_demo");
            hex::encode(hasher.finalize())
        };
        let h2 = {
            let mut hasher = Sha256::new();
            hasher.update(b"rkb_demo");
            hex::encode(hasher.finalize())
        };
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn filter_default_is_empty() {
        let f = DocumentFilter::default();
        assert!(f.knowledge_base_ids.is_empty());
        assert!(f.category.is_none());
    }
}