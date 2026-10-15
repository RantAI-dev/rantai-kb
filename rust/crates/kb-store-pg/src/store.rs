//! Postgres implementation of `DocumentStore`.

use std::time::Duration;

use async_trait::async_trait;
use kb_ports::{
    AuthContext, CategoryRecord, CreateCategory, CreateKnowledgeBase, DocumentDownloadMeta,
    DocumentFilter, DocumentId, DocumentMeta, DocumentScope, DocumentStore, KnowledgeBaseId,
    KnowledgeBaseRecord, KnowledgeBaseSummary, NewDocument, PatchCategory, PatchDocument,
    PatchKnowledgeBase, PortError, ScopeRequest, TenantId, adapter::ScopeToken,
};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};

use crate::auth::resolve_api_key_query;
use crate::documents::row_to_doc_meta;
use crate::queries::{
    DELETE_CATEGORY, DELETE_DOCUMENT_GROUPS, DELETE_KB_GROUPS, DELETE_KNOWLEDGE_BASE,
    INSERT_CATEGORY, INSERT_DOCUMENT, INSERT_DOCUMENT_GROUP, INSERT_KNOWLEDGE_BASE, MAX_LIMIT,
    SELECT_ALIVE_BY_IDS, SELECT_BY_ID, SELECT_CATEGORIES, SELECT_CATEGORY_BY_ID_TENANT,
    SELECT_DOCS_TENANT_ONLY, SELECT_DOCS_WITH_KB, SELECT_EXISTS_FOR_PATCH, SELECT_FOR_DOWNLOAD,
    SELECT_KB_BY_ID_TENANT, SELECT_KNOWLEDGE_BASES, SELECT_OWNED_KB_IDS, SET_DOCUMENT_STATUS,
    UPDATE_CATEGORY, UPDATE_DOCUMENT_PATCH, UPDATE_EXTRACTED_CONTENT, UPDATE_KNOWLEDGE_BASE,
    UPDATE_METADATA,
};

/// Open a connection pool from settings. A free function rather than a
/// constructor on `sqlx::Pool`, so one process can share a single pool between
/// the document store and the job store.
///
/// # Errors
///
/// Fails when the pool cannot be opened, naming the driver error.
pub async fn connect_pool(settings: &PgSettings) -> Result<PgPool, PortError> {
    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .connect(&settings.database_url)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg connect: {e}")))
}

/// Postgres-backed document store./// Read one `KnowledgeBase` row, as returned by the statements that alias their
/// timestamps to snake_case names.
fn kb_record(row: &PgRow) -> Result<KnowledgeBaseRecord, PortError> {
    Ok(KnowledgeBaseRecord {
        id: KnowledgeBaseId(text(row, "id")?),
        tenant_id: TenantId(text(row, "tenantId")?),
        name: text(row, "name")?,
        description: opt_text(row, "description")?,
        color: opt_text(row, "color")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
    })
}

/// Read one `Category` row.
fn category_record(row: &PgRow) -> Result<CategoryRecord, PortError> {
    Ok(CategoryRecord {
        id: text(row, "id")?,
        name: text(row, "name")?,
        label: text(row, "label")?,
        color: opt_text(row, "color")?,
    })
}

fn text(row: &PgRow, column: &str) -> Result<String, PortError> {
    row.try_get(column)
        .map_err(|e| PortError::Unavailable(format!("pg {column}: {e}")))
}

