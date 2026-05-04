//! LLM-driven chunk summarization for embedding-time enrichment.
//!
//! The decompiled Hytale server has no Javadoc - class and method names
//! survive but everything else is lost in the JAR roundtrip. That means
//! the BGE-small embedder is matching natural-language queries against
//! pure identifier soup. A query like "how do I teleport the player"
//! has nothing to lock onto when the matching method is named `m1234`
//! or even `setLocation`.
//!
//! This module fills that gap: for each public class / method / ctor
//! chunk produced by [`crate::indexer::chunker`], we ask Claude to write
//! a one-sentence summary of what the code DOES, then prepend that
//! sentence to the chunk text BEFORE the embedder sees it. The vector
//! that lands in Lance is now anchored on intent (plain English) rather
//! than identifier collisions, which is the actual fix for the
//! "100 results, none clearly right" search-quality problem.
//!
//! Summaries are generated once at central-build time (in `atlas-build`),
//! NOT on the desktop client - every user benefits from a single API
//! spend by HM. A SHA256-keyed disk cache means re-running the build on
//! the same decompile tree is free; only diffs between Hytale releases
//! cost real tokens.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::indexer::chunker::{Chunk, ChunkKind};

/// One-sentence summary of what a chunk does. Implementations are
/// expected to be cheap to call repeatedly - cache internally so the
/// indexer can call this on every chunk without thinking.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, chunk: &Chunk) -> Result<String>;
}

