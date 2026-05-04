//! `atlas-build`: headless CLI for the central index builder.
//!
//! Subcommands cover the full "decompile in, artifact out" pipeline:
//!
//! - `keygen`: generate an Ed25519 keypair for manifest signing.
//! - `index` : walk a decompile tree and emit `tantivy/`, `lance/`,
//! `symbols.sqlite` into a staging directory.
//! - `pack` : take a staging directory (containing `tantivy/`,
//! `lance/`, `symbols.sqlite`) plus manifest metadata,
//! emit a signed `.tar.zst`. The artifact never ships the
//! decompile tree - clients run Vineflower locally to
//! reconstruct source for preview / `get_source`.
//! - `verify`: open a `.tar.zst` + optional pubkey, check layout +
//! signature + digests. Used by CI as the determinism
//! guard.
//!
//! Typical local loop:
//! 1. `atlas-build index --decompile <src> --staging <dst>`
//! 2. `atlas-build pack --staging <dst> --signing-key <key> ...`
//! 3. `atlas-build verify <artifact>`
//!
//! `index` does NOT yet run Vineflower itself. The caller is responsible
//! for producing the decompile tree (the desktop app already does this;
//! a future `atlas-build decompile <jar>` subcommand will close the
//! loop end-to-end).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use atlas_lib::config::Slot;
use atlas_lib::embedder::SharedEmbedder;
use atlas_lib::fetcher::artifact::{pack, verify, FileEntry, PackRequest, VerifiedArtifact};
use atlas_lib::fetcher::manifest::Manifest;
use atlas_lib::fetcher::signing::{
    embedded_pubkey, fingerprint, generate_keypair, parse_pubkey_hex, sign_manifest,
    verify_manifest,
};
use atlas_lib::indexer::chunker;
use atlas_lib::indexer::metadata::{CHUNKER_VERSION, EMBEDDER_ID, MIN_CLIENT_VERSION, SCHEMA_VERSION};
use atlas_lib::indexer::summarizer::{
    self, AnthropicSummarizer, StubSummarizer, Summarizer,
};
use atlas_lib::indexer::{self, IndexEvent, ProgressSink, SearchCatalog};
use atlas_lib::patcher::vineflower::VINEFLOWER_VERSION;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::SigningKey;

#[derive(Parser)]
#[command(name = "atlas-build")]
#[command(about = "Atlas central index builder CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Resolve the shared cache root for embedder model, HM docs clone,
/// and Hypixel Javadoc mirror. Honored by all subcommands that touch
/// those caches so a rebuild from a new staging dir doesn't re-download
/// or re-clone. Resolution order:
/// 1. Explicit `--cache-root` (caller may pass `Some`)
/// 2. `atlas_lib::cache_root` (env var, then platform cache dir)
fn resolve_cache_root(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(|p| p.to_path_buf())
        .unwrap_or_else(atlas_lib::cache_root)
}

#[derive(Subcommand)]
enum Command {
 /// Generate an Ed25519 signing keypair. Private key goes to
 /// `--out-private` (pkcs8 PEM); public key's hex goes to
 /// `--out-public`. DO NOT check the private key into git.
    Keygen {
        #[arg(long)]
        out_private: PathBuf,
        #[arg(long)]
        out_public: PathBuf,
    },

 /// Walk a decompile tree and write Tantivy + Lance + symbols into a
 /// staging directory. The output layout matches what `pack` expects:
 /// `<staging>/tantivy/`, `<staging>/lance/`, plus the symbols sidecar
 /// inside `<staging>/tantivy/symbols.sqlite`. The decompile tree is
 /// NOT copied into the staging directory - the artifact ships only
 /// the index, never the source. Clients re-run Vineflower locally
 /// against the JAR they already have on disk for source preview.
    Index {
 /// Path to the decompile output (a directory tree of `.java`
 /// files organised by package).
        #[arg(long)]
        decompile: PathBuf,

 /// Output staging directory. Will be created if it does not
 /// exist. Existing `tantivy/` and `lance/` subdirectories are
 /// wiped.
        #[arg(long)]
        staging: PathBuf,

 /// Slot label baked into Tantivy docs. Mostly cosmetic for
 /// fetched artifacts; kept for compatibility with the desktop
 /// app's two-slot UI. Defaults to `release`.
        #[arg(long, default_value = "release")]
        slot: String,

 /// Enable LLM summary injection: every public chunk gets a
 /// one-sentence summary prepended before embedding. Closes the
 /// "no Javadoc" gap in the decompiled section. Reads the API
 /// key from `ANTHROPIC_API_KEY` (env or `.env`); summaries are
 /// cached on disk so re-runs on the same decompile are free.
        #[arg(long, default_value_t = false)]
        summarize: bool,

 /// Override the Anthropic model used by `--summarize`. Defaults
 /// to Haiku 4.5 (cheap, fast).
        #[arg(long)]
        summarize_model: Option<String>,

 /// Cache directory for summaries. Defaults to
 /// `<staging>/.summary-cache`. Pin somewhere stable to share
 /// the cache across staging directories.
        #[arg(long)]
        summarize_cache: Option<PathBuf>,

 /// Path to a local clone of the HM docs repo
 /// (https://github.com/HytaleModding/site). Local-dev escape
 /// hatch: prefer `--hm-docs-fetch` for production runs so the
 /// indexer always pulls the latest published guides. When both
 /// are set, this explicit path wins.
        #[arg(long)]
        hm_docs: Option<PathBuf>,

 /// Auto-fetch the HM docs repo from
 /// <https://github.com/HytaleModding/site> before indexing.
 /// Shallow-clones into `<staging>/.hm-docs-cache/site/`, wiping
 /// any prior clone so re-runs always pick up new commits.
 /// Requires `git` on PATH. The fetched commit SHA is printed
 /// so the build log records what was indexed.
        #[arg(long, default_value_t = false)]
        hm_docs_fetch: bool,

 /// Path to a directory of mirrored Hypixel Javadoc HTML (the
 /// release.server.docs.hytale.com / prerelease.* trees). CI is
 /// expected to mirror via `wget --mirror --no-parent
 /// --no-host-directories` before invoking this command. Each
 /// recognised class page is added as `source_type = "hypixel_doc"`.
        #[arg(long)]
        hypixel_docs: Option<PathBuf>,

 /// Convenience: fetch Hypixel Javadocs from these hosts into a
 /// cache directory before walking. Repeat to pull both
 /// release + prerelease. When given alongside `--hypixel-docs`,
 /// the cache directory is `<hypixel-docs>/<host-slug>/`.
 /// Local-dev only - production CI should use `wget --mirror`.
        #[arg(long = "hypixel-docs-fetch")]
        hypixel_docs_fetch: Vec<String>,

 /// Root for shared caches (embedder model, HM docs clone,
 /// Hypixel Javadoc mirror). Survives across staging dirs so
 /// rebuilds don't re-download / re-clone. Defaults to the
 /// `ATLAS_CACHE_ROOT` env var if set, otherwise the platform
 /// cache dir resolved by `directories::ProjectDirs`.
 /// Subdirectories used: `models/`, `hm-docs/site/`,
 /// `javadocs/<host-slug>/`.
        #[arg(long)]
        cache_root: Option<PathBuf>,
    },

