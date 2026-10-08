//! Document mapping helpers for Postgres rows.

use kb_ports::{DocumentId, DocumentMeta};
use sqlx::Row;

pub fn row_to_doc_meta(row: &sqlx::postgres::PgRow) -> Result<DocumentMeta, sqlx::Error> {
    let id: String = row.try_get("id")?;
    let title: String = row.try_get("title")?;
    let categories: Vec<String> = row.try_get("categories")?;
    let subcategory: Option<String> = row.try_get("subcategory")?;
    let status: String = row.try_get("status")?;
    let chunk_count: Option<i64> = row.try_get("chunk_count")?;
    let file_type: Option<String> = row.try_get("file_type")?;
    let file_size: Option<i32> = row.try_get("file_size")?;
    let mime_type: Option<String> = row.try_get("mime_type")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let knowledge_base_ids: Vec<String> = row.try_get("knowledge_base_ids")?;
    let metadata: Option<serde_json::Value> = row.try_get("metadata")?;
    Ok(DocumentMeta {
        id: DocumentId(id),
        title,
        categories,
        subcategory,
        status,
        chunk_count: chunk_count.unwrap_or(0).max(0) as u32,
        file_type,
        file_size,
        mime_type,
        created_at,
        updated_at,
        knowledge_base_ids,
        metadata,
    })
}
