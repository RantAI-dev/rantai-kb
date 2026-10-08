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
    Ok(DocumentMeta {
        id: DocumentId(id),
        title,
        categories,
        subcategory,
        status,
        chunk_count: chunk_count.unwrap_or(0).max(0) as u32,
    })
}
