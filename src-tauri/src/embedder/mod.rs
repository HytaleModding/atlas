//! Embedding backend for Atlas semantic search.
//!
//! Turns a chunk of Java source into a fixed-dim vector.
//!
//! The [`Embedder`] trait is intentionally minimal so the fastembed-rs
//! path in [`bge_small`] isn't load-bearing: plan.md documents `ort`,
//! `candle`, and a Python sidecar as drop-in replacements. Keeping the
//! trait small means swapping the impl is cheap if fastembed breaks on
//! a target.

pub mod bge_small;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use self::bge_small::BgeSmall;

/// Output dimension of the v1 embedding space. Pinned to BGE-small-en-v1.5.
/// Any alternate [`Embedder`] MUST return vectors of this length - the
/// LanceDB schema in M3 will reject mismatches at write time.
pub const EMBEDDING_DIM: usize = 384;

/// Load-bearing trait: "turn text into a vector."
///
/// Implementations MUST:
///   - return vectors of length [`EMBEDDING_DIM`],
///   - return one vector per input, in input order,
///   - return L2-normalized vectors so cosine similarity == dot product
///     downstream (saves a per-query normalization in search).
///
/// `Send + Sync` so a single instance can live in Tauri state and be
/// called from indexer threads without synchronization at the caller.
pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Lazily-initialised embedder handle held in Tauri state. The first
/// `get_or_init` call downloads the BGE-small weights (~80 MB) and
/// constructs the ONNX session; subsequent calls hand back the cached
/// [`Arc<dyn Embedder>`]. Construction is serialised behind a mutex so a
/// flurry of index_start calls can't race the download.
#[derive(Default)]
pub struct SharedEmbedder {
    inner: Mutex<Option<Arc<dyn Embedder>>>,
}

impl SharedEmbedder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the embedder, constructing it on first call. `cache_dir` is
    /// only read on the first call - later calls reuse whatever was built
    /// before, so the caller is free to compute a path each time.
    pub fn get_or_init(&self, cache_dir: PathBuf) -> Result<Arc<dyn Embedder>> {
        let mut guard = self.inner.lock().expect("embedder poisoned");
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        let bge = BgeSmall::new(cache_dir)?;
        let arc: Arc<dyn Embedder> = Arc::new(bge);
        *guard = Some(arc.clone());
        Ok(arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trivial impl used to prove the trait shape: any `dyn Embedder`
    /// can be boxed, called, and reasoned about without touching the
    /// BGE-small path.
    struct StubEmbedder {
        dim: usize,
    }

    impl Embedder for StubEmbedder {
        fn dim(&self) -> usize {
            self.dim
        }

        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0; self.dim]).collect())
        }
    }

    #[test]
    fn trait_is_object_safe_and_returns_one_vector_per_input() {
        let e: Box<dyn Embedder> = Box::new(StubEmbedder { dim: 4 });
        assert_eq!(e.dim(), 4);
        let out = e.embed_batch(&["alpha", "beta", "gamma"]).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|v| v.len() == 4));
    }
}
