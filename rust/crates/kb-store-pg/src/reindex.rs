//! Reading the documents `kb-server reindex` has to queue.
//!
//! Reindex does not move chunks: the originals are in S3 and the document rows
//! are in Postgres, so the rebuild is "queue an ingest job per document and let
//! the existing worker do the work". This module owns the one query that reads
//! those rows, and the pure grouping/counting that `--dry-run` prints.
//!
//! Soft-deleted rows are excluded: a deleted document must not come back to
//! life in Qdrant because an operator rebuilt the index.

use std::collections::BTreeMap;

use kb_ports::{DocumentId, PortError, TenantId};
use sqlx::{PgPool, Row};

/// One live document, plus whether an ingest job is already in flight for it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReindexDocument {
    pub tenant_id: TenantId,
    pub document_id: DocumentId,
    pub title: String,
    pub categories: Vec<String>,
    pub subcategory: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub s3_key: Option<String>,
    pub file_size: Option<i32>,
    pub mime_type: Option<String>,
    /// A `pending` or `processing` job already exists for this document.
    pub active_job: bool,
}

impl ReindexDocument {
    /// Whether `reindex` should create a job for this row.
    #[must_use]
    pub fn is_queueable(&self) -> bool {
        self.s3_key.is_some() && !self.active_job
    }

    /// The filename to put on the job.
    ///
    /// The ingest worker uses it for the extractor's format routing, and the
    /// stored key ends with the sanitised upload name, so the last path segment
    /// is the best reconstruction available without the original request.
    #[must_use]
    pub fn filename(&self) -> Option<String> {
        self.s3_key
            .as_deref()
            .and_then(|key| key.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    }

    /// The ingest parameters the upload route would have written.
    ///
    /// `figureMode` and `useCombined` are not persisted on the document row;
    /// `auto` and the combined extractor are the upload defaults, and a reindex
    /// is a rebuild of the same document, not a different ingest policy.
    #[must_use]
    pub fn job_params(&self) -> serde_json::Value {
        let mut params = serde_json::json!({
            "title": self.title,
            "categories": self.categories,
            "subcategory": self.subcategory,
            "figureMode": "auto",
            "useCombined": true,
        });
        if let Some(document_type) = self
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("documentType"))
            .and_then(serde_json::Value::as_str)
        {
            params["documentType"] = serde_json::Value::String(document_type.to_string());
        }
        params
    }
}

/// What one tenant's reindex run will do, or would do under `--dry-run`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReindexTenantCount {
    /// Live documents the tenant owns.
    pub documents: usize,
    /// Documents that will get a job.
    pub queued: usize,
    /// Documents with no `s3Key`: there is no original to rebuild from.
    pub skipped_without_original: usize,
    /// Documents whose ingest is already pending or processing.
    pub skipped_active_job: usize,
}

/// Per-tenant counts for a set of rows.
#[must_use]
pub fn plan(documents: &[ReindexDocument]) -> BTreeMap<String, ReindexTenantCount> {
    let mut plan: BTreeMap<String, ReindexTenantCount> = BTreeMap::new();
    for document in documents {
        let entry = plan
            .entry(document.tenant_id.as_str().to_string())
            .or_default();
        entry.documents += 1;
        if document.s3_key.is_none() {
            entry.skipped_without_original += 1;
        } else if document.active_job {
            entry.skipped_active_job += 1;
        } else {
            entry.queued += 1;
        }
    }
    plan
}

const SELECT_REINDEX_DOCUMENTS: &str = r#"
    SELECT d.id, d."tenantId", d.title, d.categories, d.subcategory, d.metadata,
           d."s3Key", d."fileSize", d."mimeType",
           EXISTS (
               SELECT 1 FROM "IngestJob" j
                WHERE j."documentId" = d.id
                  AND j."tenantId" = d."tenantId"
                  AND j.status IN ('pending', 'processing')
           ) AS active_job
      FROM "Document" d
     WHERE d."deletedAt" IS NULL
       AND ($1::text IS NULL OR d."tenantId" = $1)
     ORDER BY d."tenantId", d.id
"#;