 /// Surgically refresh a single section inside an existing staging
 /// directory. Opens the staging Tantivy + Lance, deletes every row
 /// whose `source_type` matches `--source-type`, re-walks the
 /// section's source path, and appends fresh chunks. The other
 /// sections (Java source, Hypixel Javadocs, etc.) are untouched.
 ///
 /// Today only `--source-type hm_doc` is supported - that's the
 /// section whose walker change most often, and it's small
 /// enough (~150 files) that re-embedding is a few minutes rather
 /// than the 30+ a full rebuild takes. Other source types fall back
 /// to the full `index` pass.
 ///
 /// Refuses to run if the staging dir's `atlas-meta.json`
 /// `embedder_id` or `chunker_version` differs from the current
 /// build's: an in-place add-section only makes sense when the new
 /// rows are produced by the same chunker + embedder as the rows
 /// already on disk, otherwise hybrid search is comparing apples to
 /// oranges.
    AddSection {
 /// Existing staging directory (from a prior `index` run) to
 /// modify in place. Must contain `tantivy/` and `lance/`.
        #[arg(long)]
        staging: PathBuf,

 /// Which section to refresh. Currently only `hm_doc` is
 /// supported; the other variants exist for future use.
        #[arg(long, default_value = "hm_doc")]
        source_type: String,

 /// Path to a local clone of the HM docs repo. Local-dev escape
 /// hatch - prefer `--hm-docs-fetch` for production runs.
 /// Required (or `--hm-docs-fetch`) when `--source-type hm_doc`.
        #[arg(long)]
        hm_docs: Option<PathBuf>,

 /// Auto-fetch the HM docs repo before re-ingesting. Same shape
 /// as `index --hm-docs-fetch`.
        #[arg(long, default_value_t = false)]
        hm_docs_fetch: bool,

 /// Slot label baked into newly written chunks. Must match the
 /// slot the staging dir was originally indexed with -
 /// otherwise the new HM doc rows won't be visible to a search
 /// scoped to the original slot.
        #[arg(long, default_value = "release")]
        slot: String,

 /// Shared cache root, same semantics as `index --cache-root`.
        #[arg(long)]
        cache_root: Option<PathBuf>,
    },

 /// Pack a staging directory into a signed `.tar.zst` artifact.
    Pack {
 /// Directory containing `tantivy/`, `lance/`, and
 /// `symbols.sqlite`. Everything under it ships in the artifact.
 /// The artifact intentionally does NOT include `decompile/`;
 /// shipping decompiled source is a license / compliance issue
 /// (see `docs/legal-spec/what-the-artifact-contains.md`).
        #[arg(long)]
        staging: PathBuf,

 /// Output path for the artifact (`.tar.zst`).
        #[arg(long)]
        out: PathBuf,

 /// Signing key path (pkcs8 PEM). If omitted, the artifact is
 /// emitted unsigned (useful for local dev / determinism tests).
        #[arg(long)]
        signing_key: Option<PathBuf>,

 /// Hytale `Implementation-Version` this artifact was built for.
        #[arg(long)]
        hytale_impl_version: String,

 /// Hytale patchline - `release` or `pre-release`.
        #[arg(long)]
        hytale_patchline: Option<String>,

 /// Build id slug, e.g. `release-89796e57b`. Becomes the key in
 /// the client's SearchCatalog.
        #[arg(long)]
        build_id: String,
    },

