//! Atlas keyword indexer.
//!
//! Walks a slot's decompile output, indexes each `.java` file into a
//! Tantivy index at `<data_dir>/indexes/tantivy/{slot}/`, and serves
//! keyword queries back to the UI.
//!
//! Mirrors the patcher module's shape:
//!   - Public `spawn_index` kicks work off on the shared Tokio runtime.
//!   - `SharedIndexerStatus` tracks the one run-in-flight.
//!   - Events (`index:phase`, `index:progress`, `index:done`, `index:error`)
//!     are slot-tagged so the UI can multiplex.
//!   - Per-slot `atlas-meta.json` lets the UI know if search is usable for
//!     a given branch without opening the index. (Named distinctly from
//!     Tantivy's own `meta.json` so they don't collide in the same dir.)

pub mod analyzer;
pub mod chunker;
pub mod hm_docs;
pub mod hypixel_docs;
pub mod metadata;
pub mod sanitize;
pub mod schema;
pub mod status;
pub mod summarizer;
pub mod symbols;
pub mod walker;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::Term;
use tantivy::tokenizer::TokenizerManager;
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument};

use crate::config::Slot;
use crate::embedder::Embedder;
use crate::lance::{self, ChunkRow, LanceStore};
use analyzer::{CodeTokenizer, CODE_TOKENIZER};
use metadata::{format_iso8601, IndexMetadata};
use schema::{build as build_schema, IndexFields, SourceType};
use status::IndexerPhase;
use symbols::SymbolsDb;

const WRITER_HEAP_BYTES: usize = 128 * 1024 * 1024;

/// Directory holding the Tantivy index for one slot.
pub fn index_dir_for(data_dir: &Path, slot: Slot) -> PathBuf {
    data_dir
        .join("indexes")
        .join("tantivy")
        .join(slot.as_str())
}

/// Progress event surfaced by [`run`]. Both the desktop app (via a Tauri
/// sink) and the `atlas-build` CLI (via a stdout sink) consume the same
/// stream, so the indexer doesn't need to know which is listening.
#[derive(Debug, Clone)]
pub enum IndexEvent {
    Phase(IndexerPhase),
    Progress {
        current: usize,
        total: usize,
        chunks: u64,
    },
    Done {
        docs: u64,
    },
}

/// Sink for [`IndexEvent`]s. Implementors translate events into whatever
/// transport their caller cares about (Tauri events, log lines, no-op).
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: IndexEvent);
}

/// Sink that drops every event. Useful for tests + callers that don't
/// care about progress.
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _: IndexEvent) {}
}

/// Headless indexer entry point. Walks `decompile_dir`, writes Tantivy
/// to `index_dir`, Lance to `lance_dir`, and a symbols sidecar inside
/// `index_dir`. Progress events go to `sink`. Errors are returned; the
/// caller decides how to surface them.
pub async fn run(
    embedder: Arc<dyn Embedder>,
    slot: Slot,
    decompile_dir: PathBuf,
    index_dir: PathBuf,
    lance_dir: PathBuf,
    sink: Arc<dyn ProgressSink>,
    summarizer_opt: Option<Arc<dyn summarizer::Summarizer>>,
    // Optional path to a clone of the HM docs repo. When `Some`, every
    // `.md` file under it is added to the index alongside the Java
    // source, tagged with `source_type = "hm_doc"`. The desktop app
    // always passes `None` - only `atlas-build` populates this.
    hm_docs_dir: Option<PathBuf>,
    // Optional path to a directory of mirrored Hypixel Javadoc HTML
    // (the `release.server.docs.hytale.com` / `prerelease.…` trees).
    // When `Some`, every recognised class page is added to the index
    // tagged `source_type = "hypixel_doc"`. CI mirrors via `wget`; the
    // desktop app always passes `None`.
    hypixel_docs_dir: Option<PathBuf>,
) -> Result<()> {
    if !decompile_dir.is_dir() {
        return Err(anyhow!(
            "decompile output missing at {}",
            decompile_dir.display()
        ));
    }

    // Clear stale meta up front so a half-built index doesn't read as ready.
    let _ = IndexMetadata::delete(&index_dir);

    // --- Phase: walking -------------------------------------------------
    sink.emit(IndexEvent::Phase(IndexerPhase::Walking));

    let walk_root = decompile_dir.clone();
    let total_files = tokio::task::spawn_blocking(move || walker::count_files(&walk_root))
        .await
        .context("walker panicked")?;
    if total_files == 0 {
        return Err(anyhow!("no .java files under {}", decompile_dir.display()));
    }

    // --- Phase: indexing ------------------------------------------------
    sink.emit(IndexEvent::Phase(IndexerPhase::Indexing));

    // Reset the Lance store up-front on the async runtime (the LanceDB
    // API is async). The handle is moved into `spawn_blocking` below so
    // the indexer thread can `block_on` the async `add_batch` / `count`
    // calls without re-entering the runtime setup.
    let lance_store = LanceStore::reset(&lance_dir)
        .await
        .context("resetting Lance store")?;

    let sink_for_index = sink.clone();
    let decompile_for_index = decompile_dir.clone();
    let index_dir_for_task = index_dir.clone();
    let slot_for_task = slot;
    let embedder_for_task = embedder.clone();
    let summarizer_for_task = summarizer_opt.clone();
    let hm_docs_for_task = hm_docs_dir.clone();
    let hypixel_docs_for_task = hypixel_docs_dir.clone();
    let rt_handle = tokio::runtime::Handle::current();
    let docs = tokio::task::spawn_blocking(move || {
        build_index(
            sink_for_index.as_ref(),
            slot_for_task,
            &decompile_for_index,
            &index_dir_for_task,
            total_files,
            embedder_for_task,
            &lance_store,
            &rt_handle,
            summarizer_for_task,
            hm_docs_for_task.as_deref(),
            hypixel_docs_for_task.as_deref(),
        )
    })
    .await
    .context("indexer task panicked")??;

    // --- Persist metadata ----------------------------------------------
    // Pull Hytale impl version + Vineflower version from the decompile's
    // SlotMetadata (`workspace/metadata.json`, one dir above the decompile
    // output). Missing file is fine for older workspaces - fields fall
    // back to their sane defaults and the compound version key just
    // carries empty strings until a re-decompile repopulates it.
    let slot_meta = decompile_dir
        .parent()
        .and_then(|workspace| crate::patcher::metadata::SlotMetadata::read(workspace));
    let (hytale_impl_version, hytale_patchline, vineflower_version) = match slot_meta {
        Some(sm) => (
            sm.hytale_version.unwrap_or_default(),
            None,
            sm.vineflower_version,
        ),
        None => (
            String::new(),
            None,
            crate::patcher::vineflower::VINEFLOWER_VERSION.to_string(),
        ),
    };

    let now_iso = format_iso8601(SystemTime::now());
    let meta = IndexMetadata {
        indexed_at: now_iso.clone(),
        docs,
        decompile_mtime: decompile_mtime_iso(&decompile_dir),
        hytale_impl_version,
        hytale_patchline,
        vineflower_version,
        chunker_version: metadata::CHUNKER_VERSION.to_string(),
        embedder_id: metadata::EMBEDDER_ID.to_string(),
        embedder_dim: crate::embedder::EMBEDDING_DIM as u32,
        schema_version: metadata::SCHEMA_VERSION,
        min_client_version: metadata::MIN_CLIENT_VERSION.to_string(),
        created_at: now_iso,
        // Empty for locally-built indexes; `atlas-build` populates this
        // from the signing keypair.
        signing_pubkey_fingerprint: String::new(),
    };
    if let Err(err) = meta.write(&index_dir) {
        tracing::warn!(?err, "failed to write index metadata");
    }

    sink.emit(IndexEvent::Done { docs });
    Ok(())
}

