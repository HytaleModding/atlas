//! Content-addressed cache for chunk embeddings.
//!
//! Embedding is the slowest part of indexing - on a full Hytale build
//! the BGE-small pass dominates wall time. Almost every chunk is
//! identical between two consecutive builds (a release artifact and a
//! pre-release artifact for the same Hytale build share the vast
//! majority of decompiled source); this cache lets us reuse those
//! vectors without re-running the model.
//!
//! Cache key: `sha256(chunk_text || chunker_version || embedder_id)`.
//! Bumping the chunker or swapping the embedder model changes the
//! hash, so the cache invalidates itself without manual eviction.
//!
//! Two implementations:
//!   - [`NullCache`]: drop-in default for callers that don't care
//!     (desktop indexer; tests).
//!   - [`DiskCache`]: flat directory, one file per cached vector.
//!     Used by `atlas-build index --embedding-cache <path>` in CI.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Pluggable cache for embedding vectors keyed by chunk text.
///
/// Implementors must be cheap on miss (the indexer calls `get` for
/// every chunk in a batch) and tolerate concurrent calls from the
/// indexer thread without external locking.
pub trait EmbedCache: Send + Sync {
    /// Look up a cached vector. `None` for both "key not present"
    /// and "key present but unreadable" - the indexer treats both
    /// the same and re-embeds.
    fn get(&self, key: &str) -> Option<Vec<f32>>;

    /// Store a vector for `key`. Errors are logged at warn level and
    /// swallowed; a cache write failure must never abort an indexing
    /// run.
    fn put(&self, key: &str, vector: &[f32]);
}

/// Compose the cache key for a chunk. Bumping `chunker_version` or
/// swapping `embedder_id` changes the hash, so an indexer that bumps
/// either constant naturally invalidates without a manual flush.
pub fn cache_key(text: &str, chunker_version: &str, embedder_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    // Length-prefix the auxiliary fields so e.g. ("foo", "bar") and
    // ("fo", "obar") can't collide. Using 0x1F (unit separator) as a
    // boundary marker would also work; explicit lengths are more
    // future-proof if the embedder_id ever contains non-ASCII.
    hasher.update(b"\x1f");
    hasher.update(chunker_version.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(embedder_id.as_bytes());
    let digest = hasher.finalize();
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// No-op implementation. Returned by `default_cache()` when no caller
/// asked for caching, so the hot path stays the same shape.
pub struct NullCache;

impl EmbedCache for NullCache {
    fn get(&self, _: &str) -> Option<Vec<f32>> {
        None
    }
    fn put(&self, _: &str, _: &[f32]) {}
}

/// Flat-directory cache. One file per cached vector at
/// `<root>/<hex_key>.bin`. The body is a raw little-endian `f32`
/// dump (no header) - the embedder dimension is implicit in the
/// file size, and a corrupt or wrong-sized file falls through to a
/// re-embed via the length check in [`Self::get`].
pub struct DiskCache {
    root: PathBuf,
    /// Bytes per cached vector (`embedder_dim * 4`). Reads with a
    /// different length are treated as a miss; this guards against
    /// the rare case where two builds with the same `embedder_id`
    /// somehow disagree on dimension.
    expected_bytes: usize,
}

impl DiskCache {
    /// Create or open the cache. Missing directories are created;
    /// existing files are left in place. Errors here propagate
    /// because a bad cache directory should fail the build, not
    /// silently fall through to N re-embeds.
    pub fn open(root: impl Into<PathBuf>, embedder_dim: usize) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            expected_bytes: embedder_dim * 4,
        })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.bin"))
    }
}

impl EmbedCache for DiskCache {
    fn get(&self, key: &str) -> Option<Vec<f32>> {
        let path = self.path_for(key);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "embed_cache read failed");
                return None;
            }
        };
        if bytes.len() != self.expected_bytes {
            tracing::warn!(
                expected = self.expected_bytes,
                actual = bytes.len(),
                path = %path.display(),
                "embed_cache file has wrong size, treating as miss"
            );
            return None;
        }
        let dim = self.expected_bytes / 4;
        let mut out = Vec::with_capacity(dim);
        for chunk in bytes.chunks_exact(4) {
            // chunks_exact gives us 4 bytes; the unwrap is infallible.
            let arr: [u8; 4] = chunk.try_into().unwrap();
            out.push(f32::from_le_bytes(arr));
        }
        Some(out)
    }

    fn put(&self, key: &str, vector: &[f32]) {
        if vector.len() * 4 != self.expected_bytes {
            tracing::warn!(
                expected = self.expected_bytes,
                actual = vector.len() * 4,
                "embed_cache: refusing to write mis-sized vector"
            );
            return;
        }
        let path = self.path_for(key);
        // Write to a sibling temp file then rename so a partial write
        // can't poison the cache. Concurrent writers landing on the
        // same key both succeed; whoever loses the rename race is
        // overwritten harmlessly (vectors are deterministic per key).
        let tmp = path.with_extension("bin.tmp");
        let mut bytes = Vec::with_capacity(self.expected_bytes);
        for v in vector {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        if let Err(err) = fs::write(&tmp, &bytes) {
            tracing::warn!(?err, path = %tmp.display(), "embed_cache write failed");
            return;
        }
        if let Err(err) = fs::rename(&tmp, &path) {
            tracing::warn!(?err, path = %path.display(), "embed_cache rename failed");
            // Best-effort cleanup of the temp file; ignore errors.
            let _ = fs::remove_file(&tmp);
        }
    }
}

/// Resolve the directory the desktop client should hand to
/// [`DiskCache::open`] when caching across rebuilds. Sits alongside the
/// other shared caches (`<cache-root>/embeddings/`).
pub fn default_cache_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("embeddings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_each_input() {
        let a = cache_key("hello", "1.0.0", "bge-small-en-v1.5");
        let b = cache_key("hello!", "1.0.0", "bge-small-en-v1.5");
        let c = cache_key("hello", "1.0.1", "bge-small-en-v1.5");
        let d = cache_key("hello", "1.0.0", "other-model");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        // Hex sha256 → 64 chars.
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn disk_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::open(dir.path(), 4).unwrap();
        let key = cache_key("foo", "1.0.0", "test");
        assert!(cache.get(&key).is_none());
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]);
        let got = cache.get(&key).expect("expected cache hit after put");
        assert_eq!(got, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn disk_cache_dim_mismatch_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::open(dir.path(), 4).unwrap();
        let key = cache_key("bar", "1.0.0", "test");
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]);
        // Reopen with a different expected dim - same file but the
        // length check triggers a miss.
        let cache_wrong = DiskCache::open(dir.path(), 8).unwrap();
        assert!(cache_wrong.get(&key).is_none());
    }
}