 /// Open a staging directory's Tantivy index and run a keyword
 /// search against it. Sanity check: confirms the index is queryable
 /// without booting the desktop app or extracting the artifact.
    Search {
 /// Staging directory previously written by `atlas-build index`
 /// (must contain a `tantivy/` subdirectory with `atlas-meta.json`).
        #[arg(long)]
        staging: PathBuf,

 /// Query text. Tantivy query syntax - e.g.
 /// `PageManager`, `getComponent OR setComponent`, `package:com.foo`.
        #[arg(long)]
        query: String,

 /// Max hits to print.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

 /// Hybrid (BM25 + vector) search against a staging directory. Mirrors
 /// what the desktop `search` command does end-to-end to A/B
 /// search quality between two staging dirs (e.g. summarized vs. raw)
 /// without running Tauri. Falls back to keyword-only if the staging
 /// dir has no Lance store.
    HybridSearch {
 /// Staging directory previously written by `atlas-build index`.
 /// Must contain `tantivy/`; `lance/` is optional (keyword-only
 /// fallback if missing).
        #[arg(long)]
        staging: PathBuf,

 /// Query text - natural-language or symbol-like; the blender
 /// auto-picks weights from query shape.
        #[arg(long)]
        query: String,

 /// Max hits to print.
        #[arg(long, default_value_t = 10)]
        limit: usize,

 /// Slot label baked into the query path. Must match what `index`
 /// was run with. Defaults to `release`.
        #[arg(long, default_value = "release")]
        slot: String,
    },

 /// Sanity-check the LLM summarizer against a single Java source
 /// file. Chunks the file, runs each chunk through the summarizer,
 /// and prints `kind | symbol → summary` lines so you can eyeball
 /// quality before paying for a full section pass.
 ///
 /// Reads the API key from `ANTHROPIC_API_KEY` in `.env` (relative
 /// to the working directory) or the process environment. Pass
 /// `--stub` to use the no-op summarizer (no network, no spend) for
 /// pipeline-plumbing tests.
    SummarizeTest {
 /// Path to a single `.java` file.
        #[arg(long)]
        file: PathBuf,

 /// Java package name to attribute chunks to. Defaults to
 /// `test.package` - only matters for the printed FQN.
        #[arg(long, default_value = "test.package")]
        package: String,

 /// Use the StubSummarizer (no API call, synthetic output).
        #[arg(long, default_value_t = false)]
        stub: bool,

 /// Override the Anthropic model. Defaults to Haiku 4.5.
        #[arg(long)]
        model: Option<String>,

 /// Cache directory. Defaults to `<file's parent>/.summary-cache`.
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },

 /// Run a golden query set against a staging directory and write a
 /// search-quality report (Top-1 / Top-3 / MRR + per-query detail).
 /// Pass `--diff <prev_report.json>` to compare against an earlier
 /// run. The report JSON is the canonical artifact for tracking
 /// search-quality regressions across pipeline tweaks.
    Eval {
 /// Staging directory previously written by `atlas-build index`.
        #[arg(long)]
        staging: PathBuf,

 /// Golden query set (JSON). See `eval/queries.json` for the
 /// canonical seed file.
        #[arg(long)]
        queries: PathBuf,

 /// Output report path (`.json`). If omitted, the report is
 /// printed to stdout but not persisted.
        #[arg(long)]
        out: Option<PathBuf>,

 /// Path to a previous report (`.json`). If given, a per-query
 /// + summary delta is appended after the current report.
        #[arg(long)]
        diff: Option<PathBuf>,

 /// Slot label baked into the query path. Must match `index`.
        #[arg(long, default_value = "release")]
        slot: String,

 /// Hits to fetch per query. Top-1 / Top-3 / MRR are computed
 /// against this slice.
        #[arg(long, default_value_t = 10)]
        top_k: usize,
    },

 /// Verify an artifact's layout, digests, and (if a pubkey is given)
 /// signature. Exits non-zero on any verification failure. Pubkey
 /// defaults to the pubkey embedded in this binary; pass
 /// `--pubkey <path>` to verify against a different one.
    Verify {
 /// Path to the `.tar.zst` artifact.
        artifact: PathBuf,

 /// Path to a hex-encoded pubkey file. If omitted, the pubkey
 /// embedded at compile time via `include_str!` is used.
        #[arg(long)]
        pubkey: Option<PathBuf>,

 /// Skip signature verification - use only for local dev on
 /// unsigned artifacts.
        #[arg(long)]
        unsigned: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

 // Tantivy spawns its own indexing threads via `std::thread::spawn`.
 // When one panics it surfaces as "An index writer was killed" in the
 // main thread, hiding the underlying cause. Print the original
 // panic + thread name to stderr so failures are diagnosable.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        eprintln!(
            "PANIC in thread '{}': {}",
            thread.name().unwrap_or("<unnamed>"),
            info
        );
        prev_hook(info);
    }));

    let cli = Cli::parse();
    match cli.command {
        Command::Keygen {
            out_private,
            out_public,
        } => cmd_keygen(&out_private, &out_public),
        Command::Index {
            decompile,
            staging,
            slot,
            summarize,
            summarize_model,
            summarize_cache,
            hm_docs,
            hm_docs_fetch,
            hypixel_docs,
            hypixel_docs_fetch,
            cache_root,
        } => cmd_index(
            &decompile,
            &staging,
            &slot,
            summarize,
            summarize_model.as_deref(),
            summarize_cache.as_deref(),
            hm_docs.as_deref(),
            hm_docs_fetch,
            hypixel_docs.as_deref(),
            &hypixel_docs_fetch,
            cache_root.as_deref(),
        ),
        Command::AddSection {
            staging,
            source_type,
            hm_docs,
            hm_docs_fetch,
            slot,
            cache_root,
        } => cmd_add_section(
            &staging,
            &source_type,
            hm_docs.as_deref(),
            hm_docs_fetch,
            &slot,
            cache_root.as_deref(),
        ),
        Command::Pack {
            staging,
            out,
            signing_key,
            hytale_impl_version,
            hytale_patchline,
            build_id,
        } => cmd_pack(
            &staging,
            &out,
            signing_key.as_deref(),
            &hytale_impl_version,
            hytale_patchline.as_deref(),
            &build_id,
        ),
        Command::Search {
            staging,
            query,
            limit,
        } => cmd_search(&staging, &query, limit),
        Command::HybridSearch {
            staging,
            query,
            limit,
            slot,
        } => cmd_hybrid_search(&staging, &query, limit, &slot),
        Command::SummarizeTest {
            file,
            package,
            stub,
            model,
            cache_dir,
        } => cmd_summarize_test(&file, &package, stub, model.as_deref(), cache_dir.as_deref()),
        Command::Eval {
            staging,
            queries,
            out,
            diff,
            slot,
            top_k,
        } => cmd_eval(
            &staging,
            &queries,
            out.as_deref(),
            diff.as_deref(),
            &slot,
            top_k,
        ),
        Command::Verify {
            artifact,
            pubkey,
            unsigned,
        } => cmd_verify(&artifact, pubkey.as_deref(), unsigned),
    }
}

