//! A [`PageDescriber`] that remembers each page's description on disk.
//!
//! Describing a page is one paid vision call, and a re-ingest of the same PDF
//! renders exactly the same page images run after run. The key is the
//! describer's fingerprint (model, prompt version and the reasoning flag, which
//! change the answer) and a SHA-256 of the page's bytes — the dimensions that
//! decide the answer. Page number and mime type are deliberately left out: a
//! repeated slide is the same image and the prompt never mentions either.
//!
//! `None` (the model said the page has no visuals) is cached too, and is not
//! the same thing as a miss: it means "do not pay for this page again".
//!
//! A cache is an optimisation, never a source of truth: every read error or
//! corrupt entry is a miss, and [`DiskCache`] already logs (and never returns)
//! write errors.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{PageDescriber, PageImage, PortError};
use sha2::{Digest, Sha256};

use crate::store::{CacheKey, DiskCache};

const NAMESPACE: &str = "page-caption-v1";

/// Disk-caching decorator around any page describer.
pub struct CachedPageDescriber {
    inner: Arc<dyn PageDescriber>,
    cache: DiskCache,
    fingerprint: String,
}

impl CachedPageDescriber {
    /// `fingerprint` names everything but the image that changes an answer:
    /// model, prompt version and reasoning flag. Never an API key.
    #[must_use]
    pub fn new(
        inner: Arc<dyn PageDescriber>,
        cache: DiskCache,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(&self, page: &PageImage) -> CacheKey {
        let image = Sha256::digest(&page.bytes).to_vec();
        CacheKey::from_parts(&[self.fingerprint.as_bytes(), &image])
    }
}

#[async_trait]
impl PageDescriber for CachedPageDescriber {
    async fn describe(&self, page: &PageImage) -> Result<Option<String>, PortError> {
        let key = self.key(page);
        if let Some(raw) = self.cache.get(NAMESPACE, &key).await
            && let Ok(cached) = serde_json::from_slice::<Option<String>>(&raw)
        {
            self.cache.stats().hit(1);
            return Ok(cached);
        }
        self.cache.stats().miss(1);
        let description = self.inner.describe(page).await?;
        if let Ok(json) = serde_json::to_vec(&description) {
            self.cache.put(NAMESPACE, &key, &json).await;
        }
        Ok(description)
    }
}