/// Surgically refresh a single section inside an already-built index.
///
/// Opens the existing Tantivy + Lance stores, deletes every row whose
/// `source_type` matches `source_type`, walks the section's source path,
/// and appends the freshly chunked + embedded rows. The other source
/// types are left untouched, so this turns "tweak the HM docs walker
/// and reindex" from a 30-minute full rebuild into a few-minute pass.
///
/// Compatibility check: refuses to run if the on-disk
/// `atlas-meta.json`'s `embedder_id` or `chunker_version` differs from
/// the current binary's. Hybrid search assumes every row was produced
/// by the same chunker + embedder; mixing two would silently degrade
/// ranking with no warning at query time.
///
/// Today only [`SourceType::HmDoc`] is wired. The other variants bail
/// - Java source needs the `decompile` walker, Hypixel Javadocs need
/// the cache pass + aux-text injection - both want a separate code
/// path or a refactor of `build_index` we're not paying for yet.
pub async fn add_section(
    embedder: Arc<dyn Embedder>,
    slot: Slot,
    index_dir: PathBuf,
    lance_dir: PathBuf,
    sink: Arc<dyn ProgressSink>,
    source_type: SourceType,
    hm_docs_dir: Option<PathBuf>,
) -> Result<u64> {
    if !index_dir.is_dir() {
        return Err(anyhow!(
            "no tantivy index at {} - run `atlas-build index` first",
            index_dir.display()
        ));
    }

    // Compatibility gate. Older meta files won't have these fields
    // populated (serde defaults to ""), in which case we can't prove
    // safety either way - refuse to proceed and let the user run a
    // full rebuild.
    let meta = IndexMetadata::read(&index_dir).ok_or_else(|| {
        anyhow!(
            "no atlas-meta.json at {} - staging dir is not from a prior `atlas-build index` run",
            index_dir.display()
        )
    })?;
    if meta.embedder_id != metadata::EMBEDDER_ID {
        return Err(anyhow!(
            "embedder mismatch: meta says `{}`, current binary uses `{}` - \
             a full rebuild is required so all rows share the same vector space",
            meta.embedder_id,
            metadata::EMBEDDER_ID
        ));
    }
    if meta.chunker_version != metadata::CHUNKER_VERSION {
        return Err(anyhow!(
            "chunker mismatch: meta says `{}`, current binary uses `{}` - \
             a full rebuild is required so chunk boundaries stay consistent",
            meta.chunker_version,
            metadata::CHUNKER_VERSION
        ));
    }

    sink.emit(IndexEvent::Phase(IndexerPhase::Indexing));

    // Open Lance up-front so any failure here aborts before we touch
    // Tantivy. The legitimate-failure modes are "no lance dir" (caller
    // ran `index --lance-skip` or similar - unsupported here) and IO
    // errors; both are clearer reported before we open the writer.
    let lance_store = LanceStore::open_existing(&lance_dir)
        .await
        .with_context(|| format!("opening Lance store at {}", lance_dir.display()))?
        .ok_or_else(|| {
            anyhow!(
                "no Lance store at {} - add-section needs vector rows alongside Tantivy",
                lance_dir.display()
            )
        })?;

    // Wipe the prior rows for this source type. SQL-shape predicate;
    // SourceType::as_str() returns short ASCII identifiers
    // (`hm_doc`, etc.) so no quote-escaping is needed.
    let predicate = format!("source_type = '{}'", source_type.as_str());
    lance_store
        .delete_where(&predicate)
        .await
        .with_context(|| format!("clearing existing `{}` rows from Lance", source_type.as_str()))?;

    let sink_for_index = sink.clone();
    let index_dir_for_task = index_dir.clone();
    let embedder_for_task = embedder.clone();
    let rt_handle = tokio::runtime::Handle::current();
    let docs = tokio::task::spawn_blocking(move || {
        add_section_blocking(
            sink_for_index.as_ref(),
            slot,
            &index_dir_for_task,
            embedder_for_task,
            &lance_store,
            &rt_handle,
            source_type,
            hm_docs_dir.as_deref(),
        )
    })
    .await
    .context("add-section task panicked")??;

    // Refresh `indexed_at` so the staging dir's freshness reflects this
    // pass; everything else (compound version key, fingerprint) stays
    // intact because we deliberately did not change the embedder /
    // chunker / decompile.
    if let Some(mut meta) = IndexMetadata::read(&index_dir) {
        meta.indexed_at = format_iso8601(SystemTime::now());
        if let Err(err) = meta.write(&index_dir) {
            tracing::warn!(?err, "failed to refresh atlas-meta.json after add-section");
        }
    }

    sink.emit(IndexEvent::Done { docs });
    Ok(docs)
}

/// Blocking body for [`add_section`]. Opens the existing Tantivy index,
/// issues a bulk delete on `source_type`, walks + embeds the new rows,
/// commits.
fn add_section_blocking(
    sink: &dyn ProgressSink,
    slot: Slot,
    index_dir: &Path,
    embedder: Arc<dyn Embedder>,
    lance_store: &LanceStore,
    rt: &tokio::runtime::Handle,
    source_type: SourceType,
    hm_docs_dir: Option<&Path>,
) -> Result<u64> {
    let (_, fields) = build_schema();
    let index = Index::open_in_dir(index_dir)
        .with_context(|| format!("opening Tantivy index at {}", index_dir.display()))?;
    register_tokenizers(&index);

    let mut writer = index
        .writer_with_num_threads(2, WRITER_HEAP_BYTES)
        .context("allocating Tantivy writer")?;

    // Delete-then-add is safe in Tantivy: delete_term applies to docs
    // whose opstamp is < the delete's opstamp, and every add_document
    // below gets a higher opstamp. Newly added rows survive the delete.
    writer.delete_term(Term::from_field_text(
        fields.source_type,
        source_type.as_str(),
    ));

    let mut pending: Vec<PendingChunk> = Vec::with_capacity(EMBED_BATCH_CHUNKS);
    let mut chunks_written = 0u64;
    let mut docs_added = 0u64;

    match source_type {
        SourceType::HmDoc => {
            let docs_dir = hm_docs_dir.ok_or_else(|| {
                anyhow!("--hm-docs or --hm-docs-fetch is required when --source-type hm_doc")
            })?;
            let docs = hm_docs::walk_docs(docs_dir)
                .with_context(|| format!("walking HM docs at {}", docs_dir.display()))?;
            let total = docs.len();
            tracing::info!(count = total, "HM docs discovered (add-section)");
            let mut current = 0usize;
            for doc in docs {
                pending.push(PendingChunk {
                    rel_path: doc.rel_path,
                    source_type: SourceType::HmDoc.as_str(),
                    package: String::new(),
                    fqn: String::new(),
                    filename: doc.title.clone(),
                    symbol: doc.title,
                    chunk_kind: "doc".to_string(),
                    start_line: 1,
                    end_line: doc.line_count.max(1),
                    line_count: doc.line_count,
                    text: doc.body,
                });
                docs_added += 1;
                current += 1;

                flush_if_full(
                    &mut pending,
                    false,
                    slot,
                    &fields,
                    &mut writer,
                    embedder.as_ref(),
                    lance_store,
                    rt,
                    &mut chunks_written,
                    sink,
                    current,
                    total,
                )?;
            }
            flush_if_full(
                &mut pending,
                true,
                slot,
                &fields,
                &mut writer,
                embedder.as_ref(),
                lance_store,
                rt,
                &mut chunks_written,
                sink,
                current,
                total,
            )?;
        }
        other => {
            return Err(anyhow!(
                "add_section: only `hm_doc` is supported today (got `{}`)",
                other.as_str()
            ));
        }
    }

    sink.emit(IndexEvent::Phase(IndexerPhase::Committing));
    writer.commit().context("committing Tantivy")?;

    tracing::info!(
        docs = docs_added,
        chunks = chunks_written,
        "add-section complete"
    );
    Ok(docs_added)
}

