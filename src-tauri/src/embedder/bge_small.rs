//! fastembed-rs backed [`BGE-small-en-v1.5`] embedder.
//!
//! Loads an ONNX copy of BGE-small from the HuggingFace hub on first use
//! (≈80 MB) and mmap's the weights thereafter. Subsequent calls hit the
//! local cache and are offline-safe. Weights + tokenizer files land under
//! the `cache_dir` passed to [`BgeSmall::new`] - Atlas points that at
//! `<atlas-data>/models/` so the blob doesn't pollute anyone's home dir.
//!
//! [`BGE-small-en-v1.5`]: https://huggingface.co/BAAI/bge-small-en-v1.5

use std::path::PathBuf;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
#[cfg(target_os = "windows")]
use ort::execution_providers::DirectMLExecutionProvider;

use super::{Embedder, EMBEDDING_DIM};

pub struct BgeSmall {
    inner: TextEmbedding,
}

impl BgeSmall {
    /// Build the embedder. `cache_dir` is the directory fastembed writes
    /// weights + tokenizer files to; it's created if missing. First call
    /// on a fresh cache downloads the model (~80 MB) - block on that in a
    /// background task, not a UI handler.
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating model cache {}", cache_dir.display()))?;

        // On Windows, try DirectML (GPU) first; fall back to CPU if it
        // fails to initialize. DirectML is Windows-native and ships with
        // the OS, so on a Windows machine with any DX12 GPU it should
        // always succeed. On non-Windows targets there is no DirectML
        // library to link against, so we skip the GPU attempt entirely
        // and go straight to CPU. Indexing is slower but functional.
        #[cfg(target_os = "windows")]
        {
            let gpu_opts = InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache_dir.clone())
                .with_show_download_progress(false)
                .with_execution_providers(vec![
                    DirectMLExecutionProvider::default().build(),
                ]);

            match TextEmbedding::try_new(gpu_opts) {
                Ok(inner) => {
                    tracing::info!("BGE-small embedder running on DirectML (GPU)");
                    return Ok(Self { inner });
                }
                Err(gpu_err) => {
                    tracing::warn!(
                        error = %gpu_err,
                        "DirectML init failed, falling back to CPU embedder"
                    );
                }
            }
        }

        let cpu_opts = InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let inner = TextEmbedding::try_new(cpu_opts)
            .context("loading BGE-small-en-v1.5 via fastembed-rs (CPU fallback)")?;
        Ok(Self { inner })
    }
}

impl Embedder for BgeSmall {
    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // fastembed's embed<S: AsRef<str>> takes Vec<S>. Passing Vec<&str>
        // avoids cloning the inputs - each &str stays borrowed from the
        // caller's chunk text.
        let owned: Vec<&str> = texts.to_vec();
        // `None` = fastembed's default internal batch size (256 at time of
        // writing). We pass our whole slice in one call and let it batch
        // internally; callers that want different batching can chunk the
        // input before handing it to us.
        let vectors = self
            .inner
            .embed(owned, None)
            .context("fastembed embed failed")?;

        // Defensive dim check. fastembed is supposed to return 384-dim
        // for BGE-small; if some future version changes the default
        // pooling and silently returns something else, fail loud.
        if let Some(first) = vectors.first() {
            if first.len() != EMBEDDING_DIM {
                anyhow::bail!(
                    "expected {EMBEDDING_DIM}-dim vectors, got {}",
                    first.len()
                );
            }
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    /// Shared model instance for real-model tests. Cargo runs tests in
    /// parallel by default; hf-hub takes a file lock during download, so
    /// racing three tests against a cold cache trips the lock and the
    /// losers fail. Loading once through `OnceLock` dodges that AND
    /// reflects how the indexer will actually use BgeSmall - construct
    /// once per run, call `embed_batch` many times.
    fn shared() -> &'static BgeSmall {
        static MODEL: OnceLock<BgeSmall> = OnceLock::new();
        MODEL.get_or_init(|| {
            let cache = std::env::temp_dir().join("atlas-fastembed-test-cache");
            BgeSmall::new(cache).expect("load BGE-small")
        })
    }

    #[test]
    #[ignore = "downloads ~80MB model on first run; opt-in via --ignored"]
    fn loads_model_and_embeds_single_text() {
        let e = shared();
        assert_eq!(e.dim(), EMBEDDING_DIM);

        let out = e
            .embed_batch(&["PlayerRef.sendMessage sends a chat message to a player"])
            .expect("embed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), EMBEDDING_DIM);

        // BGE-small's fastembed default is L2-normalized. If that ever
        // changes we want to know - downstream cosine == dot assumes it.
        let norm_sq: f32 = out[0].iter().map(|x| x * x).sum();
        let norm = norm_sq.sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "expected unit-norm vector, got |v| = {norm}"
        );

        // A non-degenerate vector has at least a few non-zero dims.
        let nonzero = out[0].iter().filter(|x| x.abs() > 1e-6).count();
        assert!(nonzero > 50, "suspicious near-zero vector");
    }

    #[test]
    #[ignore = "downloads ~80MB model on first run; opt-in via --ignored"]
    fn semantically_similar_texts_are_closer_than_unrelated() {
        // Sanity-check that the model is actually doing semantics - not
        // just returning something shaped like a vector. If this ever
        // fails with the defaults, the model or pooling changed under us.
        let e = shared();

        let texts = [
            "send a chat message to the player",  // 0 - query-shaped
            "PlayerRef.sendMessage(String msg)",  // 1 - related API
            "compile a regex pattern against a string", // 2 - unrelated
        ];
        let v = e.embed_batch(&texts.iter().copied().collect::<Vec<_>>()).unwrap();

        let cosine = |a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b).map(|(x, y)| x * y).sum()
        };
        let related = cosine(&v[0], &v[1]);
        let unrelated = cosine(&v[0], &v[2]);

        assert!(
            related > unrelated,
            "related ({related:.3}) should outscore unrelated ({unrelated:.3})"
        );
    }

    /// Opt-in throughput probe. Measures chunks/sec for a single call of
    /// ~length-of-a-method text. Results are informational, not assertive
    /// - hardware varies - but the plan budget is a full re-index of
    /// ~55k chunks in < 5 minutes (≈185 chunks/sec).
    ///
    ///   cargo test --lib --release \
    ///     embedder::bge_small::tests::throughput_probe \
    ///     -- --ignored --nocapture
    #[test]
    #[ignore = "benchmark-flavored; opt-in via --ignored"]
    fn throughput_probe() {
        let e = shared();
        let text = "public void sendMessage(PlayerRef player, String message) {\n    \
                    if (player == null) return;\n    \
                    player.sendChat(message);\n}";
        let n = 200usize;
        let batch: Vec<&str> = std::iter::repeat(text).take(n).collect();

        // Warm-up: first call may lazy-initialize tokenizer / ORT session.
        let _ = e.embed_batch(&batch[..4]).unwrap();

        let start = std::time::Instant::now();
        let vectors = e.embed_batch(&batch).expect("embed batch");
        let elapsed = start.elapsed();

        assert_eq!(vectors.len(), n);
        let per_ms = elapsed.as_secs_f64() * 1000.0 / n as f64;
        let per_sec = n as f64 / elapsed.as_secs_f64();
        eprintln!(
            "\nBGE-small throughput: {n} chunks in {:?} \
             ({per_ms:.2} ms/chunk, {per_sec:.0} chunks/sec)",
            elapsed
        );
    }
}
