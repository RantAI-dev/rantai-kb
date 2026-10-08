//! API key resolution and hashing.

use kb_ports::{AuthContext, KnowledgeBaseId, PortError, TenantId};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::queries::SELECT_API_KEY;

/// Hash a plaintext API key with SHA-256 and return hex string.
#[must_use]
pub fn hash_api_key(plaintext_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Resolve a plaintext key to its auth context in Postgres.
pub async fn resolve_api_key_query(
    pool: &PgPool,
    plaintext_key: &str,
) -> Result<Option<AuthContext>, PortError> {
    let hash = hash_api_key(plaintext_key);
    let row = sqlx::query(SELECT_API_KEY)
        .bind(&hash)
        .fetch_optional(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg api_key: {e}")))?;

    match row {
        Some(row) => {
            let key_id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("api_key id: {e}")))?;
            let tenant_id: String = row
                .try_get("tenantId")
                .map_err(|e| PortError::Unavailable(format!("api_key tenant: {e}")))?;
            let scopes: Vec<String> = row.try_get("scopes").unwrap_or_default();
            let kb_ids: Vec<String> = row.try_get("knowledgeBaseIds").unwrap_or_default();
            Ok(Some(AuthContext {
                tenant_id: TenantId(tenant_id),
                key_id,
                scopes,
                knowledge_base_ids: kb_ids
                    .into_iter()
                    .map(KnowledgeBaseId::new_unchecked)
                    .collect(),
            }))
        }
        None => Ok(None),
    }
}