/// [`ProgressSink`] that prints each event to stdout. Plain text, not
/// JSON: this CLI is meant for humans + CI logs, not for piping into
/// another tool. If a structured progress stream is ever needed, add a
/// `--json` flag and a second sink.
struct StdoutSink;

impl ProgressSink for StdoutSink {
    fn emit(&self, event: IndexEvent) {
        match event {
            IndexEvent::Phase(phase) => {
                println!("phase: {}", phase.as_str());
            }
            IndexEvent::Progress {
                current,
                total,
                chunks,
            } => {
                println!("progress: {current}/{total} files, {chunks} chunks");
            }
            IndexEvent::Done { docs } => {
                println!("done: {docs} files indexed");
            }
        }
    }
}

fn cmd_index(
    decompile: &Path,
    staging: &Path,
    slot_label: &str,
    summarize: bool,
    summarize_model: Option<&str>,
    summarize_cache: Option<&Path>,
    hm_docs: Option<&Path>,
    hm_docs_fetch: bool,
    hypixel_docs: Option<&Path>,
    hypixel_docs_fetch: &[String],
    cache_root: Option<&Path>,
) -> Result<()> {
    if !decompile.is_dir() {
        bail!(
            "decompile path is not a directory: {}",
            decompile.display()
        );
    }

    let slot = match slot_label {
        "release" => Slot::Release,
        "pre-release" | "prerelease" => Slot::PreRelease,
        other => bail!("unknown slot {other:?}; expected 'release' or 'pre-release'"),
    };

    fs::create_dir_all(staging)
        .with_context(|| format!("creating staging dir {}", staging.display()))?;

 // Shared caches root - survives across staging dirs so rebuilds
 // don't re-download the embedder, re-clone HM docs, or re-mirror
 // the Hypixel Javadocs. See `resolve_cache_root` for the resolution
 // order (CLI flag → env → Windows dev default).
    let resolved_cache_root = resolve_cache_root(cache_root);
    fs::create_dir_all(&resolved_cache_root).with_context(|| {
        format!(
            "creating shared cache root {}",
            resolved_cache_root.display()
        )
    })?;
    println!("cache root:       {}", resolved_cache_root.display());

 // Tantivy + symbols sidecar live under <staging>/tantivy/.
 // Lance lives under <staging>/lance/. The packer walks the entire
 // staging tree, so anything else here ships in the artifact too.
    let index_dir = staging.join("tantivy");
    let lance_dir = staging.join("lance");

 // Embedder model cache lives at <cache-root>/models/ so don't
 // re-download BGE-small on every CI run. ~80 MB once.
    let model_cache = resolved_cache_root.join("models");
    fs::create_dir_all(&model_cache)
        .with_context(|| format!("creating model cache {}", model_cache.display()))?;

    let shared = SharedEmbedder::new();
    let embedder = shared
        .get_or_init(model_cache)
        .context("loading BGE-small embedder")?;

    let sink: Arc<dyn ProgressSink> = Arc::new(StdoutSink);

 // Optional LLM summarizer. Built once per `index` run; the indexer
 // calls `.summarize` per chunk, the impl handles caching internally.
    let summarizer_arc: Option<Arc<dyn summarizer::Summarizer>> = if summarize {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let env_anchor = find_env_root(&cwd).unwrap_or(cwd);
        let api_key = summarizer::load_env_var(&env_anchor, "ANTHROPIC_API_KEY")?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "ANTHROPIC_API_KEY not found in env or {}/.env",
                    env_anchor.display()
                )
            })?;
        let cache = summarize_cache
            .map(PathBuf::from)
            .unwrap_or_else(|| staging.join(".summary-cache"));
        let mut s = AnthropicSummarizer::new(api_key, cache)?;
        if let Some(m) = summarize_model {
            s = s.with_model(m);
        }
        println!(
            "summarizer enabled (model: {})",
            summarize_model.unwrap_or(AnthropicSummarizer::DEFAULT_MODEL)
        );
        Some(Arc::new(s))
    } else {
        None
    };

 // Build a multi-threaded runtime. The indexer relies on
 // `tokio::task::spawn_blocking` for the CPU-heavy build loop and
 // calls `rt.block_on(lance.add_batch(...))` from inside that
 // blocking task; both require a multi-thread runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("atlas-build")
        .build()
        .context("building tokio runtime")?;

 // Optional in-process Javadoc mirror - only fires when the user
 // passed `--hypixel-docs-fetch <host>` (one or more times). Pulls
 // each host into a sub-dir of the cache root so release + prerelease
 // coexist. Production CI should `wget --mirror` instead and just
 // point `--hypixel-docs` at the result.
    let hypixel_docs_owned = if !hypixel_docs_fetch.is_empty() {
        let cache_root = hypixel_docs
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| resolved_cache_root.join("javadocs"));
        fs::create_dir_all(&cache_root)
            .with_context(|| format!("creating hypixel cache {}", cache_root.display()))?;
        let host_refs: Vec<&str> = hypixel_docs_fetch.iter().map(String::as_str).collect();
        let results = rt
            .block_on(atlas_lib::indexer::hypixel_docs::fetch_many_to_cache(
                &host_refs,
                &cache_root,
            ))
            .context("fetching Hypixel Javadocs")?;
        for (sub, n) in &results {
            println!(
                "fetched {} Hypixel Javadoc pages → {}",
                n,
                sub.display()
            );
        }
        Some(cache_root)
    } else {
        hypixel_docs.map(|p| p.to_path_buf())
    };

 // Resolve the effective HM docs path. Explicit `--hm-docs` wins;
 // otherwise `--hm-docs-fetch` shallow-clones the live repo into a
 // cache under the staging directory so re-runs always pick up new
 // commits. Neither flag → no HM docs in the artifact.
    let effective_hm_docs: Option<PathBuf> = if let Some(p) = hm_docs {
        Some(p.to_path_buf())
    } else if hm_docs_fetch {
        Some(fetch_hm_docs(&resolved_cache_root)?)
    } else {
        None
    };

    rt.block_on(indexer::run(
        embedder,
        slot,
        decompile.to_path_buf(),
        index_dir,
        lance_dir,
        sink,
        summarizer_arc,
        effective_hm_docs,
        hypixel_docs_owned,
    ))
    .context("indexer run failed")?;

 // The decompile tree is intentionally NOT copied into the staging
 // directory. Shipping decompiled Hytale source inside the artifact
 // is a license/compliance issue (Hytale Modding's hosting terms
 // require distribute only the search index, not the underlying
 // source). Clients reconstruct source on demand by running Vineflower
 // against the JAR already present on the user's machine.
 //
 // See `docs/legal-spec/what-the-artifact-contains.md` for the full
 // policy.

    println!("staging ready at {}", staging.display());
    Ok(())
}

