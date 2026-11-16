//! An [`ImageDescriber`] that remembers each image's reading on disk (`ING-5`).
//!
//! Reading an image is a vision-model call: paid on the cloud tier, tens of
//! seconds of CPU on the local ones. A re-ingest, a reindex or the same logo on
//! every slide of every deck would otherwise pay again. The key is the
//! describer's fingerprint (model, prompt version, language, reasoning flag)
//! and a SHA-256 of the normalised image bytes; mime type and position are left
//! out because the answer does not depend on them.
//!
//! Only answers are cached. A failure is not: it may be a rate limit.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{ImageDescriber, ImageReading, PortError};
use sha2::{Digest, Sha256};

use crate::store::{CacheKey, DiskCache};

const NAMESPACE: &str = "image-read-v1";

/// Disk-caching decorator around any image describer.
pub struct CachedImageDescriber {
    inner: Arc<dyn ImageDescriber>,
    cache: DiskCache,
    fingerprint: String,
}

impl CachedImageDescriber {
    /// `fingerprint` names everything but the image that changes an answer.
    /// Never an API key.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ImageDescriber>,
        cache: DiskCache,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(&self, bytes: &[u8]) -> CacheKey {
        let image = Sha256::digest(bytes).to_vec();
        CacheKey::from_parts(&[self.fingerprint.as_bytes(), &image])
    }
}

#[async_trait]
impl ImageDescriber for CachedImageDescriber {
    async fn read(&self, mime: &str, bytes: &[u8]) -> Result<ImageReading, PortError> {
        let key = self.key(bytes);
        if let Some(raw) = self.cache.get(NAMESPACE, &key).await
            && let Ok(cached) = serde_json::from_slice::<ImageReading>(&raw)
        {
            self.cache.stats().hit(1);
            return Ok(cached);
        }
        self.cache.stats().miss(1);
        let reading = self.inner.read(mime, bytes).await?;
        if let Ok(json) = serde_json::to_vec(&reading) {
            self.cache.put(NAMESPACE, &key, &json).await;
        }
        Ok(reading)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kb_ports::VisualKind;

    use super::*;

    struct Counting(AtomicUsize, bool);

    #[async_trait]
    impl ImageDescriber for Counting {
        async fn read(&self, _: &str, bytes: &[u8]) -> Result<ImageReading, PortError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if self.1 {
                return Err(PortError::Unavailable("429".into()));
            }
            Ok(ImageReading {
                kind: VisualKind::Photo,
                description: format!("{} bytes", bytes.len()),
                text: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn the_same_image_is_read_once_and_a_failure_is_never_cached() {
        let dir = std::env::temp_dir().join(format!("kb-cache-image-{}", uuid::Uuid::new_v4()));
        let inner = Arc::new(Counting(AtomicUsize::new(0), false));
        let cached = CachedImageDescriber::new(inner.clone(), DiskCache::new(&dir), "m|v1");
        let a = cached.read("image/jpeg", b"abc").await.unwrap();
        let b = cached.read("image/png", b"abc").await.unwrap();
        assert_eq!(a, b);
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            1,
            "mime is not part of the key"
        );
        cached.read("image/jpeg", b"abcd").await.unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);

        let other = CachedImageDescriber::new(inner.clone(), DiskCache::new(&dir), "m|v2");
        other.read("image/jpeg", b"abc").await.unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            3,
            "a new prompt version is a miss"
        );

        let failing = Arc::new(Counting(AtomicUsize::new(0), true));
        let cached = CachedImageDescriber::new(failing.clone(), DiskCache::new(&dir), "f");
        assert!(cached.read("image/jpeg", b"x").await.is_err());
        assert!(cached.read("image/jpeg", b"x").await.is_err());
        assert_eq!(failing.0.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
