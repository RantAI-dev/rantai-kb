//! An [`Embedder`] that remembers every vector on disk.
//!
//! The key is the embedder's *fingerprint* (model, dimension, prefixes, an
//! optional operator tag) plus the exact text, so a vector is reused only when
//! the same model would be asked the same question. Passages and queries live
//! in separate key spaces because many models embed them differently.
//!
//! A batch is split into hits and misses; only the misses go to the provider,
//! in one call, in their original order. A provider response of the wrong
//! length or dimension is an error and nothing from it is stored.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{Embedder, PortError, QueryEmbedding};

use crate::store::{CacheKey, DiskCache};
use crate::vectors::{decode, encode};

const NAMESPACE: &str = "embed-v1";

/// Disk-caching decorator around any embedder.
pub struct CachedEmbedder {
    inner: Arc<dyn Embedder>,
    cache: DiskCache,
    fingerprint: String,
    query_prefix: String,
    passage_prefix: String,
}

impl CachedEmbedder {
    /// `fingerprint` must change whenever the vectors for a given text would:
    /// a different model, dimension, prefix or quantisation.
    #[must_use]
    pub fn new(inner: Arc<dyn Embedder>, cache: DiskCache, fingerprint: impl Into<String>) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
            query_prefix: String::new(),
            passage_prefix: String::new(),
        }
    }

    /// The prefixes the inner embedder wraps around queries and passages.
    /// Each side's key depends only on its own prefix, so changing the query
    /// instruction never invalidates the (far more numerous) passage vectors.
    #[must_use]
    pub fn with_prefixes(mut self, query_prefix: &str, passage_prefix: &str) -> Self {
        self.query_prefix = query_prefix.to_string();
        self.passage_prefix = passage_prefix.to_string();
        self
    }

    fn key(&self, kind: &[u8], text: &str) -> CacheKey {
        let prefix = if kind == b"query" {
            &self.query_prefix
        } else {
            &self.passage_prefix
        };
        CacheKey::from_parts(&[
            self.fingerprint.as_bytes(),
            kind,
            prefix.as_bytes(),
            text.as_bytes(),
        ])
    }

    async fn load(&self, key: &CacheKey) -> Option<Vec<f32>> {
        let bytes = self.cache.get(NAMESPACE, key).await?;
        decode(&bytes, self.inner.dimension())
    }

    async fn store(&self, key: &CacheKey, vector: &[f32]) {
        self.cache.put(NAMESPACE, key, &encode(vector)).await;
    }

    fn check_dim(&self, v: &[f32]) -> Result<(), PortError> {
        let want = self.inner.dimension() as usize;
        if v.len() == want {
            Ok(())
        } else {
            Err(PortError::Malformed(format!(
                "embedder returned a {}-d vector, expected {want}",
                v.len()
            )))
        }
    }
}

#[async_trait]
impl Embedder for CachedEmbedder {
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        self.embed_query_detailed(text).await.map(|q| q.vector)
    }

    async fn embed_query_detailed(&self, text: &str) -> Result<QueryEmbedding, PortError> {
        let key = self.key(b"query", text);
        if let Some(vector) = self.load(&key).await {
            self.cache.stats().hit(1);
            return Ok(QueryEmbedding {
                vector,
                cached: true,
            });
        }
        self.cache.stats().miss(1);
        let q = self.inner.embed_query_detailed(text).await?;
        self.check_dim(&q.vector)?;
        self.store(&key, &q.vector).await;
        Ok(q)
    }

    async fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, PortError> {
        let keys: Vec<CacheKey> = texts.iter().map(|t| self.key(b"passage", t)).collect();
        let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        for key in &keys {
            out.push(self.load(key).await);
        }
        let miss_idx: Vec<usize> = (0..texts.len()).filter(|&i| out[i].is_none()).collect();
        self.cache
            .stats()
            .hit((texts.len() - miss_idx.len()) as u64);
        self.cache.stats().miss(miss_idx.len() as u64);

        if !miss_idx.is_empty() {
            let miss_texts: Vec<String> = miss_idx.iter().map(|&i| texts[i].clone()).collect();
            let fresh = self.inner.embed_passages(&miss_texts).await?;
            if fresh.len() != miss_texts.len() {
                return Err(PortError::Malformed(format!(
                    "embedder returned {} vectors for {} texts",
                    fresh.len(),
                    miss_texts.len()
                )));
            }
            for v in &fresh {
                self.check_dim(v)?;
            }
            for (&i, v) in miss_idx.iter().zip(fresh) {
                self.store(&keys[i], &v).await;
                out[i] = Some(v);
            }
        }
        out.into_iter()
            .map(|v| v.ok_or_else(|| PortError::Storage("cache assembly lost a vector".into())))
            .collect()
    }

    fn dimension(&self) -> u32 {
        self.inner.dimension()
    }
}
