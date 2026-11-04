//! A [`PageVisionScorer`] that remembers each query's scores on disk.
//!
//! One search is one paid vision call over 10-20 base64 page images, and an
//! eval repeats exactly the same query over exactly the same images run after
//! run. The key is the scorer's fingerprint (model, prompt version and the
//! reasoning flag, which change the answer), the query, and per page its
//! number, mime type and a SHA-256 of its bytes — the same dimensions the
//! Python harness keys its cache on (`vision_rerank.py`). Page order is part of
//! the key, because the model is asked to score them in the order shown.
//!
//! A cache is an optimisation, never a source of truth: every read error is a
//! miss, and [`DiskCache`] already logs (and never returns) write errors.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{PageImage, PageVisionScorer, PortError};
use sha2::{Digest, Sha256};

use crate::store::{CacheKey, DiskCache};

const NAMESPACE: &str = "vision-rerank-v1";

/// Disk-caching decorator around any page vision scorer.
pub struct CachedPageVisionScorer {
    inner: Arc<dyn PageVisionScorer>,
    cache: DiskCache,
    fingerprint: String,
}

impl CachedPageVisionScorer {
    /// `fingerprint` names everything but the query and the images that changes
    /// an answer: model, prompt version and reasoning flag. Never an API key.
    #[must_use]
    pub fn new(
        inner: Arc<dyn PageVisionScorer>,
        cache: DiskCache,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(&self, query: &str, pages: &[PageImage]) -> CacheKey {
        let mut parts: Vec<Vec<u8>> = vec![
            self.fingerprint.as_bytes().to_vec(),
            query.as_bytes().to_vec(),
        ];
        for page in pages {
            parts.push(page.page.to_le_bytes().to_vec());
            parts.push(page.mime.as_bytes().to_vec());
            parts.push(Sha256::digest(&page.bytes).to_vec());
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        CacheKey::from_parts(&refs)
    }
}

#[async_trait]
impl PageVisionScorer for CachedPageVisionScorer {
    async fn score(&self, query: &str, pages: &[PageImage]) -> Result<Vec<f32>, PortError> {
        if pages.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.key(query, pages);
        if let Some(raw) = self.cache.get(NAMESPACE, &key).await
            && let Ok(scores) = serde_json::from_slice::<Vec<f32>>(&raw)
            && scores.len() == pages.len()
        {
            self.cache.stats().hit(1);
            return Ok(scores);
        }
        self.cache.stats().miss(1);
        let scores = self.inner.score(query, pages).await?;
        if scores.len() == pages.len()
            && let Ok(json) = serde_json::to_vec(&scores)
        {
            self.cache.put(NAMESPACE, &key, &json).await;
        }
        Ok(scores)
    }
}
