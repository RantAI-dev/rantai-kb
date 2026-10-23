//! A [`PageEmbedder`] that remembers every page and query vector on disk,
//! keyed by the embedder's fingerprint and the image bytes (or query text).
//! Page images are the most expensive input a hosted embedder sees, so this
//! is where rule 7 pays most.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{PageEmbedder, PageImage, PortError};
use sha2::{Digest, Sha256};

use crate::store::{CacheKey, DiskCache};
use crate::vectors::{decode, encode};

const NAMESPACE: &str = "page-embed-v1";

/// Disk-caching decorator around any page embedder.
pub struct CachedPageEmbedder {
    inner: Arc<dyn PageEmbedder>,
    cache: DiskCache,
    fingerprint: String,
}

impl CachedPageEmbedder {
    #[must_use]
    pub fn new(
        inner: Arc<dyn PageEmbedder>,
        cache: DiskCache,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(&self, kind: &[u8], content: &[u8]) -> CacheKey {
        let digest: [u8; 32] = Sha256::digest(content).into();
        CacheKey::from_parts(&[self.fingerprint.as_bytes(), kind, &digest])
    }

    async fn load(&self, key: &CacheKey) -> Option<Vec<f32>> {
        decode(
            &self.cache.get(NAMESPACE, key).await?,
            self.inner.dimension(),
        )
    }

    fn check(&self, v: &[f32]) -> Result<(), PortError> {
        let want = self.inner.dimension() as usize;
        if v.len() == want {
            Ok(())
        } else {
            Err(PortError::Malformed(format!(
                "page embedder returned a {}-d vector, expected {want}",
                v.len()
            )))
        }
    }
}

#[async_trait]
impl PageEmbedder for CachedPageEmbedder {
    async fn embed_pages(&self, pages: &[PageImage]) -> Result<Vec<Vec<f32>>, PortError> {
        // Image-only pages keep the key they always had; a page with text is a
        // different input and a different vector.
        let keys: Vec<CacheKey> = pages
            .iter()
            .map(|p| {
                if p.text.is_empty() {
                    self.key(b"page", &p.bytes)
                } else {
                    let mut content = p.text.as_bytes().to_vec();
                    content.push(0);
                    content.extend_from_slice(&p.bytes);
                    self.key(b"page+text", &content)
                }
            })
            .collect();
        let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(pages.len());
        for k in &keys {
            out.push(self.load(k).await);
        }
        let miss: Vec<usize> = (0..pages.len()).filter(|&i| out[i].is_none()).collect();
        self.cache.stats().hit((pages.len() - miss.len()) as u64);
        self.cache.stats().miss(miss.len() as u64);
        if !miss.is_empty() {
            let todo: Vec<PageImage> = miss.iter().map(|&i| pages[i].clone()).collect();
            let fresh = self.inner.embed_pages(&todo).await?;
            if fresh.len() != todo.len() {
                return Err(PortError::Malformed(format!(
                    "page embedder returned {} vectors for {} pages",
                    fresh.len(),
                    todo.len()
                )));
            }
            for v in &fresh {
                self.check(v)?;
            }
            for (&i, v) in miss.iter().zip(fresh) {
                self.cache.put(NAMESPACE, &keys[i], &encode(&v)).await;
                out[i] = Some(v);
            }
        }
        out.into_iter()
            .map(|v| v.ok_or_else(|| PortError::Storage("cache assembly lost a vector".into())))
            .collect()
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, PortError> {
        let key = self.key(b"query", text.as_bytes());
        if let Some(v) = self.load(&key).await {
            self.cache.stats().hit(1);
            return Ok(v);
        }
        self.cache.stats().miss(1);
        let v = self.inner.embed_query(text).await?;
        self.check(&v)?;
        self.cache.put(NAMESPACE, &key, &encode(&v)).await;
        Ok(v)
    }

    fn dimension(&self) -> u32 {
        self.inner.dimension()
    }
}