/// Number of chunks per embed + Lance-write batch. Bigger batches mean
/// fewer Tantivy flushes (and so fewer micro-segments to merge) and
/// fewer Lance commits (each one writes a manifest, which adds up on
/// Windows). 256 chunks at ~1 KB average = ~256 KB per batch, well
/// within fastembed's safe range.
const EMBED_BATCH_CHUNKS: usize = 256;

/// Concurrency cap for the LLM summarization step (atlas-build path
/// only). Sequential `block_on` over ~50K chunks took ~16 hours on
/// Haiku; with `buffer_unordered(SUMMARIZE_CONCURRENCY)` per file the
/// same section drops to ~30-60 min. 16 is conservative - Anthropic's
/// Tier-1 limit on Haiku 4.5 is well above this; we can raise later if
/// the section grows. Per-file batching means concurrency varies with
/// file size, but average chunks/file × this cap is ~160 in flight,
/// which is plenty.
const SUMMARIZE_CONCURRENCY: usize = 16;

/// Holds everything the indexer needs to write one chunk to both
/// stores. Text is owned because the surrounding file `content` string
/// goes out of scope once we move to the next file - embedding happens
/// on the batch boundary, which may span files.
struct PendingChunk {
    rel_path: String,
    /// See [`schema::SourceType`].
    source_type: &'static str,
    package: String,
    fqn: String,
    filename: String,
    symbol: String,
    chunk_kind: String,
    start_line: u64,
    end_line: u64,
    line_count: u64,
    text: String,
}