fn opt_text(row: &PgRow, column: &str) -> Result<Option<String>, PortError> {
    row.try_get(column)
        .map_err(|e| PortError::Unavailable(format!("pg {column}: {e}")))
}

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
        Ok(Self::from_pool(connect_pool(settings).await?))
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

    async fn update_extracted_content(
        &self,
        id: &DocumentId,
        content: &str,
        file_type: Option<&str>,
        file_size: Option<i32>,
    ) -> Result<(), PortError> {
        // The statement is built with only bound parameters, so a document with
        // no extracted `fileType` clears the column rather than writing NULL
        // into a nullable-but-meaningful field.
        sqlx::query(UPDATE_EXTRACTED_CONTENT)
            .bind(id.as_str())
            .bind(content)
            .bind(file_type)
            .bind(file_size)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg update content: {e}")))?;
        Ok(())
    }

    async fn update_metadata(
        &self,
        id: &DocumentId,
        patch: &serde_json::Value,
    ) -> Result<(), PortError> {
        sqlx::query(UPDATE_METADATA)
            .bind(id.as_str())
            .bind(patch)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg update_metadata: {e}")))?;
        Ok(())
    }

    async fn set_status(&self, id: &DocumentId, status: &str) -> Result<(), PortError> {
        sqlx::query(SET_DOCUMENT_STATUS)
            .bind(id.as_str())
            .bind(status)
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg set_status: {e}")))?;
        Ok(())
    }

    async fn resolve_api_key(&self, plaintext_key: &str) -> Result<Option<AuthContext>, PortError> {
        resolve_api_key_query(&self.pool, plaintext_key).await
    }

    async fn health_check(&self) -> Result<(), PortError> {
        sqlx::query("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map(|_| ())
            .map_err(|e| PortError::Unavailable(format!("pg health check: {e}")))
    }

    async fn create_document(
        &self,
        doc: &NewDocument,
        group_kb_ids: &[KnowledgeBaseId],
    ) -> Result<(), PortError> {
        // The row and its links land together: a document with no group row is
        // invisible to every knowledge-base-scoped query, so a half-written
        // upload must not survive.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx begin: {e}")))?;

        sqlx::query(INSERT_DOCUMENT)
            .bind(doc.id.as_str())
            .bind(doc.tenant_id.as_str())
            .bind(&doc.title)
            .bind(&doc.content)
            .bind(&doc.categories)
            .bind(doc.subcategory.as_deref())
            .bind(doc.s3_key.as_deref())
            .bind(doc.file_size)
            .bind(doc.mime_type.as_deref())
            .bind(&doc.status)
            .bind(doc.external_ref.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg insert document: {e}")))?;

        for kb_id in group_kb_ids {
            sqlx::query(INSERT_DOCUMENT_GROUP)
                .bind(new_group_id())
                .bind(doc.id.as_str())
                .bind(kb_id.0.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg insert document group: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx commit: {e}")))?;
        Ok(())
    }

    async fn patch_document(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
        patch: &PatchDocument,
    ) -> Result<Option<DocumentMeta>, PortError> {
        // Tenant-scoped existence check first: every statement below addresses
        // the row by id alone, exactly as the TypeScript route does after its
        // own `findFirst({ id, tenantId })`.
        let exists = sqlx::query(SELECT_EXISTS_FOR_PATCH)
            .bind(id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg check exists for patch: {e}")))?;
        if exists.is_none() {
            return Ok(None);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx begin: {e}")))?;

        if patch.title.is_some()
            || patch.categories.is_some()
            || patch.subcategory.is_some()
        {
            let no_categories: Vec<String> = Vec::new();
            sqlx::query(UPDATE_DOCUMENT_PATCH)
                .bind(id.as_str())
                .bind(patch.title.is_some())
                .bind(patch.title.as_deref().unwrap_or_default())
                .bind(patch.categories.is_some())
                .bind(patch.categories.as_deref().unwrap_or(&no_categories))
                .bind(patch.subcategory.is_some())
                .bind(patch.subcategory.as_ref().and_then(|s| s.as_deref()))
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg update document patch: {e}")))?;
        }

        // `knowledgeBaseIds` is replace-not-merge: absent leaves the links
        // untouched, `[]` clears them.
        if let Some(kb_ids) = &patch.knowledge_base_ids {
            sqlx::query(DELETE_DOCUMENT_GROUPS)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(|e| PortError::Unavailable(format!("pg delete document groups: {e}")))?;
            for kb_id in kb_ids {
                sqlx::query(INSERT_DOCUMENT_GROUP)
                    .bind(new_group_id())
                    .bind(id.as_str())
                    .bind(kb_id.0.as_str())
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| PortError::Unavailable(format!("pg insert document group: {e}")))?;
            }
        }

        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx commit: {e}")))?;

        let updated = sqlx::query(SELECT_BY_ID)
            .bind(id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg fetch updated document: {e}")))?;
        match updated {
            Some(row) => row_to_doc_meta(&row)
                .map(Some)
                .map_err(|e| PortError::Unavailable(format!("pg row: {e}"))),
            None => Ok(None),
        }
    }

    async fn find_for_download(
        &self,
        tenant: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<DocumentDownloadMeta>, PortError> {
        let row = sqlx::query(SELECT_FOR_DOWNLOAD)
            .bind(id.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg find_for_download: {e}")))?;
        match row {
            Some(row) => Ok(Some(DocumentDownloadMeta {
                id: DocumentId::new_unchecked(text(&row, "id")?),
                s3_key: opt_text(&row, "s3_key")?,
                mime_type: opt_text(&row, "mime_type")?,
                title: text(&row, "title")?,
            })),
            None => Ok(None),
        }
    }

    async fn filter_owned_kb_ids(
        &self,
        tenant: &TenantId,
        kb_ids: &[KnowledgeBaseId],
    ) -> Result<Vec<KnowledgeBaseId>, PortError> {
        if kb_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<&str> = kb_ids.iter().map(|k| k.0.as_str()).collect();
        let rows = sqlx::query(SELECT_OWNED_KB_IDS)
            .bind(tenant.as_str())
            .bind(&ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg filter_owned_kb_ids: {e}")))?;
        rows.iter()
            .map(|row| Ok(KnowledgeBaseId(text(row, "id")?)))
            .collect()
    }

    async fn create_knowledge_base(
        &self,
        tenant: &TenantId,
        input: &CreateKnowledgeBase,
    ) -> Result<KnowledgeBaseRecord, PortError> {
        let row = sqlx::query(INSERT_KNOWLEDGE_BASE)
            .bind(input.id.0.as_str())
            .bind(tenant.as_str())
            .bind(&input.name)
            .bind(input.description.as_deref())
            .bind(input.color.as_deref())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg insert kb: {e}")))?;
        kb_record(&row)
    }

    async fn patch_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
        input: &PatchKnowledgeBase,
    ) -> Result<Option<KnowledgeBaseRecord>, PortError> {
        let existing = sqlx::query(SELECT_KB_BY_ID_TENANT)
            .bind(id.0.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select kb for patch: {e}")))?;
        if existing.is_none() {
            return Ok(None);
        }

        let row = sqlx::query(UPDATE_KNOWLEDGE_BASE)
            .bind(id.0.as_str())
            .bind(tenant.as_str())
            .bind(input.name.is_some())
            .bind(input.name.as_deref().unwrap_or_default())
            .bind(input.description.is_some())
            .bind(input.description.as_ref().and_then(|d| d.as_deref()))
            .bind(input.color.is_some())
            .bind(input.color.as_ref().and_then(|c| c.as_deref()))
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg update kb: {e}")))?;
        Ok(Some(kb_record(&row)?))
    }

    async fn delete_knowledge_base(
        &self,
        tenant: &TenantId,
        id: &KnowledgeBaseId,
    ) -> Result<bool, PortError> {
        let existing = sqlx::query(SELECT_KB_BY_ID_TENANT)
            .bind(id.0.as_str())
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select kb for delete: {e}")))?;
        if existing.is_none() {
            return Ok(false);
        }

        // Links and base together: Document has no relation to KnowledgeBase,
        // so nothing reachable from here can delete a document.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx begin: {e}")))?;
        sqlx::query(DELETE_KB_GROUPS)
            .bind(id.0.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete kb groups: {e}")))?;
        sqlx::query(DELETE_KNOWLEDGE_BASE)
            .bind(id.0.as_str())
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete kb: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| PortError::Unavailable(format!("pg tx commit: {e}")))?;
        Ok(true)
    }

    async fn list_categories(&self, tenant: &TenantId) -> Result<Vec<CategoryRecord>, PortError> {
        let rows = sqlx::query(SELECT_CATEGORIES)
            .bind(tenant.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg list categories: {e}")))?;
        rows.iter().map(category_record).collect()
    }

    async fn create_category(
        &self,
        tenant: &TenantId,
        input: &CreateCategory,
    ) -> Result<CategoryRecord, PortError> {
        let row = sqlx::query(INSERT_CATEGORY)
            .bind(&input.id)
            .bind(tenant.as_str())
            .bind(&input.name)
            .bind(&input.label)
            .bind(input.color.as_deref())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg insert category: {e}")))?;
        category_record(&row)
    }

    async fn patch_category(
        &self,
        tenant: &TenantId,
        id: &str,
        input: &PatchCategory,
    ) -> Result<Option<CategoryRecord>, PortError> {
        let existing = sqlx::query(SELECT_CATEGORY_BY_ID_TENANT)
            .bind(id)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select category for patch: {e}")))?;
        if existing.is_none() {
            return Ok(None);
        }

        let row = sqlx::query(UPDATE_CATEGORY)
            .bind(id)
            .bind(tenant.as_str())
            .bind(input.name.is_some())
            .bind(input.name.as_deref().unwrap_or_default())
            .bind(input.label.is_some())
            .bind(input.label.as_deref().unwrap_or_default())
            .bind(input.color.is_some())
            .bind(input.color.as_ref().and_then(|c| c.as_deref()))
            .fetch_one(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg update category: {e}")))?;
        Ok(Some(category_record(&row)?))
    }

    async fn delete_category(&self, tenant: &TenantId, id: &str) -> Result<bool, PortError> {
        let existing = sqlx::query(SELECT_CATEGORY_BY_ID_TENANT)
            .bind(id)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg select category for delete: {e}")))?;
        if existing.is_none() {
            return Ok(false);
        }

        sqlx::query(DELETE_CATEGORY)
            .bind(id)
            .bind(tenant.as_str())
            .execute(&self.pool)
            .await
            .map_err(|e| PortError::Unavailable(format!("pg delete category: {e}")))?;
        Ok(true)
    }
}

/// Primary key for a `DocumentGroup` row. The schema defaults it, but the
/// statement binds it explicitly so the id is generated the same way by Postgres
/// and by the fakes.
fn new_group_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
