//! The OCR page cache on the disk store (`ING-6`).
//!
//! One entry per page, under the `ocr-page-v1` namespace. The key is the hex
//! SHA-256 the router computes over the provider id, model, prompt, resolution
//! and the page's own bytes, so the same page of the same file gets the same
//! key and a different provider never reuses another's answer.
//!
//! Only successful reads are stored (the router decides that), so a page lost
//! to an outage is retried on the next ingest instead of being remembered as
//! blank.

use async_trait::async_trait;
use kb_ports::OcrPageCache;

use crate::store::{CacheKey, DiskCache};

/// Namespace for cached page text. Bump the suffix when the stored shape
/// changes.
pub const OCR_PAGE_NAMESPACE: &str = "ocr-page-v1";

/// A [`kb_ports::OcrPageCache`] over a [`DiskCache`].
#[derive(Debug, Clone)]
pub struct DiskOcrPageCache {
    cache: DiskCache,
}

impl DiskOcrPageCache {
    /// Wrap an existing store, so the page cache shares the directory and the
    /// statistics of the rest of the ingest cache.
    #[must_use]
    pub fn new(cache: DiskCache) -> Self {
        Self { cache }
    }

    /// The store underneath, for tests and diagnostics.
    #[must_use]
    pub fn store(&self) -> &DiskCache {
        &self.cache
    }
}

fn key_of(hex_key: &str) -> Option<CacheKey> {
    let bytes = hex::decode(hex_key).ok()?;
    let raw: [u8; 32] = bytes.try_into().ok()?;
    Some(CacheKey::from_raw(raw))
}

#[async_trait]
impl OcrPageCache for DiskOcrPageCache {
    async fn get(&self, key: &str) -> Option<String> {
        let key = key_of(key)?;
        let bytes = self.cache.get(OCR_PAGE_NAMESPACE, &key).await?;
        String::from_utf8(bytes).ok()
    }

    async fn put(&self, key: &str, text: &str) {
        let Some(key) = key_of(key) else {
            tracing::warn!("OCR page cache key is not a SHA-256 hex string; not stored");
            return;
        };
        self.cache
            .put(OCR_PAGE_NAMESPACE, &key, text.as_bytes())
            .await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> DiskCache {
        let dir =
            std::env::temp_dir().join(format!("kb-ocr-page-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        DiskCache::new(dir)
    }

    #[tokio::test]
    async fn a_page_written_once_is_read_back_by_its_key() {
        let cache = DiskOcrPageCache::new(temp_store("roundtrip"));
        let key = "ab".repeat(32);
        assert_eq!(cache.get(&key).await, None);
        cache.put(&key, "page text").await;
        assert_eq!(cache.get(&key).await.as_deref(), Some("page text"));
    }

    #[tokio::test]
    async fn a_key_that_is_not_a_sha256_is_a_miss_not_a_panic() {
        let cache = DiskOcrPageCache::new(temp_store("bad-key"));
        assert_eq!(cache.get("not-a-key").await, None);
        cache.put("not-a-key", "text").await;
        assert_eq!(cache.get("not-a-key").await, None);
    }

    #[tokio::test]
    async fn two_keys_never_share_an_entry() {
        let cache = DiskOcrPageCache::new(temp_store("distinct"));
        let a = "11".repeat(32);
        let b = "22".repeat(32);
        cache.put(&a, "a").await;
        assert_eq!(cache.get(&b).await, None);
        assert_eq!(cache.get(&a).await.as_deref(), Some("a"));
    }
}