/// Blocking indexer body. Opens (or recreates) the Tantivy index at
/// `index_dir`, walks the decompile tree, and commits one document per
/// chunk. Dual-writes each chunk to LanceDB with a BGE-small embedding
/// so semantic search has a vector store to query.
#[allow(clippy::too_many_arguments)]
fn build_index(
    sink: &dyn ProgressSink,
    slot: Slot,
    decompile_dir: &Path,
    index_dir: &Path,
    total_files: usize,
    embedder: Arc<dyn Embedder>,
    lance_store: &LanceStore,
    rt: &tokio::runtime::Handle,
    summarizer_opt: Option<Arc<dyn summarizer::Summarizer>>,
    hm_docs_dir: Option<&Path>,
    hypixel_docs_dir: Option<&Path>,
) -> Result<u64> {
    // Fresh directory each build. Tantivy supports incremental writes, but
    // the contract is "decompile changed -> rebuild from scratch."
    if index_dir.exists() {
        std::fs::remove_dir_all(index_dir)
            .with_context(|| format!("wiping old index at {}", index_dir.display()))?;
    }
    std::fs::create_dir_all(index_dir)
        .with_context(|| format!("creating index dir {}", index_dir.display()))?;

    let (schema_obj, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema_obj.clone())
        .with_context(|| format!("create Tantivy index at {}", index_dir.display()))?;
    register_tokenizers(&index);

    // Cap indexing threads at 2. Tantivy's default is min(num_cpus, 8),
    // which on 16-core boxes spawns 8 threads that each emit their own
    // small segment per flush - leading to a flurry of micro-segments
    // and Windows-side file thrash. Two fatter segments per flush is
    // far easier on the merger and (empirically) fixes the "An index
    // writer was killed" error we saw with the default thread count.
    let mut writer = index
        .writer_with_num_threads(2, WRITER_HEAP_BYTES)
        .context("allocating Tantivy writer")?;

    // Symbol sidecar. Lives inside index_dir so the existing
    // wipe-on-rebuild contract covers it without a second code path.
    let symbols_path = index_dir.join("symbols.sqlite");
    let mut symbols_db = SymbolsDb::create(&symbols_path)
        .with_context(|| format!("creating symbols db at {}", symbols_path.display()))?;
    let symbols_tx = symbols_db
        .begin_write()
        .context("starting symbols write transaction")?;

    let mut current = 0usize;
    let mut files_indexed = 0u64;
    let mut chunks_written = 0u64;
    let mut pending: Vec<PendingChunk> = Vec::with_capacity(EMBED_BATCH_CHUNKS);

    // Walk the Hypixel Javadoc cache up front when present. Two reasons
    // to do this once at the top instead of inside the file loop:
    //   1. We need `fqn → class description` available *during* the
    //      source-chunk loop for aux-text injection.
    //   2. We reuse the same entries to emit standalone `hypixel_doc`
    //      chunks after the source pass - walking the cache once.
    let javadoc_entries = match hypixel_docs_dir {
        Some(dir) => hypixel_docs::walk_cache(dir).with_context(|| {
            format!("walking Hypixel Javadocs at {}", dir.display())
        })?,
        None => Vec::new(),
    };
    let aux_text_index = hypixel_docs::build_aux_text_index(&javadoc_entries);
    if !aux_text_index.is_empty() {
        tracing::info!(
            entries = aux_text_index.len(),
            "Hypixel Javadoc aux-text index built"
        );
    }

    for file in walker::walk(decompile_dir) {
        current += 1;
        // Skip files we can't read; log and move on.
        let content = match std::fs::read_to_string(&file.abs_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(?err, path = %file.abs_path.display(), "skipping unreadable file");
                continue;
            }
        };
        let line_count = content.lines().count() as u64;

        // Chunk the file. chunker::chunk_and_extract returns at least one
        // chunk (falling back to a File chunk if tree-sitter can't parse)
        // plus structural symbols for the SQLite sidecar. The walker's
        // path-derived package is passed as a fallback only - the chunker
        // re-derives the authoritative package from the file's own
        // `package` declaration when present.
        let chunker_out = chunker::chunk_and_extract(&content, &file.package);

        // Override the path-derived package on `file` with the parsed
        // declaration when the chunker found one. This keeps every
        // downstream FQN/`package` field (symbols.sqlite, Tantivy, Lance,
        // aux-text injection) aligned with the source's truth - load-bearing
        // for cross-section pairing because the Hypixel Javadoc side stores
        // FQNs derived from the published Javadoc tree, which carries the
        // full `com.hypixel.*` prefix that Vineflower strips from disk.
        let mut file = file;
        if let Some(pkg) = chunker_out.parsed_package.as_ref() {
            if !pkg.is_empty() && pkg != &file.package {
                file.package = pkg.clone();
                file.fqn = if file.filename.is_empty() {
                    pkg.clone()
                } else {
                    format!("{}.{}", pkg, file.filename)
                };
            }
        }

        // Write this file's symbols to the sidecar. `insert_file` is
        // per-file (cheap) and the whole index build runs inside a single
        // transaction committed at the end, so this is effectively a
        // batched append.
        if let Err(err) =
            symbols_tx.insert_file(&file.rel_path, &chunker_out.symbols)
        {
            // Don't fail the whole index build on a symbol-insert hiccup;
            // search still works without the sidecar. Log loudly so it's
            // obvious if this becomes common.
            tracing::warn!(
                ?err,
                path = %file.rel_path,
                "symbols insert failed; sidecar may be incomplete"
            );
        }

        let mut chunks = chunker_out.chunks;

        // Optional LLM summary injection (atlas-build path). Done in
        // parallel across this file's chunks via `buffer_unordered` -
        // sequential `block_on` per chunk was the section-build
        // bottleneck (~16 hr for ~50K chunks). The central builder
        // pays once per chunk and ships enriched text to every user;
        // desktop indexing passes None and skips this entirely. A
        // summarizer failure on a single chunk is non-fatal - log and
        // ship the raw text.
        if let Some(s) = summarizer_opt.as_ref() {
            // Pair indices with chunk clones so we can write summaries
            // back to `chunks` in any completion order while preserving
            // the original chunk order downstream.
            let to_summarize: Vec<(usize, chunker::Chunk)> = chunks
                .iter()
                .enumerate()
                .filter(|(_, c)| summarizer::should_summarize(c))
                .map(|(i, c)| (i, c.clone()))
                .collect();

            if !to_summarize.is_empty() {
                let s = s.clone();
                let results: Vec<(usize, Result<String>)> =
                    rt.block_on(async move {
                        use futures_util::stream::{self, StreamExt};
                        stream::iter(to_summarize.into_iter().map(|(i, c)| {
                            let s = s.clone();
                            async move { (i, s.summarize(&c).await) }
                        }))
                        .buffer_unordered(SUMMARIZE_CONCURRENCY)
                        .collect::<Vec<_>>()
                        .await
                    });

                for (i, result) in results {
                    match result {
                        Ok(summary) => {
                            summarizer::inject_summary(&mut chunks[i], &summary);
                        }
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                symbol = %chunks[i].symbol_name,
                                fqn = %chunks[i].class_fqn,
                                "summarize failed; indexing raw chunk text"
                            );
                        }
                    }
                }
            }
        }

        // Hypixel Javadoc aux-text injection. For every source chunk
        // whose owning class has an entry in the Javadoc cache, prepend
        // the class-level prose. Method-level chunks all share the same
        // class FQN so they all see the same prose - duplication across
        // chunks is fine because each chunk is its own embedding +
        // BM25 doc. We only inject the type-level description (not the
        // full method dump) to keep chunk size bounded.
        if !aux_text_index.is_empty() {
            for chunk in chunks.iter_mut() {
                let fqn = if chunk.class_fqn.is_empty() {
                    file.fqn.as_str()
                } else {
                    chunk.class_fqn.as_str()
                };
                if let Some(javadoc) = aux_text_index.get(fqn) {
                    hypixel_docs::inject_aux_text(chunk, javadoc);
                }
            }
        }

        for chunk in chunks {
            // For method/type chunks inside nested classes, chunk.class_fqn
            // (e.g. `com.foo.Outer.Inner`) is more precise than the file's
            // own FQN. Fall back to the file FQN for File-level chunks.
            let doc_fqn = if chunk.class_fqn.is_empty() {
                file.fqn.clone()
            } else {
                chunk.class_fqn.clone()
            };
            pending.push(PendingChunk {
                rel_path: file.rel_path.clone(),
                source_type: SourceType::Source.as_str(),
                package: file.package.clone(),
                fqn: doc_fqn,
                filename: file.filename.clone(),
                symbol: chunk.symbol_name,
                chunk_kind: chunk.kind.as_str().to_string(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                line_count,
                text: chunk.text,
            });

            // Embedding is the real time-sink, not file walking - the
            // helper ticks progress after every batch so the UI doesn't
            // look hung while BGE-small is running.
            flush_if_full(
                &mut pending,
                false,
                slot,
                &fields,
                &mut writer,
                embedder.as_ref(),
                lance_store,
                rt,
                &mut chunks_written,
                sink,
                current,
                total_files,
            )?;
        }
        files_indexed += 1;

        // File-cadence fallback: for runs where chunks-per-file is
        // small and flushes are rare, still tick per ~1% of files.
        let step = std::cmp::max(1, total_files / 100);
        if current == total_files || current % step == 0 || current % 50 == 0 {
            sink.emit(IndexEvent::Progress {
                current,
                total: total_files,
                chunks: chunks_written,
            });
        }
    }

    // --- HM docs pass ---------------------------------------
    // When the central builder passes a clone of the HM docs repo, walk
    // every `.md` file under it and add one chunk per file tagged with
    // `source_type = "hm_doc"`. The desktop indexer always passes None
    // here, so per-user runs are unaffected.
    if let Some(docs_dir) = hm_docs_dir {
        let docs = hm_docs::walk_docs(docs_dir)
            .with_context(|| format!("walking HM docs at {}", docs_dir.display()))?;
        tracing::info!(count = docs.len(), "HM docs discovered");
        for doc in docs {
            pending.push(PendingChunk {
                rel_path: doc.rel_path,
                source_type: SourceType::HmDoc.as_str(),
                package: String::new(),
                fqn: String::new(),
                filename: doc.title.clone(),
                symbol: doc.title,
                chunk_kind: "doc".to_string(),
                start_line: 1,
                end_line: doc.line_count.max(1),
                line_count: doc.line_count,
                text: doc.body,
            });

            flush_if_full(
                &mut pending,
                false,
                slot,
                &fields,
                &mut writer,
                embedder.as_ref(),
                lance_store,
                rt,
                &mut chunks_written,
                sink,
                current,
                total_files,
            )?;
        }
    }

    // --- Hypixel Javadoc pass -------------------------------
    // Same shape as the HM docs pass above. Each cached class HTML page
    // becomes one chunk tagged `source_type = "hypixel_doc"`; the
    // chunker concatenates the type description + every method
    // description into a single body so a single search hit covers the
    // whole class's documented surface.
    //
    // Reuses `javadoc_entries` walked at the top of `build_index` -
    // walking the cache twice would double the disk-read cost on
    // central-builder runs where the cache holds 1k+ pages.
    if !javadoc_entries.is_empty() {
        tracing::info!(
            count = javadoc_entries.len(),
            "Hypixel Javadoc pages discovered"
        );
        for doc in javadoc_entries {
            // FQN minus simple_name = package; mirror the source-chunk
            // shape so search results render with familiar metadata.
            let package = doc
                .fqn
                .rsplit_once('.')
                .map(|(pkg, _)| pkg.to_string())
                .unwrap_or_default();
            pending.push(PendingChunk {
                rel_path: doc.rel_path,
                source_type: SourceType::HypixelDoc.as_str(),
                package,
                fqn: doc.fqn,
                filename: doc.simple_name.clone(),
                symbol: doc.simple_name,
                chunk_kind: doc.kind,
                start_line: 1,
                end_line: doc.line_count.max(1),
                line_count: doc.line_count,
                text: doc.body,
            });

            flush_if_full(
                &mut pending,
                false,
                slot,
                &fields,
                &mut writer,
                embedder.as_ref(),
                lance_store,
                rt,
                &mut chunks_written,
                sink,
                current,
                total_files,
            )?;
        }
    }

    // Flush any remainder.
    flush_if_full(
        &mut pending,
        true,
        slot,
        &fields,
        &mut writer,
        embedder.as_ref(),
        lance_store,
        rt,
        &mut chunks_written,
        sink,
        current,
        total_files,
    )?;

    // Commit.
    sink.emit(IndexEvent::Phase(IndexerPhase::Committing));
    writer.commit().context("committing index")?;

    // Commit the symbols transaction. A failure here leaves the DB empty
    // (rusqlite auto-rolls back on drop), which is recoverable - the
    // search index is already committed and usable.
    symbols_tx
        .commit()
        .context("committing symbols transaction")?;

    let symbol_counts = symbols_db.row_counts().unwrap_or(symbols::RowCounts {
        classes: 0,
        methods: 0,
        fields: 0,
    });

    tracing::info!(
        files = files_indexed,
        chunks = chunks_written,
        classes = symbol_counts.classes,
        methods = symbol_counts.methods,
        fields = symbol_counts.fields,
        "index build complete"
    );
    Ok(files_indexed)
}

