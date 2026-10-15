//! API key resolution and hashing.

use kb_ports::{AuthContext, KnowledgeBaseId, PortError, TenantId};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::queries::{SELECT_API_KEY, UPDATE_API_KEY_LAST_USED};

/// Hash a plaintext API key with SHA-256 and return hex string.
#[must_use]
pub fn hash_api_key(plaintext_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Constant-time comparison between two strings, matching Bun's timingSafeEqual.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
            let stored_hash: String = row
                .try_get("hash")
                .map_err(|e| PortError::Unavailable(format!("api_key hash: {e}")))?;
            if !constant_time_eq(&stored_hash, &hash) {
                return Ok(None);
            }

            let key_id: String = row
                .try_get("id")
                .map_err(|e| PortError::Unavailable(format!("api_key id: {e}")))?;
            let tenant_id: String = row
                .try_get("tenantId")
                .map_err(|e| PortError::Unavailable(format!("api_key tenant: {e}")))?;
            let scopes: Vec<String> = row.try_get("scopes").unwrap_or_default();
            let kb_ids: Vec<String> = row.try_get("knowledgeBaseIds").unwrap_or_default();

            // Best-effort usage stamp; never blocks the request (mirrors TS void prisma.apiKey.update)
            let pool_clone = pool.clone();
            let id_clone = key_id.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(UPDATE_API_KEY_LAST_USED)
                    .bind(&id_clone)
                    .execute(&pool_clone)
                    .await;
            });

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