/// Anthropic Claude-backed summarizer. Hits the Messages API directly;
/// no SDK because rmcp + reqwest already cover that surface and pulling
/// in another HTTP wrapper for one endpoint isn't worth it.
///
/// Caching: every successful response is written to
/// `<cache_dir>/<aa>/<sha256>.txt` where `aa` is the first two hex chars
/// of the chunk-text hash. The shard prefix keeps directories small
/// when the section has tens of thousands of chunks.
pub struct AnthropicSummarizer {
    api_key: String,
    model: String,
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl AnthropicSummarizer {
    /// Default model - Haiku 4.5. Cheap (~$1/M input, ~$5/M output),
    /// fast, and quality is plenty for one-sentence "what does this do"
    /// summaries. If quality is ever a problem on a specific section
    /// (e.g. very dense rendering code), swap to Sonnet via
    /// [`Self::with_model`] at the cost of ~10× the spend.
    pub const DEFAULT_MODEL: &'static str = "claude-haiku-4-5-20251001";

    /// Prompt version. Bump when [`build_prompt`] changes in a way that
    /// should invalidate cached summaries. The version is mixed into the
    /// cache key, so old cache entries are silently orphaned (not deleted)
    /// when this changes - easy to roll back by reverting the bump.
    ///
    /// v1: declarative one-sentence summary starting with "Computes" /
    ///     "A class that".
    /// v2: query-shaped two-sentence summary (user-facing verb + "Used
    ///     when …" framing) - calibrated against the 4 NL-query MISSes
    ///     in the 29-query golden set on 2026-04-30.
    pub const PROMPT_VERSION: u32 = 2;

    pub fn new(api_key: String, cache_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&cache_dir).with_context(|| {
            format!("creating summary cache dir {}", cache_dir.display())
        })?;
        Ok(Self {
            api_key,
            model: Self::DEFAULT_MODEL.to_string(),
            client: reqwest::Client::new(),
            cache_dir,
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    fn cache_path(&self, hash: &str) -> PathBuf {
        // sha256 → 64 hex chars; first two for sharding is plenty.
        self.cache_dir.join(&hash[..2]).join(format!("{hash}.txt"))
    }

    fn read_cache(&self, hash: &str) -> Option<String> {
        fs::read_to_string(self.cache_path(hash)).ok()
    }

    fn write_cache(&self, hash: &str, summary: &str) -> Result<()> {
        let path = self.cache_path(hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("creating cache shard {}", parent.display())
            })?;
        }
        fs::write(&path, summary)
            .with_context(|| format!("writing cache {}", path.display()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Summarizer for AnthropicSummarizer {
    async fn summarize(&self, chunk: &Chunk) -> Result<String> {
        let hash = hash_text(&chunk.text, Self::PROMPT_VERSION);
        if let Some(cached) = self.read_cache(&hash) {
            return Ok(cached);
        }

        let prompt = build_prompt(chunk);
        let req = AnthropicRequest {
            model: &self.model,
            max_tokens: 200,
            messages: vec![AnthropicMessage {
                role: "user",
                content: &prompt,
            }],
        };

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&req)
            .send()
            .await
            .context("calling anthropic messages api")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_else(|_| "<unreadable>".into());
            anyhow::bail!("anthropic api returned {status}: {body}");
        }

        let parsed: AnthropicResponse =
            resp.json().await.context("parsing anthropic response")?;

        let summary = parsed
            .content
            .iter()
            .find(|c| c.kind == "text")
            .and_then(|c| c.text.as_deref())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("anthropic response had no text content"))?;

        if summary.is_empty() {
            anyhow::bail!("anthropic returned empty summary");
        }

        self.write_cache(&hash, &summary)?;
        Ok(summary)
    }
}

/// No-op summarizer used by tests and `--dry-run` paths. Returns a
/// synthetic sentence so call sites can exercise the prepend pathway
/// without an API key or network.
pub struct StubSummarizer;

#[async_trait::async_trait]
impl Summarizer for StubSummarizer {
    async fn summarize(&self, chunk: &Chunk) -> Result<String> {
        Ok(format!(
            "Stub summary for {} ({}).",
            if chunk.symbol_name.is_empty() {
                "<anon>"
            } else {
                chunk.symbol_name.as_str()
            },
            chunk.kind.as_str()
        ))
    }
}

/// Whether a given chunk is worth spending tokens on. This is the
/// primary cost lever: skipping `File` fallbacks alone keeps a typical
/// build from ballooning when tree-sitter chokes on a few sources.
///
/// Future tightening (deferred until access-modifier info is plumbed
/// through `Chunk`): skip private/package-private methods. That cuts
/// cost roughly 3-5× for the same search-quality lift, since those
/// methods aren't typically what mod authors search for by name.
pub fn should_summarize(chunk: &Chunk) -> bool {
    match chunk.kind {
        ChunkKind::Type | ChunkKind::Method | ChunkKind::Constructor => true,
        ChunkKind::File => false,
    }
}

/// Prepend `summary` to `chunk.text` in the canonical format the
/// embedder ingests. Kept as a free function so call sites stay
/// one-liners and the wire-format is documented in exactly one place.
pub fn inject_summary(chunk: &mut Chunk, summary: &str) {
    chunk.text = format!("// SUMMARY: {summary}\n\n{}", chunk.text);
}

// -- internals -------------------------------------------------------

fn hash_text(text: &str, prompt_version: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    // Domain-separate by prompt version so changes to the prompt
    // produce a fresh cache key. Old cache files are orphaned, not
    // deleted - easy to roll back by reverting the version bump.
    hasher.update(b"\0prompt_v");
    hasher.update(prompt_version.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn build_prompt(chunk: &Chunk) -> String {
    let kind_label = match chunk.kind {
        ChunkKind::Type => "class/interface/enum",
        ChunkKind::Method => "method",
        ChunkKind::Constructor => "constructor",
        ChunkKind::File => "file (parse fallback)",
    };
    format!(
        "You are writing a search-engine summary for one chunk of decompiled \
Java from the Hytale game server. The summary will be embedded as a \
vector and matched against natural-language developer queries like \
\"how do I teleport a player\", \"give item to player\", \"where is \
damage calculated\", \"world generation entry point\", \"check player \
permissions\".\n\n\
Decompiled code may have obfuscated names like `m1234`, `aB`, or \
`field_192_a`; focus on what the code DOES based on its operations and \
control flow, not just on the names.\n\n\
Output exactly TWO sentences. No preface, no markdown, no surrounding \
quotes.\n\n\
Sentence 1: What this code does, in user-facing verb form. Prefer \
\"gives\", \"teleports\", \"damages\", \"calculates\", \"spawns\", \
\"checks\" over \"computes\" or \"returns\" when both fit. For a type, \
start with \"A class that\" / \"An interface that\" / \"An enum that\".\n\n\
Sentence 2: When a developer would search for this. Start with \"Used \
when\" or \"Answers\". Repeat the high-level domain noun-phrase a \
developer would type - e.g. \"world generation\", \"damage \
calculation\", \"permission check\", \"give item to player\", \
\"inventory update\", \"chat message\". This is the part that anchors \
the embedding to natural-language queries; do NOT skip it.\n\n\
Kind: {kind_label}\n\
Symbol: {}\n\
Enclosing FQN: {}\n\n\
Code:\n{}",
        if chunk.symbol_name.is_empty() {
            "<anon>"
        } else {
            chunk.symbol_name.as_str()
        },
        chunk.class_fqn,
        chunk.text
    )
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMessage<'a>>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Best-effort load of an env var from a `.env` file in `project_root`,
/// then from the process environment. Returns `Ok(None)` if neither
/// source has the variable. Used by `atlas-build` so a developer can run
/// `cargo run --bin atlas-build -- index --summarize ...` without
/// exporting the key first.
pub fn load_env_var(project_root: &Path, name: &str) -> Result<Option<String>> {
    let env_path = project_root.join(".env");
    if env_path.is_file() {
        // dotenvy::from_path_iter doesn't mutate process env; we only
        // want the named var, so iterate manually.
        let iter = dotenvy::from_path_iter(&env_path)
            .with_context(|| format!("reading {}", env_path.display()))?;
        for item in iter {
            let (k, v) = item.with_context(|| format!("parsing {}", env_path.display()))?;
            if k == name {
                return Ok(Some(v));
            }
        }
    }
    Ok(std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_chunk(text: &str) -> Chunk {
        Chunk {
            kind: ChunkKind::Method,
            symbol_name: "doThing".into(),
            class_fqn: "com.hytale.Foo".into(),
            start_line: 1,
            end_line: 10,
            text: text.into(),
        }
    }

    #[test]
    fn hash_text_is_stable() {
        let a = hash_text("hello", 1);
        let b = hash_text("hello", 1);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn hash_text_changes_with_prompt_version() {
        let a = hash_text("hello", 1);
        let b = hash_text("hello", 2);
        assert_ne!(a, b);
    }

    #[test]
    fn inject_summary_prepends_marker() {
        let mut c = fake_chunk("public void doThing() { return; }");
        inject_summary(&mut c, "Does the thing.");
        assert!(c.text.starts_with("// SUMMARY: Does the thing.\n\n"));
        assert!(c.text.contains("public void doThing()"));
    }

    #[test]
    fn should_summarize_skips_file_fallback() {
        let mut c = fake_chunk("whatever");
        c.kind = ChunkKind::File;
        assert!(!should_summarize(&c));
        c.kind = ChunkKind::Method;
        assert!(should_summarize(&c));
        c.kind = ChunkKind::Type;
        assert!(should_summarize(&c));
    }

    #[tokio::test]
    async fn stub_summarizer_returns_synthetic() {
        let s = StubSummarizer;
        let c = fake_chunk("body");
        let out = s.summarize(&c).await.unwrap();
        assert!(out.contains("doThing"));
        assert!(out.contains("method"));
    }
}