/// Surgical re-ingest of one section inside an existing staging dir.
/// See [`Command::AddSection`] for the user-visible contract.
fn cmd_add_section(
    staging: &Path,
    source_type_str: &str,
    hm_docs: Option<&Path>,
    hm_docs_fetch: bool,
    slot_label: &str,
    cache_root: Option<&Path>,
) -> Result<()> {
    if !staging.is_dir() {
        bail!("staging path is not a directory: {}", staging.display());
    }
    let index_dir = staging.join("tantivy");
    let lance_dir = staging.join("lance");
    if !index_dir.is_dir() {
        bail!(
            "no tantivy index at {}; run `atlas-build index` first",
            index_dir.display()
        );
    }
    if !lance_dir.is_dir() {
        bail!(
            "no lance store at {}; run `atlas-build index` first",
            lance_dir.display()
        );
    }

    let source_type =
        atlas_lib::indexer::schema::SourceType::from_str(source_type_str).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown source type {source_type_str:?}; expected one of \
                 source / hm_doc / hypixel_doc / asset"
            )
        })?;

    let slot = match slot_label {
        "release" => Slot::Release,
        "pre-release" | "prerelease" => Slot::PreRelease,
        other => bail!("unknown slot {other:?}; expected 'release' or 'pre-release'"),
    };

    let resolved_cache_root = resolve_cache_root(cache_root);
    fs::create_dir_all(&resolved_cache_root).with_context(|| {
        format!(
            "creating shared cache root {}",
            resolved_cache_root.display()
        )
    })?;

 // Embedder cache lives at <cache-root>/models/, same as `index`.
    let model_cache = resolved_cache_root.join("models");
    fs::create_dir_all(&model_cache)
        .with_context(|| format!("creating model cache {}", model_cache.display()))?;
    let shared = SharedEmbedder::new();
    let embedder = shared
        .get_or_init(model_cache)
        .context("loading BGE-small embedder")?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("atlas-add-section")
        .build()
        .context("building tokio runtime")?;

 // Resolve section source path. Today only `hm_doc` is implemented;
 // the indexer will reject anything else after the call. Do the
 // path resolution here rather than inside the indexer so the
 // `--hm-docs-fetch` shallow-clone happens up front and gets logged
 // alongside the rest of the CLI output.
    let hm_docs_path: Option<PathBuf> = if matches!(
        source_type,
        atlas_lib::indexer::schema::SourceType::HmDoc
    ) {
        let resolved = if let Some(p) = hm_docs {
            Some(p.to_path_buf())
        } else if hm_docs_fetch {
            Some(fetch_hm_docs(&resolved_cache_root)?)
        } else {
            bail!("--hm-docs <path> or --hm-docs-fetch is required for --source-type hm_doc");
        };
        if let Some(ref p) = resolved {
            if !p.is_dir() {
                bail!("HM docs path is not a directory: {}", p.display());
            }
            println!("HM docs path:    {}", p.display());
        }
        resolved
    } else {
        None
    };

    let sink: Arc<dyn ProgressSink> = Arc::new(StdoutSink);

    let added = rt
        .block_on(indexer::add_section(
            embedder,
            slot,
            index_dir.clone(),
            lance_dir.clone(),
            sink,
            source_type,
            hm_docs_path,
        ))
        .context("add-section run failed")?;

    println!(
        "add-section done - refreshed {added} `{}` rows in {}",
        source_type.as_str(),
        staging.display()
    );
    Ok(())
}

fn cmd_keygen(out_private: &Path, out_public: &Path) -> Result<()> {
    let (signing_key, verifying_key) = generate_keypair();

 // Private key → pkcs8 PEM. Standard format, readable by most
 // tooling; easy to paste into CI secret stores.
    let pkcs8_pem = signing_key
        .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
        .context("encoding signing key to pkcs8 PEM")?;
    fs::write(out_private, pkcs8_pem.as_bytes())
        .with_context(|| format!("writing private key to {}", out_private.display()))?;

 // Public key → hex with a commented header, matching the shape of
 // the embedded `atlas-pubkey.hex`.
    let pub_hex = hex::encode(verifying_key.as_bytes());
    let fp = fingerprint(verifying_key.as_bytes())?;
    let public_text = format!(
        "# Atlas artifact signing pubkey (Ed25519, 32 bytes / 64 hex chars).\n\
         # Fingerprint (first 16 bytes hex): {fp}\n\
         {pub_hex}\n"
    );
    fs::write(out_public, public_text.as_bytes())
        .with_context(|| format!("writing pubkey to {}", out_public.display()))?;

    println!("wrote private key → {}", out_private.display());
    println!("wrote public key  → {}", out_public.display());
    println!("fingerprint       → {fp}");
    Ok(())
}

