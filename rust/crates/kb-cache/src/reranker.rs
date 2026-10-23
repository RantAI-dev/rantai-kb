//! A [`Reranker`] that remembers each call's result on disk.
//!
//! Reranking is now the most frequent paid call in a search (the chunk rerank
//! and the page rerank each send ~20–30 passages per query), and an eval
//! repeats exactly the same calls run after run. The key is the reranker's
//! fingerprint (provider, model, endpoint; never a key), the query, `final_k`
//! and the candidates' texts in order, so a hit is the same question over the
//! same passages. The stored value is the reranker's order as candidate
//! positions with scores; ids are re-attached from the current call, so a hit
//! can never return an id the caller did not pass in.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{PortError, RerankCandidate, RerankResult, Reranker};
use serde::{Deserialize, Serialize};

use crate::store::{CacheKey, DiskCache};

const NAMESPACE: &str = "rerank-v1";

/// Disk-caching decorator around any reranker.
pub struct CachedReranker {
    inner: Arc<dyn Reranker>,
    cache: DiskCache,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    /// `(position in the candidate list, score)` in the reranker's order.
    order: Vec<(u32, f32)>,
}

impl CachedReranker {
    #[must_use]
    pub fn new(inner: Arc<dyn Reranker>, cache: DiskCache, fingerprint: impl Into<String>) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(&self, query: &str, candidates: &[RerankCandidate], final_k: u32) -> CacheKey {
        let k = final_k.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![self.fingerprint.as_bytes(), query.as_bytes(), &k];
        parts.extend(candidates.iter().map(|c| c.text.as_bytes()));
        CacheKey::from_parts(&parts)
    }
}

#[async_trait]
impl Reranker for CachedReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
        final_k: u32,
    ) -> Result<Vec<RerankResult>, PortError> {
        if candidates.is_empty() {
            return self.inner.rerank(query, candidates, final_k).await;
        }
        let key = self.key(query, &candidates, final_k);
        if let Some(raw) = self.cache.get(NAMESPACE, &key).await
            && let Ok(entry) = serde_json::from_slice::<Entry>(&raw)
        {
            let hit: Option<Vec<RerankResult>> = entry
                .order
                .iter()
                .enumerate()
                .map(|(rank, (pos, score))| {
                    candidates.get(*pos as usize).map(|c| RerankResult {
                        id: c.id.clone(),
                        final_rank: u32::try_from(rank).unwrap_or(u32::MAX),
                        score: *score,
                    })
                })
                .collect();
            if let Some(results) = hit {
                self.cache.stats().hit(1);
                return Ok(results);
            }
        }
        self.cache.stats().miss(1);
        let ids: Vec<_> = candidates.iter().map(|c| c.id.clone()).collect();
        let results = self.inner.rerank(query, candidates, final_k).await?;
        let order: Option<Vec<(u32, f32)>> = results
            .iter()
            .map(|r| {
                ids.iter()
                    .position(|id| *id == r.id)
                    .map(|p| (u32::try_from(p).unwrap_or(u32::MAX), r.score))
            })
            .collect();
        // Only a result made entirely of the caller's ids is cacheable.
        if let Some(order) = order
            && let Ok(json) = serde_json::to_vec(&Entry { order })
        {
            self.cache.put(NAMESPACE, &key, &json).await;
        }
        Ok(results)
    }
}
