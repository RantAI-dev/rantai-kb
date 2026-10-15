//! Object storage port and path generation utilities.

use async_trait::async_trait;

use crate::error::PortError;

/// Object storage port (S3-compatible blob store).
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Upload bytes at `key`. Returns the number of bytes stored.
    async fn upload(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<u64, PortError>;

    /// Download bytes from the object store.
    async fn download(&self, key: &str) -> Result<Vec<u8>, PortError>;

    /// Delete an object from the object store.
    async fn delete(&self, key: &str) -> Result<(), PortError>;
}

/// Sanitize filename: replace non-[a-zA-Z0-9._-] characters with `_`.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Storage key for a document's original file. An absent or empty tenant id
/// becomes `global`, as in the Bun adapter's `tenantId || "global"`.
#[must_use]
pub fn document_path(tenant_id: Option<&str>, document_id: &str, filename: &str) -> String {
    let org = tenant_id.filter(|t| !t.is_empty()).unwrap_or("global");
    let safe = sanitize_filename(filename);
    format!("documents/{org}/{document_id}/{safe}")
}

/// Storage key for a derived asset (figure crop, table image).
#[must_use]
pub fn asset_path(tenant_id: Option<&str>, document_id: &str, filename: &str) -> String {
    let org = tenant_id.filter(|t| !t.is_empty()).unwrap_or("global");
    let safe = sanitize_filename(filename);
    format!("documents/{org}/{document_id}/assets/{safe}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_every_disallowed_character() {
        assert_eq!(sanitize_filename("my file (1).pdf"), "my_file__1_.pdf");
        assert_eq!(
            sanitize_filename("doc-name_v1.0.docx"),
            "doc-name_v1.0.docx"
        );
        // A path separator cannot survive into a key.
        assert_eq!(sanitize_filename("../../etc/passwd"), ".._.._etc_passwd");
    }

    #[test]
    fn empty_tenant_falls_back_to_global() {
        assert_eq!(
            document_path(None, "doc123", "guide.pdf"),
            "documents/global/doc123/guide.pdf"
        );
        assert_eq!(
            document_path(Some(""), "doc123", "guide.pdf"),
            "documents/global/doc123/guide.pdf"
        );
    }

    #[test]
    fn asset_paths_live_under_the_document() {
        assert_eq!(
            asset_path(Some("tenant1"), "doc123", "fig1.png"),
            "documents/tenant1/doc123/assets/fig1.png"
        );
    }
}
