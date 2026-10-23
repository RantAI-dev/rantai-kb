//! A content-addressed key → bytes store on local disk.
//!
//! Layout: `<root>/<namespace>/<first 2 hex chars>/<sha256 hex>`. Writes go to a
//! temporary file in the same directory and are renamed into place, so a reader
//! never sees a half-written entry, and two writers racing on the same key both
//! leave a complete file behind.
//!
//! The cache is an optimisation, never a source of truth: every I/O error is
//! logged and reported as a miss (on read) or ignored (on write). A broken cache
//! directory makes ingest slower, never wrong.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tracing::warn;

/// Hit and miss counters, shared by every wrapper built on one store.
#[derive(Debug, Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
    write_errors: AtomicU64,
}

impl CacheStats {
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }
    pub(crate) fn hit(&self, n: u64) {
        self.hits.fetch_add(n, Ordering::Relaxed);
    }
    pub(crate) fn miss(&self, n: u64) {
        self.misses.fetch_add(n, Ordering::Relaxed);
    }
}

/// Disk store rooted at one directory. Cheap to clone.
#[derive(Debug, Clone)]
pub struct DiskCache {
    root: Arc<PathBuf>,
    stats: Arc<CacheStats>,
}

/// A 32-byte content key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    /// Hash length-prefixed parts, so `("ab", "c")` and `("a", "bc")` differ.
    #[must_use]
    pub fn from_parts(parts: &[&[u8]]) -> Self {
        let mut h = Sha256::new();
        for p in parts {
            h.update((p.len() as u64).to_le_bytes());
            h.update(p);
        }
        Self(h.finalize().into())
    }

    /// Rebuild a key from its 32 raw bytes.
    #[must_use]
    pub fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl DiskCache {
    /// A store rooted at `root`. The directory is created lazily on first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
            stats: Arc::new(CacheStats::default()),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    fn path(&self, namespace: &str, key: &CacheKey) -> PathBuf {
        let hex = key.hex();
        self.root.join(namespace).join(&hex[..2]).join(hex)
    }

    /// Read an entry. `None` on a miss or on any I/O error.
    pub async fn get(&self, namespace: &str, key: &CacheKey) -> Option<Vec<u8>> {
        let path = self.path(namespace, key);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cache read failed; treating as a miss");
                None
            }
        }
    }

    /// Write an entry atomically. Errors are logged and counted, never returned.
    pub async fn put(&self, namespace: &str, key: &CacheKey, value: &[u8]) {
        let path = self.path(namespace, key);
        if let Err(e) = write_atomic(&path, value).await {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            warn!(path = %path.display(), error = %e, "cache write failed");
        }
    }
}

async fn write_atomic(path: &Path, value: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    tokio::fs::create_dir_all(dir).await?;
    let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    if let Err(e) = tokio::fs::write(&tmp, value).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(())
}
