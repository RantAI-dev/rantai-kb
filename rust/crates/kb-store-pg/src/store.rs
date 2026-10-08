//! Postgres implementation of `DocumentStore`.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    AuthContext, DocumentFilter, DocumentId, DocumentMeta, DocumentScope, DocumentStore,
    KnowledgeBaseId, KnowledgeBaseSummary, PortError, ScopeRequest, TenantId, adapter::ScopeToken,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use crate::auth::resolve_api_key_query;
use crate::documents::row_to_doc_meta;
use crate::queries::{
    MAX_LIMIT, SELECT_ALIVE_BY_IDS, SELECT_BY_ID, SELECT_DOCS_TENANT_ONLY, SELECT_DOCS_WITH_KB,
    SELECT_KNOWLEDGE_BASES,
};

/// Postgres pool settings.
#[derive(Clone, Debug)]
pub struct PgSettings {
    pub database_url: String,
    pub max_connections: u32,
    pub connect_timeout: Duration,
    pub acquire_timeout: Duration,
}

impl Default for PgSettings {
    fn default() -> Self {
        Self {
            database_url: String::new(),
            max_connections: 10,
            connect_timeout: Duration::from_secs(5),
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

/// Postgres-backed document store.
#[derive(Clone)]
pub struct PgDocumentStore {
    pool: PgPool,
}

impl PgDocumentStore {
    /// Construct from an existing sqlx pool.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Construct a store from settings, connecting to Postgres.
    pub async fn new(settings: PgSettings) -> Result<Self, PortError> {
        Self::connect(&settings).await
    }

    /// Construct a store from settings reference, connecting to Postgres.
    pub async fn connect(settings: &PgSettings) -> Result<Self, PortError> {
        let pool = PgPoolOptions::new()
            .max_connections(settings.max_connections)
            .acquire_timeout(settings.acquire_timeout)
            .connect(&settings.database_url)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg connect: {e}")))?;
        Ok(Self { pool })
    }

    /// Exposed for tests that need to query the test DB directly.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn resolve_filter(
        &self,
        tenant: &TenantId,
        filter: &DocumentFilter,
    ) -> Result<Vec<DocumentId>, PortError> {
        let mut query = String::from(
            r#"
            SELECT DISTINCT d.id
              FROM "Document" d
         LEFT JOIN "DocumentGroup" dg ON dg."documentId" = d.id
             WHERE d."tenantId" = $1
               AND d."deletedAt" IS NULL
        "#,
        );

        if !filter.knowledge_base_ids.is_empty() {
            query.push_str(" AND dg.\"knowledgeBaseId\" = ANY($2::text[])");
        }
        if filter.category.is_some() {
            let cat_param = if filter.knowledge_base_ids.is_empty() {
                "$2"
            } else {
                "$3"
            };
            query.push_str(&format!(" AND {cat_param} = ANY(d.categories)"));
        }

        let mut q = sqlx::query(&query).bind(tenant.0.as_str());
        let kb_str_ids: Vec<&str> = filter
            .knowledge_base_ids
            .iter()
            .map(|k| k.0.as_str())
            .collect();

        if !filter.knowledge_base_ids.is_empty() {
            q = q.bind(&kb_str_ids);
        }
        if let Some(cat) = &filter.category {
            q = q.bind(cat.as_str());
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg scope: {e}")))?;

        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("pg scope id: {e}")))?;
            ids.push(DocumentId(id));
        }
        Ok(ids)
    }
}

#[async_trait]
impl DocumentStore for PgDocumentStore {
    async fn build_scope(&self, req: &ScopeRequest) -> Result<DocumentScope, PortError> {
        let ids = self.resolve_filter(&req.tenant, &req.filter).await?;
        let token = ScopeToken::new_for_adapter();
        Ok(DocumentScope::new(&token, req.tenant.clone(), ids))
    }

    async fn find_by_id(
        &self,
        scope: &DocumentScope,
        id: &DocumentId,
    ) -> Result<Option<DocumentMeta>, PortError> {
        if !scope.document_ids().iter().any(|d| d == id) {
            return Ok(None);
        }
        let row = sqlx::query(SELECT_BY_ID)
            .bind(id.0.as_str())
            .bind(scope.tenant().0.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg find_by_id: {e}")))?;
        match row {
            Some(row) => {
                Ok(Some(row_to_doc_meta(&row).map_err(|e| {
                    PortError::Unavailable(format!("pg row: {e}"))
                })?))
            }
            None => Ok(None),
        }
    }

    async fn soft_delete(&self, tenant: &TenantId, id: &DocumentId) -> Result<bool, PortError> {
        let res = sqlx::query(
            r#"UPDATE "Document" SET "deletedAt" = NOW()
               WHERE id = $1 AND "tenantId" = $2 AND "deletedAt" IS NULL"#,
        )
        .bind(id.0.as_str())
        .bind(tenant.0.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg delete: {e}")))?;
        Ok(res.rows_affected() > 0)
    }

    async fn hard_delete(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<Option<String>>, PortError> {
        // DocumentGroup rows cascade on delete (see the init migration).
        let row = sqlx::query(
            r#"DELETE FROM "Document" WHERE id = $1 AND "tenantId" = $2 RETURNING "s3Key""#,
        )
        .bind(id.0.as_str())
        .bind(tenant.0.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg delete: {e}")))?;
        match row {
            None => Ok(None),
            Some(r) => {
                let s3_key: Option<String> = r
                    .try_get("s3Key")
                    .map_err(|e| PortError::Unavailable(format!("pg delete: {e}")))?;
                Ok(Some(s3_key))
            }
        }
    }

    async fn find_alive_meta_by_ids(
        &self,
        tenant: &TenantId,
        document_ids: &[DocumentId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<&str> = document_ids.iter().map(|d| d.0.as_str()).collect();
        let rows = sqlx::query(SELECT_ALIVE_BY_IDS)
            .bind(tenant.0.as_str())
            .bind(&ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg find_alive_meta_by_ids: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(
                row_to_doc_meta(row).map_err(|e| PortError::Unavailable(format!("pg row: {e}")))?,
            );
        }
        Ok(out)
    }

    async fn list(
        &self,
        tenant: &TenantId,
        limit: u32,
        knowledge_base_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<DocumentMeta>, PortError> {
        let limit = (limit.clamp(1, MAX_LIMIT)) as i64;

        let rows = if knowledge_base_ids.is_empty() {
            sqlx::query(SELECT_DOCS_TENANT_ONLY)
                .bind(tenant.0.as_str())
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        } else {
            let ids: Vec<&str> = knowledge_base_ids.iter().map(|k| k.0.as_str()).collect();
            sqlx::query(SELECT_DOCS_WITH_KB)
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
                row_to_doc_meta(row).map_err(|e| PortError::Unavailable(format!("pg row: {e}")))?,
            );
        }
        Ok(out)
    }

    async fn list_knowledge_bases(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<KnowledgeBaseSummary>, PortError> {
        let rows = sqlx::query(SELECT_KNOWLEDGE_BASES)
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

    async fn resolve_api_key(&self, plaintext_key: &str) -> Result<Option<AuthContext>, PortError> {
        resolve_api_key_query(&self.pool, plaintext_key).await
    }
}