/// Shallow-clone the HM docs repo into the shared cache root,
/// wiping any prior clone so re-runs always reflect the latest commits.
/// Returns the path the indexer should walk. Requires `git` on PATH.
fn fetch_hm_docs(cache_root: &Path) -> Result<PathBuf> {
    const REPO_URL: &str = "https://github.com/HytaleModding/site";
    let hm_root = cache_root.join("hm-docs");
    let target = hm_root.join("site");

    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("removing prior clone {}", target.display()))?;
    }
    fs::create_dir_all(&hm_root)
        .with_context(|| format!("creating cache root {}", hm_root.display()))?;

    println!("fetching HM docs: {REPO_URL} → {}", target.display());

    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", REPO_URL])
        .arg(&target)
        .status()
        .context("running `git clone` (is git installed and on PATH?)")?;
    if !status.success() {
        bail!("git clone {REPO_URL} failed (exit {})", status);
    }

 // Capture HEAD sha for traceability - the build log records exactly
 // which commit was indexed. Best-effort: a failure here doesn't
 // invalidate the clone.
    if let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["rev-parse", "HEAD"])
        .output()
    {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let short = &sha[..sha.len().min(12)];
            println!("HM docs commit:   {short}");
        }
    }

    Ok(target)
}

fn cmd_pack(
    staging: &Path,
    out: &Path,
    signing_key_path: Option<&Path>,
    hytale_impl_version: &str,
    hytale_patchline: Option<&str>,
    build_id: &str,
) -> Result<()> {
    if !staging.is_dir() {
        bail!("staging path is not a directory: {}", staging.display());
    }

 // Walk the staging tree and enumerate file entries. Reserved names
 // (`manifest.json`, etc.) at the root must be absent - the packer
 // synthesizes them.
    let entries = walk_staging(staging)?;
    if entries.is_empty() {
        bail!("staging dir {} is empty", staging.display());
    }

 // Build the manifest ONCE so `created_at` (and any other
 // time-sensitive fields) are stable across the signing round-trip.
 // If a key was supplied, pack unsigned → read back the finalized
 // manifest bytes → sign them → re-pack with the signature. Because
 // the manifest value feeding pack() is identical both calls, the
 // only delta pack() introduces is `sha256sums_sha256`, which is a
 // pure function of the staging files → stable → signature survives.
    let (signing_key_opt, fp) = if let Some(path) = signing_key_path {
        let pem = fs::read_to_string(path)
            .with_context(|| format!("reading signing key {}", path.display()))?;
        let signing_key = SigningKey::from_pkcs8_pem(&pem)
            .context("parsing pkcs8 signing key")?;
        let fp = fingerprint(signing_key.verifying_key().as_bytes())?;
        (Some(signing_key), fp)
    } else {
        (None, String::new())
    };

    let base_manifest = make_manifest(build_id, hytale_impl_version, hytale_patchline, fp);

    let signature_bytes = if let Some(mut signing_key) = signing_key_opt {
        let staging_tmp_out = out.with_extension("tar.zst.unsigned");
        let finalized = pack(
            PackRequest {
                files: &entries,
                manifest: base_manifest.clone(),
                signature: None,
            },
            &staging_tmp_out,
        )?;
        let manifest_bytes = finalized.to_bytes()?;
        let sig = sign_manifest(&mut signing_key, &manifest_bytes);
        let _ = fs::remove_file(&staging_tmp_out);
        Some(sig)
    } else {
        None
    };

    let finalized = pack(
        PackRequest {
            files: &entries,
            manifest: base_manifest,
            signature: signature_bytes,
        },
        out,
    )?;

    println!("packed → {}", out.display());
    println!("build_id           {}", finalized.build_id);
    println!("sha256sums_sha256  {}", finalized.sha256sums_sha256);
    if !finalized.signing_pubkey_fingerprint.is_empty() {
        println!(
            "signing_pubkey_fp  {}",
            finalized.signing_pubkey_fingerprint
        );
    }
    Ok(())
}

fn cmd_search(staging: &Path, query: &str, limit: usize) -> Result<()> {
    let index_dir = staging.join("tantivy");
    if !index_dir.is_dir() {
        bail!(
            "no tantivy index at {}; run `atlas-build index` first",
            index_dir.display()
        );
    }

    let catalog = SearchCatalog::new();
    let hits = catalog
        .search(Slot::Release, &index_dir, query, limit)
        .with_context(|| format!("searching {}", index_dir.display()))?;

    if hits.is_empty() {
        println!("(no hits for {query:?})");
        return Ok(());
    }

    println!("{} hits for {query:?}:", hits.len());
    for (i, hit) in hits.iter().enumerate() {
        let line = hit
            .start_line
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        let symbol = if hit.symbol_name.is_empty() {
            String::new()
        } else {
            format!(" [{}]", hit.symbol_name)
        };
        println!(
            "  {:>2}. {:.3}  {}{}{}",
            i + 1,
            hit.score,
            hit.path,
            line,
            symbol
        );
    }
    Ok(())
}