/// Every live document, optionally for one tenant.
///
/// # Errors
///
/// Fails when Postgres cannot answer, naming the statement.
pub async fn list_documents(
    pool: &PgPool,
    tenant: Option<&str>,
) -> Result<Vec<ReindexDocument>, PortError> {
    let rows = sqlx::query(SELECT_REINDEX_DOCUMENTS)
        .bind(tenant)
        .fetch_all(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg reindex documents: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let tenant_id: String = row
            .try_get("tenantId")
            .map_err(|e| PortError::Unavailable(format!("pg reindex tenant: {e}")))?;
        let document_id: String = row
            .try_get("id")
            .map_err(|e| PortError::Unavailable(format!("pg reindex document id: {e}")))?;
        out.push(ReindexDocument {
            tenant_id: TenantId::new_unchecked(tenant_id),
            document_id: DocumentId::new_unchecked(document_id),
            title: row
                .try_get("title")
                .map_err(|e| PortError::Unavailable(format!("pg reindex title: {e}")))?,
            categories: row.try_get("categories").unwrap_or_default(),
            subcategory: row.try_get("subcategory").unwrap_or_default(),
            metadata: row.try_get("metadata").unwrap_or_default(),
            s3_key: row
                .try_get("s3Key")
                .map_err(|e| PortError::Unavailable(format!("pg reindex s3Key: {e}")))?,
            file_size: row
                .try_get("fileSize")
                .map_err(|e| PortError::Unavailable(format!("pg reindex fileSize: {e}")))?,
            mime_type: row
                .try_get("mimeType")
                .map_err(|e| PortError::Unavailable(format!("pg reindex mimeType: {e}")))?,
            active_job: row.try_get("active_job").unwrap_or(false),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn document(tenant: &str, id: &str, s3_key: Option<&str>, active_job: bool) -> ReindexDocument {
        ReindexDocument {
            tenant_id: TenantId::new_unchecked(tenant),
            document_id: DocumentId::new_unchecked(id),
            title: id.to_string(),
            categories: Vec::new(),
            subcategory: None,
            metadata: None,
            s3_key: s3_key.map(str::to_string),
            file_size: None,
            mime_type: None,
            active_job,
        }
    }

    #[test]
    fn plan_groups_by_tenant_and_classifies_every_skip() {
        let rows = vec![
            document("a", "d1", Some("documents/a/d1/one.pdf"), false),
            document("a", "d2", None, false),
            document("a", "d3", Some("documents/a/d3/three.pdf"), true),
            document("b", "d4", Some("documents/b/d4/four.csv"), false),
        ];
        let plan = plan(&rows);
        assert_eq!(
            plan["a"],
            ReindexTenantCount {
                documents: 3,
                queued: 1,
                skipped_without_original: 1,
                skipped_active_job: 1,
            }
        );
        assert_eq!(
            plan["b"],
            ReindexTenantCount {
                documents: 1,
                queued: 1,
                skipped_without_original: 0,
                skipped_active_job: 0,
            }
        );
    }

    #[test]
    fn the_filename_is_the_last_key_segment_and_keeps_the_extension() {
        let doc = document(
            "a",
            "d1",
            Some("documents/a/d1/quarterly report.pdf"),
            false,
        );
        assert_eq!(doc.filename().as_deref(), Some("quarterly report.pdf"));
        assert_eq!(document("a", "d2", None, false).filename(), None);
        assert_eq!(
            document("a", "d3", Some("documents/a/d3/"), false).filename(),
            None,
            "a trailing slash has no filename to give the extractor"
        );
    }

    #[test]
    fn job_params_reconstruct_the_upload_defaults_and_keep_the_document_type() {
        let mut doc = document("a", "d1", Some("documents/a/d1/one.pdf"), false);
        doc.title = "One".to_string();
        doc.categories = vec!["finance".to_string()];
        doc.subcategory = Some("q3".to_string());
        doc.metadata = Some(serde_json::json!({ "documentType": "invoice" }));
        let params = doc.job_params();
        assert_eq!(params["title"], "One");
        assert_eq!(params["categories"][0], "finance");
        assert_eq!(params["subcategory"], "q3");
        assert_eq!(params["figureMode"], "auto");
        assert_eq!(params["useCombined"], true);
        assert_eq!(params["documentType"], "invoice");
    }

    #[test]
    fn the_query_excludes_soft_deleted_rows_and_only_reports_live_jobs() {
        assert!(SELECT_REINDEX_DOCUMENTS.contains(r#"d."deletedAt" IS NULL"#));
        assert!(SELECT_REINDEX_DOCUMENTS.contains("'pending', 'processing'"));
        assert!(SELECT_REINDEX_DOCUMENTS.contains(r#"d."tenantId" = $1"#));
    }
}
