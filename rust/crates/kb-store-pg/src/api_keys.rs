//! API key minting — the write side of `scripts/create-api-key.ts`.
//!
//! The format is fixed by the TypeScript service: `rkb_` followed by 32 random
//! bytes in unpadded base64url, hashed with SHA-256, and only the hash stored.
//! A key minted here is byte-for-byte interchangeable with one minted by
//! `bun scripts/create-api-key.ts`, which is what lets an operator create keys
//! with whichever binary is running during cutover.

use std::io::Read;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use kb_ports::PortError;
use sqlx::PgPool;

use crate::auth::hash_api_key;
use crate::queries::INSERT_API_KEY;

/// Entropy per key, matching `randomBytes(32)` in `generateApiKey`.
const KEY_ENTROPY_BYTES: usize = 32;

/// A freshly minted key. `plaintext` is shown once and never stored.
#[derive(Clone, Debug)]
pub struct MintedApiKey {
    /// Primary key the row is written with.
    pub id: String,
    /// `rkb_<43 base64url chars>` — the value the caller authenticates with.
    pub plaintext: String,
    /// SHA-256 hex of `plaintext`, which is all that reaches the database.
    pub hash: String,
}

/// The tenant, name and optional restrictions of a new key.
#[derive(Clone, Debug)]
pub struct NewApiKey<'a> {
    pub tenant_id: &'a str,
    /// Default name: "default", as in the TypeScript script.
    pub name: &'a str,
    /// Empty means every scope; otherwise e.g. `["kb:read", "kb:write"]`.
    pub scopes: &'a [String],
    /// Empty means every knowledge base in the tenant.
    pub knowledge_base_ids: &'a [String],
}

/// Mint a key: 32 bytes of entropy from the kernel CSPRNG, base64url-encoded.
///
/// `/dev/urandom` is read directly rather than through a `rand` dependency:
/// the format must match Node's exactly, and a second CSPRNG crate would not
/// change what the hash is computed over.
pub fn generate_api_key() -> Result<MintedApiKey, PortError> {
    let plaintext = format!("rkb_{}", URL_SAFE_NO_PAD.encode(random_bytes()?));
    Ok(MintedApiKey {
        id: uuid::Uuid::new_v4().to_string(),
        hash: hash_api_key(&plaintext),
        plaintext,
    })
}

/// Store the row for a minted key.
///
/// # Errors
///
/// Fails when the insert is rejected — a hash collision being the only
/// realistic cause, and the one worth failing on rather than retrying.
pub async fn insert_api_key(
    pool: &PgPool,
    key: &MintedApiKey,
    input: &NewApiKey<'_>,
) -> Result<(), PortError> {
    sqlx::query(INSERT_API_KEY)
        .bind(&key.id)
        .bind(input.tenant_id)
        .bind(input.name)
        .bind(&key.hash)
        .bind(input.scopes)
        .bind(input.knowledge_base_ids)
        .execute(pool)
        .await
        .map_err(|e| PortError::Unavailable(format!("pg insert api_key: {e}")))?;
    Ok(())
}

fn random_bytes() -> Result<Vec<u8>, PortError> {
    let mut source = std::fs::File::open("/dev/urandom")
        .map_err(|e| PortError::Unavailable(format!("open /dev/urandom: {e}")))?;
    let mut buffer = vec![0u8; KEY_ENTROPY_BYTES];
    source
        .read_exact(&mut buffer)
        .map_err(|e| PortError::Unavailable(format!("read /dev/urandom: {e}")))?;
    Ok(buffer)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_match_the_typescript_format() {
        let key = generate_api_key().expect("mint");
        assert!(key.plaintext.starts_with("rkb_"));
        // 32 bytes in unpadded base64url is always 43 characters.
        let payload = key.plaintext.strip_prefix("rkb_").expect("prefix");
        assert_eq!(payload.len(), 43);
        assert!(
            payload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        // The row stores the hash, never the plaintext.
        assert_eq!(key.hash, hash_api_key(&key.plaintext));
        assert_eq!(key.hash.len(), 64);
        assert!(!key.hash.contains("rkb_"));
    }

    #[test]
    fn keys_are_unique() {
        let a = generate_api_key().expect("mint");
        let b = generate_api_key().expect("mint");
        assert_ne!(a.plaintext, b.plaintext);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn hashing_matches_a_known_sha256_vector() {
        // NIST/RFC 6234 vector, so a hashing regression is caught here
        // rather than by a key that silently stops authenticating.
        assert_eq!(
            hash_api_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