/// Conditional wrapper around [`flush_batch`] that the build_index loop
/// calls at every chunk-push site. With `force = false` it only flushes
/// once the pending vec hits `EMBED_BATCH_CHUNKS` (the hot-loop case);
/// with `force = true` it drains whatever's left after the walk
/// completes. Either way it bumps `chunks_written` and emits a Progress
/// event so the UI doesn't appear hung between large batches.
#[allow(clippy::too_many_arguments)]
fn flush_if_full(
    pending: &mut Vec<PendingChunk>,
    force: bool,
    slot: Slot,
    fields: &IndexFields,
    writer: &mut tantivy::IndexWriter,
    embedder: &dyn Embedder,
    lance_store: &LanceStore,
    rt: &tokio::runtime::Handle,
    chunks_written: &mut u64,
    sink: &dyn ProgressSink,
    current: usize,
    total: usize,
) -> Result<usize> {
    let should_flush = if force {
        !pending.is_empty()
    } else {
        pending.len() >= EMBED_BATCH_CHUNKS
    };
    if !should_flush {
        return Ok(0);
    }
    let n = pending.len();
    flush_batch(pending, slot, fields, writer, embedder, lance_store, rt)?;
    *chunks_written += n as u64;
    sink.emit(IndexEvent::Progress {
        current,
        total,
        chunks: *chunks_written,
    });
    Ok(n)
}

/// Embed the pending chunks in one fastembed call, then write them to
/// both stores. Tantivy is append-only via the writer; Lance takes one
/// Arrow RecordBatch per flush.
fn flush_batch(
    pending: &mut Vec<PendingChunk>,
    slot: Slot,
    fields: &IndexFields,
    writer: &mut tantivy::IndexWriter,
    embedder: &dyn Embedder,
    lance_store: &LanceStore,
    rt: &tokio::runtime::Handle,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }

    // Embed all pending texts in one call. BGE-small batches internally;
    // fastembed returns vectors in input order.
    let texts: Vec<&str> = pending.iter().map(|c| c.text.as_str()).collect();
    let vectors = embedder
        .embed_batch(&texts)
        .context("embedding chunk batch")?;
    if vectors.len() != pending.len() {
        return Err(anyhow!(
            "embedder returned {} vectors for {} inputs",
            vectors.len(),
            pending.len()
        ));
    }

    // Write Tantivy docs.
    for chunk in pending.iter() {
        let mut doc = TantivyDocument::new();
        doc.add_text(fields.slot, slot.as_str());
        doc.add_text(fields.source_type, chunk.source_type);
        doc.add_text(fields.path, &chunk.rel_path);
        doc.add_text(fields.package, &chunk.package);
        doc.add_text(fields.fqn, &chunk.fqn);
        doc.add_text(fields.filename, &chunk.filename);
        doc.add_text(fields.symbol, &chunk.symbol);
        doc.add_text(fields.chunk_kind, &chunk.chunk_kind);
        doc.add_u64(fields.start_line, chunk.start_line);
        doc.add_u64(fields.end_line, chunk.end_line);
        doc.add_u64(fields.line_count, chunk.line_count);
        // For Java source we strip comments and string-literal contents
        // before tokenization so the inverted index can't be used to
        // reconstruct prose, error messages, or Javadoc from the original
        // source. Doc-type rows (markdown from public sites) pass through
        // unchanged. See `sanitize` module for full rationale.
        let indexed_text: std::borrow::Cow<'_, str> = if chunk.source_type == "source" {
            std::borrow::Cow::Owned(sanitize::strip_for_indexing(&chunk.text))
        } else {
            std::borrow::Cow::Borrowed(chunk.text.as_str())
        };
        doc.add_text(fields.content, indexed_text.as_ref());
        writer.add_document(doc)?;
    }

    // Build a RecordBatch mirroring the Tantivy stored fields + embedding.
    // Chunk body text is intentionally NOT written to Lance - see the
    // schema doc-comment in `crate::lance` for rationale (the artifact must
    // be free of decompiled implementation so it can be redistributed).
    let rows: Vec<ChunkRow<'_>> = pending
        .iter()
        .zip(vectors.iter())
        .map(|(c, v)| ChunkRow {
            slot: slot.as_str(),
            source_type: c.source_type,
            path: &c.rel_path,
            package: &c.package,
            fqn: &c.fqn,
            filename: &c.filename,
            symbol: &c.symbol,
            chunk_kind: &c.chunk_kind,
            start_line: c.start_line,
            end_line: c.end_line,
            line_count: c.line_count,
            embedding: v.as_slice(),
        })
        .collect();
    let batch = lance::batch_from_rows(&rows).context("building Arrow batch")?;
    // `rows` borrows from `pending` - drop it before clearing.
    drop(rows);

    // LanceDB API is async; block on the runtime from this blocking thread.
    rt.block_on(lance_store.add_batch(batch))
        .context("writing batch to Lance")?;

    pending.clear();
    Ok(())
}

