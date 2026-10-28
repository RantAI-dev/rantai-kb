//! An [`Extractor`] that remembers each file's extraction on disk.
//!
//! The key is the extractor chain's fingerprint, the MIME type, the file
//! extension, the extraction options and the SHA-256 of the file bytes. Two
//! uploads of the same PDF are parsed once, whatever their names, and an OCR or
//! VLM parse is never paid for twice.
//!
//! Only successful results are stored: a result carrying `error` may be a
//! transient outage and must be retried next time.
//!
//! Figure crops are stored as their own entries, keyed by their bytes, so the
//! JSON stays small and an identical crop shared by many files is kept once.
//!
//! Cache the *raw* parser output only. Cleanup that runs after extraction
//! (header and footer removal, table rebuilding) belongs downstream, so changing
//! it never needs a cache flush.

use std::sync::Arc;

use async_trait::async_trait;
use kb_ports::{ExtractionOptions, ExtractionResult, Extractor, PortError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::{CacheKey, DiskCache};

const NAMESPACE: &str = "extract-v1";
const FIGURE_NAMESPACE: &str = "extract-figure-v1";

/// Disk-caching decorator around any extractor.
pub struct CachedExtractor {
    inner: Arc<dyn Extractor>,
    cache: DiskCache,
    fingerprint: String,
}

/// What is written for one file: the result with figure bytes replaced by keys.
#[derive(Serialize, Deserialize)]
struct Entry {
    result: ExtractionResult,
    figure_keys: Vec<String>,
}

impl CachedExtractor {
    /// `fingerprint` must change whenever the extractor chain's output for a
    /// given file could: a different parser, model, order or code version.
    #[must_use]
    pub fn new(
        inner: Arc<dyn Extractor>,
        cache: DiskCache,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            cache,
            fingerprint: fingerprint.into(),
        }
    }

    fn key(
        &self,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        options: &ExtractionOptions,
    ) -> CacheKey {
        let ext = filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        let opts = serde_json::to_vec(options).unwrap_or_default();
        let content: [u8; 32] = Sha256::digest(bytes).into();
        CacheKey::from_parts(&[
            self.fingerprint.as_bytes(),
            mime_type.as_bytes(),
            ext.as_bytes(),
            &opts,
            &content,
        ])
    }

    async fn load(&self, key: &CacheKey) -> Option<ExtractionResult> {
        let raw = self.cache.get(NAMESPACE, key).await?;
        let entry: Entry = serde_json::from_slice(&raw).ok()?;
        let mut result = entry.result;
        if entry.figure_keys.len() != result.figures.len() {
            return None;
        }
        for (fig, hex) in result.figures.iter_mut().zip(&entry.figure_keys) {
            let key = parse_key(hex)?;
            fig.image_bytes = self.cache.get(FIGURE_NAMESPACE, &key).await?;
        }
        Some(result)
    }

    async fn store(&self, key: &CacheKey, result: &ExtractionResult) {
        let mut stripped = result.clone();
        let mut figure_keys = Vec::with_capacity(stripped.figures.len());
        for fig in &mut stripped.figures {
            let bytes = std::mem::take(&mut fig.image_bytes);
            let fkey = CacheKey::from_parts(&[&bytes]);
            self.cache.put(FIGURE_NAMESPACE, &fkey, &bytes).await;
            figure_keys.push(fkey.hex());
        }
        let entry = Entry {
            result: stripped,
            figure_keys,
        };
        if let Ok(json) = serde_json::to_vec(&entry) {
            // Figures first, then the entry that points at them, so a reader
            // never finds an entry whose figures are missing.
            self.cache.put(NAMESPACE, key, &json).await;
        }
    }
}

fn parse_key(hex_str: &str) -> Option<CacheKey> {
    let bytes = hex::decode(hex_str).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(CacheKey::from_raw(arr))
}

#[async_trait]
impl Extractor for CachedExtractor {
    async fn extract(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
    ) -> Result<ExtractionResult, PortError> {
        self.extract_with_options(
            filename,
            mime_type,
            file_bytes,
            ExtractionOptions::default(),
        )
        .await
    }

    async fn extract_with_options(
        &self,
        filename: &str,
        mime_type: &str,
        file_bytes: &[u8],
        options: ExtractionOptions,
    ) -> Result<ExtractionResult, PortError> {
        let key = self.key(filename, mime_type, file_bytes, &options);
        if let Some(hit) = self.load(&key).await {
            self.cache.stats().hit(1);
            return Ok(hit);
        }
        self.cache.stats().miss(1);
        let result = self
            .inner
            .extract_with_options(filename, mime_type, file_bytes, options)
            .await?;
        if result.error.is_none() {
            self.store(&key, &result).await;
        }
        Ok(result)
    }
}
