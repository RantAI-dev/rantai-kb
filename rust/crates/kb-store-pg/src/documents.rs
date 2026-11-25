//! Document mapping helpers for Postgres rows.

use kb_ports::{DocumentId, DocumentMeta};
use sqlx::Row;

pub fn row_to_doc_meta(row: &sqlx::postgres::PgRow) -> Result<DocumentMeta, sqlx::Error> {
    let id: String = row.try_get("id")?;
    let title: String = row.try_get("title")?;
    let categories: Vec<String> = row.try_get("categories")?;
    let subcategory: Option<String> = row.try_get("subcategory")?;
    let status: String = row.try_get("status")?;
    let external_ref: Option<String> = row.try_get("external_ref").ok().flatten();
    let chunk_count: Option<i64> = row.try_get("chunk_count")?;
    let file_type: Option<String> = row.try_get("file_type")?;
    let file_size: Option<i32> = row.try_get("file_size")?;
    let mime_type: Option<String> = row.try_get("mime_type")?;
    let s3_key: Option<String> = row.try_get("s3_key").ok();
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let knowledge_base_ids: Vec<String> = row.try_get("knowledge_base_ids")?;
    let metadata: Option<serde_json::Value> = row.try_get("metadata")?;
    // Tolerant like `external_ref`: a statement that does not select the hash
    // yields `None` rather than failing the whole read.
    let sha256: Option<String> = row.try_get("sha256").ok().flatten();
    // RET-16: tolerant like `sha256` — a statement that predates the version
    // columns yields no version, never a failed read.
    let doc_key: Option<String> = row.try_get("doc_key").ok().flatten();
    let version: Option<String> = row.try_get("version").ok().flatten();
    let supersedes_document_id: Option<String> =
        row.try_get("supersedes_document_id").ok().flatten();
    let effective_from: Option<String> = row.try_get("effective_from").ok().flatten();
    let effective_to: Option<String> = row.try_get("effective_to").ok().flatten();
    let is_latest: bool = row.try_get("is_latest").ok().unwrap_or(true);
    // SEC-3: tolerant like the version fields — a statement that predates the
    // ACL column yields no ACL, which means "visible as before".
    let acl_principals: Vec<String> = row.try_get("acl_principals").unwrap_or_default();
    Ok(DocumentMeta {
        id: DocumentId(id),
        title,
        categories,
        subcategory,
        status,
        external_ref,
        chunk_count: chunk_count.unwrap_or(0).max(0) as u32,
        file_type,
        file_size,
        mime_type,
        s3_key,
        created_at,
        updated_at,
        knowledge_base_ids,
        metadata,
        sha256,
        doc_key,
        version,
        supersedes_document_id,
        effective_from,
        effective_to,
        is_latest,
        acl_principals,
    })
}