fn decompile_mtime_iso(decompile_dir: &Path) -> String {
    std::fs::metadata(decompile_dir)
        .and_then(|m| m.modified())
        .ok()
        .map(format_iso8601)
        .unwrap_or_default()
}

/// Register our custom tokenizer against the index's tokenizer manager.
/// Must be called on both the writer's index and any reader's index.
pub fn register_tokenizers(index: &Index) {
    let manager: &TokenizerManager = index.tokenizers();
    manager.register(CODE_TOKENIZER, CodeTokenizer);
}

/// Delete a slot's index directory and metadata.
pub fn clear_slot(index_dir: &Path) -> std::io::Result<()> {
    if index_dir.is_dir() {
        std::fs::remove_dir_all(index_dir)?;
    }
    Ok(())
}

// -----------------------------------------------------------------------
// Search
// -----------------------------------------------------------------------

/// One hit surfaced to the UI. A hit corresponds to a single file, but
/// ranking is done at chunk granularity - the highest-scoring chunk's
/// metadata (symbol name, line range) is surfaced here so the UI can
/// show "the method in Foo.java that matched."
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub slot: String,
    /// Which section this hit lives in. One of `source` (decompiled
    /// Java), `hm_doc` (Hytale Modding markdown guides), `hypixel_doc`
    /// (Hypixel Javadoc-derived API docs), or `asset` (assets.zip
    /// metadata). Used by the desktop UI's section chips and the MCP
    /// `source_type` filter for post-search narrowing.
    pub source_type: String,
    pub path: String,
    pub fqn: String,
    pub package: String,
    pub filename: String,
    pub score: f32,
    pub line_count: u64,
    /// 1-based start line of the best-scoring chunk - used by the file
    /// viewer to jump to the match.
    pub preview_line: Option<usize>,
    /// Short excerpt around the match - plain text, not HTML.
    pub preview: Option<String>,
    /// "type" | "method" | "file" - what kind of chunk scored highest.
    pub chunk_kind: String,
    /// Simple symbol name (class or method). Empty when `chunk_kind == "file"`.
    pub symbol_name: String,
    /// Inclusive line range of the best-scoring chunk.
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    /// Debug ranking info populated by the hybrid blender. `None` for
    /// the BM25-only `search()` path. Surfaced to the UI's debug panel
    /// so search-quality regressions are diagnosable without having to
    /// re-instrument the backend each time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<HitDebug>,
    /// Comma-joined author names for `hm_doc` hits, pulled from the
    /// frontmatter at index time. `None` for every other section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<String>,
}

/// Per-hit ranking breakdown. All fields are optional because a hit
/// might come from one ranker and not the other (a vector-only hit
/// has no `bm25_*` fields, etc.).
#[derive(Debug, Clone, Serialize)]
pub struct HitDebug {
    pub bm25_rank: Option<u32>,
    pub bm25_score: Option<f32>,
    pub vector_rank: Option<u32>,
    pub vector_distance: Option<f32>,
    pub rrf_score: f32,
    pub weight_bm25: f32,
    pub weight_vector: f32,
}

/// Identifier for a mounted index. One of:
///   - `release` / `pre-release` - the legacy slot-backed local indexes.
///     Identical to `Slot::as_str()`.
///   - `release-<impl_version_short_sha>` - an artifact fetched from the
///     central builder.
///   - `user-project-<uuid>` - a locally-indexed mod project.
///
/// Newtype over `String` so callers can't accidentally mix an IndexId
/// with a random string (e.g. a path).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IndexId(String);

impl IndexId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<Slot> for IndexId {
    fn from(slot: Slot) -> Self {
        // Slot::as_str() - "release" | "pre-release" - is already a stable
        // id for the legacy two-slot layout. Keeping this mapping means
        // the catalog keys line up with the frontend's slot strings until
        // we start mounting fetched artifacts.
        IndexId(slot.as_str().to_string())
    }
}

impl std::fmt::Display for IndexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Shared search handle held in Tauri state. Opens each mounted index
/// lazily and caches the `IndexReader` so queries stay cheap.
///
/// Earlier this held two fixed `Option<Arc<OpenedIndex>>` slots (release
/// and pre-release). Generalising to `HashMap<IndexId, _>` lets the
/// fetcher mount fetched artifacts by build id and lets a future user
/// mod project mount through the same surface.
#[derive(Default)]
pub struct SearchCatalog {
    inner: parking_lot_like::Mutex<CatalogInner>,
}

#[derive(Default)]
struct CatalogInner {
    mounted: std::collections::HashMap<IndexId, Arc<OpenedIndex>>,
}

struct OpenedIndex {
    index: Index,
    reader: IndexReader,
    fields: IndexFields,
    /// Lazily-built map of Javadoc class FQN → cached HTML path on disk.
    /// Populated on first call to [`SearchCatalog::inline_javadocs_for_class`]
    ///. Empty when no Javadoc cache root was provided.
    javadoc_fqn_to_path: std::sync::OnceLock<std::collections::HashMap<String, std::path::PathBuf>>,
    /// Per-class parsed-method cache. Hot when the user clicks through
    /// multiple hits in the same class. Stored as `(type_description,
    /// methods)` so the second-hit path doesn't re-read or re-parse HTML.
    javadoc_methods_cache: parking_lot_like::Mutex<
        std::collections::HashMap<
            String,
            (String, Arc<Vec<crate::indexer::hypixel_docs::MethodDoc>>),
        >,
    >,
}