fn cmd_hybrid_search(
    staging: &Path,
    query: &str,
    limit: usize,
    slot_label: &str,
) -> Result<()> {
    let index_dir = staging.join("tantivy");
    let lance_dir = staging.join("lance");
    if !index_dir.is_dir() {
        bail!(
            "no tantivy index at {}; run `atlas-build index` first",
            index_dir.display()
        );
    }

    let slot = match slot_label {
        "release" => Slot::Release,
        "pre-release" | "prerelease" => Slot::PreRelease,
        other => bail!("unknown slot {other:?}; expected 'release' or 'pre-release'"),
    };

 // Embedder is only needed if a Lance store is present; otherwise
 // hybrid::run falls back to keyword-only. Reuse the shared cache
 // root so don't re-download BGE-small per staging dir.
    let model_cache = resolve_cache_root(None).join("models");
    fs::create_dir_all(&model_cache)
        .with_context(|| format!("creating model cache {}", model_cache.display()))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("atlas-hybrid")
        .build()
        .context("building tokio runtime")?;

    let lance_store = rt
        .block_on(atlas_lib::lance::LanceStore::open_existing(&lance_dir))
        .with_context(|| format!("opening lance store at {}", lance_dir.display()))?;

    let embedder: Option<Arc<dyn atlas_lib::embedder::Embedder>> = if lance_store.is_some() {
        let shared = SharedEmbedder::new();
        Some(
            shared
                .get_or_init(model_cache)
                .context("loading BGE-small embedder")?,
        )
    } else {
        println!("(no lance store at {} - keyword-only)", lance_dir.display());
        None
    };

    let catalog = Arc::new(SearchCatalog::new());
    let start = std::time::Instant::now();
    let hits = rt
        .block_on(atlas_lib::search::hybrid::run(
            catalog,
            lance_store,
            embedder,
            slot,
            &index_dir,
            query,
            limit,
            None,
        ))
        .with_context(|| format!("hybrid search against {}", index_dir.display()))?;
    let elapsed_ms = start.elapsed().as_millis();

    if hits.is_empty() {
        println!("(no hits for {query:?} in {elapsed_ms}ms)");
        return Ok(());
    }

    println!(
        "{} hits for {query:?} ({elapsed_ms}ms):",
        hits.len()
    );
    for (i, hit) in hits.iter().enumerate() {
        let line = hit
            .start_line
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        let symbol = if hit.symbol_name.is_empty() {
            String::new()
        } else {
            format!(" [{}]", hit.symbol_name)
        };
        let dbg = hit
            .debug
            .as_ref()
            .map(|d| {
                let bm = d
                    .bm25_score
                    .map(|s| format!("bm25={s:.2}"))
                    .unwrap_or_else(|| "bm25=-".into());
                let vd = d
                    .vector_distance
                    .map(|v| format!("vdist={v:.3}"))
                    .unwrap_or_else(|| "vdist=-".into());
                format!(" ({bm}, {vd})")
            })
            .unwrap_or_default();
        println!(
            "  {:>2}. {:.3}  {}{}{}{}",
            i + 1,
            hit.score,
            hit.path,
            line,
            symbol,
            dbg
        );
        if let Some(preview) = hit.preview.as_deref() {
 // First non-empty line of preview only - keeps the CLI scannable.
            if let Some(first) = preview.lines().find(|l| !l.trim().is_empty()) {
                let trimmed = first.trim();
                let snippet = if trimmed.len() > 120 {
                    format!("{}…", &trimmed[..120])
                } else {
                    trimmed.to_string()
                };
                println!("       {snippet}");
            }
        }
    }
    Ok(())
}

fn cmd_eval(
    staging: &Path,
    queries_path: &Path,
    out: Option<&Path>,
    diff: Option<&Path>,
    slot_label: &str,
    top_k: usize,
) -> Result<()> {
    use atlas_lib::eval::{self, EvalConfig, EvalReport, GoldenSet};

    let index_dir = staging.join("tantivy");
    let lance_dir = staging.join("lance");
    if !index_dir.is_dir() {
        bail!(
            "no tantivy index at {}; run `atlas-build index` first",
            index_dir.display()
        );
    }

    let slot = match slot_label {
        "release" => Slot::Release,
        "pre-release" | "prerelease" => Slot::PreRelease,
        other => bail!("unknown slot {other:?}; expected 'release' or 'pre-release'"),
    };

    let raw = fs::read_to_string(queries_path)
        .with_context(|| format!("reading queries file {}", queries_path.display()))?;
    let set: GoldenSet = serde_json::from_str(&raw)
        .with_context(|| format!("parsing queries file {}", queries_path.display()))?;
    if set.queries.is_empty() {
        bail!("queries file is empty: {}", queries_path.display());
    }

    let model_cache = resolve_cache_root(None).join("models");
    fs::create_dir_all(&model_cache)
        .with_context(|| format!("creating model cache {}", model_cache.display()))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("atlas-eval")
        .build()
        .context("building tokio runtime")?;

 // Lance is optional - keyword-only fallback is still a meaningful
 // baseline to eval against. Probe by opening once; if the dir's
 // there re-opens per query during eval.
    let has_lance = rt
        .block_on(atlas_lib::lance::LanceStore::open_existing(&lance_dir))
        .with_context(|| format!("probing lance store at {}", lance_dir.display()))?
        .is_some();

    let embedder: Option<Arc<dyn atlas_lib::embedder::Embedder>> = if has_lance {
        let shared = SharedEmbedder::new();
        Some(
            shared
                .get_or_init(model_cache.clone())
                .context("loading BGE-small embedder")?,
        )
    } else {
        println!("(no lance store at {} - keyword-only eval)", lance_dir.display());
        None
    };

    let catalog = Arc::new(SearchCatalog::new());
    let config = EvalConfig {
        top_k,
        ..EvalConfig::default()
    };

    let lance_arg = if has_lance { Some(lance_dir.as_path()) } else { None };
    let report = rt
        .block_on(eval::run_eval(
            &set,
            catalog,
            lance_arg,
            embedder,
            slot,
            &index_dir,
            &config,
            staging.display().to_string(),
            queries_path.display().to_string(),
        ))
        .context("running eval")?;

    eval::print_report(&report);

    if let Some(prev_path) = diff {
        let prev_raw = fs::read_to_string(prev_path)
            .with_context(|| format!("reading prev report {}", prev_path.display()))?;
        let prev: EvalReport = serde_json::from_str(&prev_raw)
            .with_context(|| format!("parsing prev report {}", prev_path.display()))?;
        eval::print_diff(&prev, &report);
    }

    if let Some(out_path) = out {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating output dir {}", parent.display()))?;
            }
        }
        let json = serde_json::to_string_pretty(&report).context("serializing report")?;
        fs::write(out_path, json)
            .with_context(|| format!("writing report to {}", out_path.display()))?;
        println!();
        println!("Report written to {}", out_path.display());
    }

    Ok(())
}