impl SearchCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Invalidate any cached reader for a slot - call after a rebuild so
    /// the next search sees the fresh index (Tantivy doesn't auto-reopen
    /// across a blow-away-and-recreate).
    pub fn invalidate(&self, slot: Slot) {
        self.invalidate_id(&IndexId::from(slot));
    }

    /// Like [`Self::invalidate`] but addresses a specific mounted index
    /// by id - used by the fetcher and Index Catalog UX
    /// where a slot alone isn't enough to name the index.
    pub fn invalidate_id(&self, id: &IndexId) {
        let mut inner = self.inner.lock();
        inner.mounted.remove(id);
    }

    /// Enumerate the ids of currently-mounted indexes. Used by the Index
    /// Catalog UX to render the "what's mounted" list.
    #[allow(dead_code)]
    pub fn mounted_ids(&self) -> Vec<IndexId> {
        let inner = self.inner.lock();
        inner.mounted.keys().cloned().collect()
    }

    fn ensure(&self, slot: Slot, index_dir: &Path) -> Result<Arc<OpenedIndex>> {
        self.ensure_id(&IndexId::from(slot), index_dir)
    }

    /// Open (or reuse the cached) reader for the index at `index_dir`,
    /// keyed by `id`. The caller owns the mapping from id → filesystem
    /// path; the catalog only caches the opened handle.
    fn ensure_id(&self, id: &IndexId, index_dir: &Path) -> Result<Arc<OpenedIndex>> {
        {
            let inner = self.inner.lock();
            if let Some(opened) = inner.mounted.get(id).cloned() {
                return Ok(opened);
            }
        }
        if IndexMetadata::read(index_dir).is_none() {
            return Err(anyhow!("no index for {}", id));
        }
        let (_, fields) = build_schema();
        let index = Index::open_in_dir(index_dir)
            .with_context(|| format!("opening index at {}", index_dir.display()))?;
        register_tokenizers(&index);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("building Tantivy reader")?;
        let opened = Arc::new(OpenedIndex {
            index,
            reader,
            fields,
            javadoc_fqn_to_path: std::sync::OnceLock::new(),
            javadoc_methods_cache: parking_lot_like::Mutex::default(),
        });
        let mut inner = self.inner.lock();
        inner.mounted.insert(id.clone(), opened.clone());
        Ok(opened)
    }

    /// Raw chunk-level search. Returns hits in Tantivy's score order,
    /// one row per chunk - the caller is responsible for dedup. Used by
    /// the hybrid-search path so RRF can fuse Tantivy and Lance
    /// rankings before collapsing duplicates.
    pub fn search_chunks(
        &self,
        slot: Slot,
        index_dir: &Path,
        query_text: &str,
        limit: usize,
        source_types: Option<&[String]>,
    ) -> Result<Vec<SearchHit>> {
        let trimmed = query_text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let opened = self.ensure(slot, index_dir)?;
        let searcher = opened.reader.searcher();

        // Search across content + fqn + filename + package + symbol so
        // symbol lookups ("PageManager", "getComponent") rank highly
        // without the user needing to disambiguate.
        let mut parser = QueryParser::for_index(
            &opened.index,
            vec![
                opened.fields.fqn,
                opened.fields.filename,
                opened.fields.symbol,
                opened.fields.package,
                opened.fields.content,
            ],
        );
        parser.set_field_boost(opened.fields.fqn, 3.5);
        parser.set_field_boost(opened.fields.filename, 3.0);
        parser.set_field_boost(opened.fields.symbol, 2.5);
        parser.set_field_boost(opened.fields.package, 1.5);

        let parsed = parser
            .parse_query(trimmed)
            .with_context(|| format!("parsing query: {trimmed}"))?;

        // If a section filter is active, AND it into the query so it
        // narrows BEFORE TopDocs caps the result list. Otherwise a
        // niche section (e.g. HM docs at <2% of total chunks) gets
        // crowded out of the top-N by the dominant section and the
        // chip filter visibly returns zero hits.
        let final_query: Box<dyn Query> = match source_types {
            Some(types) if !types.is_empty() => {
                let mut clauses: Vec<(Occur, Box<dyn Query>)> =
                    vec![(Occur::Must, parsed.box_clone())];
                let st_clauses: Vec<(Occur, Box<dyn Query>)> = types
                    .iter()
                    .map(|t| {
                        let term = Term::from_field_text(opened.fields.source_type, t);
                        let q: Box<dyn Query> =
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                        (Occur::Should, q)
                    })
                    .collect();
                clauses.push((Occur::Must, Box::new(BooleanQuery::new(st_clauses))));
                Box::new(BooleanQuery::new(clauses))
            }
            _ => parsed,
        };

        let top = searcher.search(&final_query, &TopDocs::with_limit(limit))?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(build_hit(slot, &opened.fields, &doc, score));
        }
        Ok(hits)
    }

    /// Find the cross-section pair for a class - given a Javadoc FQN, return
    /// the matching source-code class (and vice versa). Returns `None`
    /// when no sibling exists in the index (e.g. an internal source class
    /// with no public Javadoc, or a Javadoc page whose source isn't in
    /// the decompile tree). Empty FQN or no-pair source types (`hm_doc`,
    /// `asset`) also return `None`.
    ///
    /// The matched chunk is whichever scores best for the FQN phrase -
    /// for source we prefer `chunk_kind = type | file` so the right pane
    /// lands on the class, not a method.
    pub fn find_sibling(
        &self,
        slot: Slot,
        index_dir: &Path,
        fqn: &str,
        own_source_type: &str,
    ) -> Result<Option<SearchHit>> {
        let target = match own_source_type {
            "source" => "hypixel_doc",
            "hypixel_doc" => "source",
            _ => return Ok(None),
        };
        let trimmed = fqn.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let opened = self.ensure(slot, index_dir)?;
        let searcher = opened.reader.searcher();

        let mut parser = QueryParser::for_index(&opened.index, vec![opened.fields.fqn]);
        parser.set_field_boost(opened.fields.fqn, 1.0);
        // Quote so the parser treats it as a phrase across the CODE_TOKENIZER
        // splits (`com`, `foo`, `Bar`) and won't match unrelated classes
        // that happen to share a path component.
        let escaped = trimmed.replace('"', "");
        let parsed = match parser.parse_query(&format!("\"{escaped}\"")) {
            Ok(q) => q,
            Err(err) => {
                tracing::debug!(?err, fqn = %trimmed, "fqn parse failed; no sibling");
                return Ok(None);
            }
        };

        let st_term = Term::from_field_text(opened.fields.source_type, target);
        let st_query: Box<dyn Query> =
            Box::new(TermQuery::new(st_term, IndexRecordOption::Basic));
        let combined: Box<dyn Query> = Box::new(BooleanQuery::new(vec![
            (Occur::Must, parsed),
            (Occur::Must, st_query),
        ]));

        // Over-fetch a small batch so we can prefer type/file chunks on
        // the source side - the file is multiple chunks; the Javadoc is
        // always one.
        let top = searcher.search(&combined, &TopDocs::with_limit(8))?;
        if top.is_empty() {
            return Ok(None);
        }

        let mut hits: Vec<SearchHit> = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(build_hit(slot, &opened.fields, &doc, score));
        }

        // For source-side siblings, prefer the type or file chunk so the
        // viewer shows the whole class. Javadoc has only one chunk per
        // class so this is a no-op there.
        let pick = hits
            .iter()
            .find(|h| h.chunk_kind == "type" || h.chunk_kind == "file")
            .cloned()
            .unwrap_or_else(|| hits[0].clone());
        Ok(Some(pick))
    }

    /// Look up the cached Javadoc HTML for a class FQN and return its
    /// per-method docs alongside the class-level description. Feeds the
    /// inline-Javadoc resolver in the source viewer.
    ///
    /// `cache_dir` is the Javadoc cache root (typically
    /// `<atlas-cache>/javadocs/<host>` - the caller picks per-slot).
    /// Returns `Ok(None)` when no cached page exists for the FQN; that
    /// just means the source class has no Javadoc and the viewer should
    /// render zero inline anchors.
    pub fn class_javadoc(
        &self,
        slot: Slot,
        index_dir: &Path,
        cache_dir: &Path,
        class_fqn: &str,
    ) -> Result<Option<(String, Arc<Vec<crate::indexer::hypixel_docs::MethodDoc>>)>>
    {
        let trimmed = class_fqn.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let opened = self.ensure(slot, index_dir)?;

        // Cache hit on parsed methods? Re-parse only on first request
        // for a given class. Lookup table itself is OnceLock-built.
        // We don't cache `type_description` separately - re-parse pulls
        // it for free on miss.
        let map = opened.javadoc_fqn_to_path.get_or_init(|| {
            crate::indexer::hypixel_docs::walk_cache_paths(cache_dir)
                .map(|pairs| pairs.into_iter().collect())
                .unwrap_or_default()
        });
        let html_path = match map.get(trimmed) {
            Some(p) => p.clone(),
            None => return Ok(None),
        };

        // Cache hit? Clone out the value (cheap - Arc + small String)
        // and drop the lock before returning.
        if let Some(hit) = opened
            .javadoc_methods_cache
            .lock()
            .get(trimmed)
            .cloned()
        {
            return Ok(Some(hit));
        }

        let html = std::fs::read_to_string(&html_path)
            .with_context(|| format!("reading {}", html_path.display()))?;
        // The rel_path arg is only used for filename-stem fallback; the
        // value we feed it is good enough - derived from the same FQN.
        let rel_path = format!("{}.html", trimmed.replace('.', "/"));
        let entry = match crate::indexer::hypixel_docs::parse_class_page(&rel_path, &html) {
            Some(e) => e,
            None => return Ok(None),
        };

        let value = (entry.type_description, Arc::new(entry.methods));
        opened
            .javadoc_methods_cache
            .lock()
            .insert(trimmed.to_string(), value.clone());
        Ok(Some(value))
    }

    /// Keyword-only search kept for back-compat + tests. Over-fetches at
    /// chunk granularity then dedups to one hit per file. Callers that
    /// want the hybrid ranking should go through
    /// [`crate::search::hybrid::run`].
    pub fn search(
        &self,
        slot: Slot,
        index_dir: &Path,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        // Over-fetch chunks so that after dedup-by-path we still have
        // enough rows to fill `limit`.
        let fetch_limit = limit.saturating_mul(5).max(limit);
        let raw = self.search_chunks(slot, index_dir, query_text, fetch_limit, None)?;

        let mut best_by_path: std::collections::HashMap<String, SearchHit> =
            std::collections::HashMap::new();
        for hit in raw {
            match best_by_path.get(&hit.path) {
                Some(prev) if prev.score >= hit.score => {}
                _ => {
                    best_by_path.insert(hit.path.clone(), hit);
                }
            }
        }
        let mut out: Vec<SearchHit> = best_by_path.into_values().collect();
        out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        Ok(out)
    }
}

fn build_hit(
    slot: Slot,
    fields: &IndexFields,
    doc: &TantivyDocument,
    score: f32,
) -> SearchHit {
    let slot_str = slot.as_str().to_string();
    let path = doc_text(doc, fields.path).unwrap_or_default();
    let fqn = doc_text(doc, fields.fqn).unwrap_or_default();
    let package = doc_text(doc, fields.package).unwrap_or_default();
    let filename = doc_text(doc, fields.filename).unwrap_or_default();
    let symbol_name = doc_text(doc, fields.symbol).unwrap_or_default();
    let chunk_kind = doc_text(doc, fields.chunk_kind).unwrap_or_default();
    // Defaults to "source" for indexes built before source_type was added
    // (older artifacts have no field; Tantivy returns None there).
    let source_type =
        doc_text(doc, fields.source_type).unwrap_or_else(|| "source".to_string());
    let start_line = doc_u64(doc, fields.start_line);
    let end_line = doc_u64(doc, fields.end_line);
    let line_count = doc_u64(doc, fields.line_count).unwrap_or(0);

    // preview_line = start of the best-scoring chunk so the right panel
    // scrolls to the match. No disk read needed - the chunker already
    // recorded the line range during indexing.
    let preview_line = start_line.map(|n| n as usize);

    SearchHit {
        slot: slot_str,
        source_type,
        path,
        fqn,
        package,
        filename,
        score,
        line_count,
        preview_line,
        preview: None,
        chunk_kind,
        symbol_name,
        start_line,
        end_line,
        debug: None,
        authors: None,
    }
}

fn doc_text(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
}

fn doc_u64(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<u64> {
    doc.get_first(field).and_then(|v| v.as_u64())
}

/// Read a source file given a slot and the relative path stored in the
/// index. Returns an error if the path escapes the decompile root.
pub fn read_source(decompile_dir: &Path, rel_path: &str) -> Result<String> {
    let joined = join_safe(decompile_dir, rel_path)?;
    let content = std::fs::read_to_string(&joined)
        .with_context(|| format!("reading {}", joined.display()))?;
    Ok(content)
}

fn join_safe(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(anyhow!("unsafe path: {rel}"));
    }
    let mut out = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(anyhow!("unsafe path: {rel}"));
        }
        out.push(seg);
    }
    if !out.starts_with(root) {
        return Err(anyhow!("path escapes decompile root: {rel}"));
    }
    Ok(out)
}

// -----------------------------------------------------------------------
// Per-slot readiness summary for the UI
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SlotIndexSummary {
    pub slot: &'static str,
    pub ready: bool,
    pub docs: Option<u64>,
    pub indexed_at: Option<String>,
    pub decompile_mtime_at_index: Option<String>,
    pub stale: bool,
}

pub fn summarize_slot(slot: Slot, index_dir: &Path, decompile_dir: &Path) -> SlotIndexSummary {
    let meta = IndexMetadata::read(index_dir);
    let ready = meta.is_some();
    let current_mtime = if decompile_dir.is_dir() {
        std::fs::metadata(decompile_dir)
            .and_then(|m| m.modified())
            .ok()
            .map(format_iso8601)
    } else {
        None
    };
    let stale = match (&meta, &current_mtime) {
        // Fetched artifacts have a non-empty signing fingerprint. Their
        // staleness is "is there a newer build available from central"
        // - answered by the resolver, not by comparing against the local
        // decompile mtime. Until the resolver lands, treat them as fresh.
        (Some(m), _) if !m.signing_pubkey_fingerprint.is_empty() => false,
        (Some(m), Some(current)) => &m.decompile_mtime != current,
        _ => false,
    };
    SlotIndexSummary {
        slot: slot.as_str(),
        ready,
        docs: meta.as_ref().map(|m| m.docs),
        indexed_at: meta.as_ref().map(|m| m.indexed_at.clone()),
        decompile_mtime_at_index: meta.as_ref().map(|m| m.decompile_mtime.clone()),
        stale,
    }
}

// -----------------------------------------------------------------------
// Tiny Mutex shim so we don't have to add `parking_lot`; a std::sync::Mutex
// wrapped in a newtype keeps the call sites ergonomic.
// -----------------------------------------------------------------------
mod parking_lot_like {
    use std::sync::{Mutex as StdMutex, MutexGuard};

    pub struct Mutex<T>(StdMutex<T>);

    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self(StdMutex::new(T::default()))
        }
    }

    impl<T> Mutex<T> {
        pub fn lock(&self) -> MutexGuard<'_, T> {
            self.0.lock().expect("catalog poisoned")
        }
    }
}