fn cmd_summarize_test(
    file: &Path,
    package: &str,
    stub: bool,
    model: Option<&str>,
    cache_dir: Option<&Path>,
) -> Result<()> {
    if !file.is_file() {
        bail!("not a file: {}", file.display());
    }
    let source = fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?;
    let chunks = chunker::chunk_file(&source, package);
    if chunks.is_empty() {
        println!("(no chunks produced - tree-sitter parse may have failed)");
        return Ok(());
    }

    let cache = cache_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            file.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(".summary-cache")
        });

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    let summarizer: Box<dyn Summarizer> = if stub {
        Box::new(StubSummarizer)
    } else {
 // Walk up from the cwd to find the nearest `.env` (the file
 // path being summarized is unrelated to where project secrets
 // live). Falls back to the cwd itself if nothing is found.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let env_anchor = find_env_root(&cwd).unwrap_or(cwd);
        let api_key = summarizer::load_env_var(&env_anchor, "ANTHROPIC_API_KEY")?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "ANTHROPIC_API_KEY not found in env or {}/.env (pass --stub to skip)",
                    env_anchor.display()
                )
            })?;
        let mut s = AnthropicSummarizer::new(api_key, cache)?;
        if let Some(m) = model {
            s = s.with_model(m);
        }
        Box::new(s)
    };

    println!("{} chunks in {}", chunks.len(), file.display());
    println!();
    for (i, chunk) in chunks.iter().enumerate() {
        if !summarizer::should_summarize(chunk) {
            println!(
                "{:>3}. {} | {} → (skipped: {})",
                i + 1,
                chunk.kind.as_str(),
                chunk.symbol_name,
                chunk.kind.as_str()
            );
            continue;
        }
        let summary = rt
            .block_on(summarizer.summarize(chunk))
            .with_context(|| format!("summarizing chunk {} ({})", i + 1, chunk.symbol_name))?;
        println!(
            "{:>3}. {} | {} → {}",
            i + 1,
            chunk.kind.as_str(),
            chunk.symbol_name,
            summary
        );
    }
    Ok(())
}

/// Walk upward from `start` looking for the nearest directory that
/// contains a `.env` file. Returns the directory path, not the `.env`
/// path itself, so callers can join other resources to it. Returns
/// `None` if hitting the filesystem root without finding one.
fn find_env_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".env").is_file() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

fn cmd_verify(artifact: &Path, pubkey_path: Option<&Path>, unsigned: bool) -> Result<()> {
    let verified: VerifiedArtifact = verify(artifact)?;
    println!("layout ok ({} payload files)", verified.verified_files);
    println!("build_id          {}", verified.manifest.build_id);
    println!("hytale_impl_ver   {}", verified.manifest.hytale_impl_version);
    println!(
        "patchline         {}",
        verified
            .manifest
            .hytale_patchline
            .as_deref()
            .unwrap_or("<none>")
    );
    println!("schema_version    {}", verified.manifest.schema_version);

    if unsigned {
        if !verified.signature_bytes.is_empty() {
            println!("WARN: --unsigned specified but artifact HAS a signature; skipping verify");
        }
        return Ok(());
    }

    if verified.signature_bytes.is_empty() {
        bail!("artifact is unsigned and --unsigned was not passed");
    }

    let pubkey = match pubkey_path {
        Some(p) => {
            let text = fs::read_to_string(p)
                .with_context(|| format!("reading pubkey from {}", p.display()))?;
            parse_pubkey_hex(&text)?
        }
        None => embedded_pubkey()?,
    };

    verify_manifest(
        &verified.manifest_bytes,
        &verified.signature_bytes,
        &pubkey,
    )?;

    let actual_fp = fingerprint(&pubkey)?;
    if verified.manifest.signing_pubkey_fingerprint != actual_fp {
        bail!(
            "manifest fingerprint {} doesn't match verifying pubkey {}",
            verified.manifest.signing_pubkey_fingerprint,
            actual_fp
        );
    }

    println!("signature ok      (fingerprint {actual_fp})");
    Ok(())
}

// -- helpers ----------------------------------------------------------

fn make_manifest(
    build_id: &str,
    hytale_impl_version: &str,
    hytale_patchline: Option<&str>,
    fingerprint: String,
) -> Manifest {
    Manifest {
        build_id: build_id.to_string(),
        hytale_impl_version: hytale_impl_version.to_string(),
        hytale_patchline: hytale_patchline.map(|s| s.to_string()),
        vineflower_version: VINEFLOWER_VERSION.to_string(),
        chunker_version: CHUNKER_VERSION.to_string(),
        schema_version: SCHEMA_VERSION,
        embedder_id: EMBEDDER_ID.to_string(),
        embedder_dim: atlas_lib::embedder::EMBEDDING_DIM as u32,
        min_client_version: MIN_CLIENT_VERSION.to_string(),
        signing_pubkey_fingerprint: fingerprint,
        created_at: iso8601_now(),
 // Packer overwrites this before writing the tarball.
        sha256sums_sha256: "0".repeat(64),
    }
}

fn iso8601_now() -> String {
    use std::time::SystemTime;
    atlas_lib::indexer::metadata::format_iso8601(SystemTime::now())
}

/// Walk `staging` and emit one [`FileEntry`] per file. Directories are
/// traversed recursively; symlinks are skipped to keep artifacts
/// hermetic. Paths are normalised to forward-slash form relative to the
/// staging root.
fn walk_staging(staging: &Path) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let root = staging.canonicalize().with_context(|| {
        format!("canonicalizing staging root {}", staging.display())
    })?;
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.with_context(|| "walking staging")?;
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
 // Symlinks, sockets, etc - skip.
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(&root)
            .with_context(|| format!("relativizing {}", abs.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        out.push(FileEntry {
            rel_path: rel,
            abs_path: abs,
        });
    }
    Ok(out)
}
