//! Tauri commands exposed to the front-end.
//!
//! Keep these thin: each command should delegate to a plain Rust module
//! (`config`, `patcher`, later `indexer`) so the real logic stays
//! testable without a Tauri runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use directories::ProjectDirs;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::config::{self, AtlasConfig, HytalePathCheck, Slot};
use crate::embedder::SharedEmbedder;
use crate::fetcher::{
    self,
    status::{FetchStatus, SharedFetchStatus},
    FetchRequest,
};
use crate::guides::GuidesIndex;
use crate::indexer::{
    self,
    status::{IndexerPhase, IndexerStatus, SharedIndexerStatus},
    IndexEvent, ProgressSink, SearchCatalog, SearchHit, SlotIndexSummary,
};
use crate::lance;
use crate::patcher::{
    self,
    ide::{self, DetectedIde, IdeId},
    status::SharedStatus,
    SlotOverview,
};
use crate::RuntimeHandle;

/// Tauri-flavoured [`ProgressSink`]: forwards each `IndexEvent` to both
/// [`SharedIndexerStatus`] (so polling commands stay accurate) and the
/// frontend's event bus (so the UI can subscribe live). The headless
/// `atlas-build index` CLI uses a different sink; the indexer itself
/// doesn't know which is listening.
struct TauriSink {
    app: AppHandle,
    status: SharedIndexerStatus,
    slot: Slot,
}

impl ProgressSink for TauriSink {
    fn emit(&self, event: IndexEvent) {
        match event {
            IndexEvent::Phase(phase) => {
                self.status.set(IndexerStatus::Phase {
                    slot: self.slot.as_str(),
                    phase,
                });
                let _ = self.app.emit(
                    "index:phase",
                    serde_json::json!({
                        "slot": self.slot.as_str(),
                        "phase": phase.as_str(),
                    }),
                );
            }
            IndexEvent::Progress {
                current,
                total,
                chunks,
            } => {
                self.status.set(IndexerStatus::Progress {
                    slot: self.slot.as_str(),
                    phase: IndexerPhase::Indexing,
                    current,
                    total,
                    chunks,
                });
                let _ = self.app.emit(
                    "index:progress",
                    serde_json::json!({
                        "slot": self.slot.as_str(),
                        "phase": IndexerPhase::Indexing.as_str(),
                        "current": current,
                        "total": total,
                        "chunks": chunks,
                    }),
                );
            }
            IndexEvent::Done { docs } => {
                self.status.set(IndexerStatus::Done {
                    slot: self.slot.as_str(),
                    docs,
                });
                let _ = self.app.emit(
                    "index:done",
                    serde_json::json!({
                        "slot": self.slot.as_str(),
                        "docs": docs,
                    }),
                );
            }
        }
    }
}

/// Spawn the headless indexer on the shared runtime, wired up to a
/// `TauriSink` so progress reaches the frontend. On success the catalog
/// is invalidated so the next search reopens the fresh index; on failure
/// the error is surfaced via the same status + event surface.
#[allow(clippy::too_many_arguments)]
fn spawn_index(
    rt: &tokio::runtime::Handle,
    app: AppHandle,
    status: SharedIndexerStatus,
    catalog: Arc<SearchCatalog>,
    embedder: Arc<dyn crate::embedder::Embedder>,
    slot: Slot,
    decompile_dir: PathBuf,
    index_dir: PathBuf,
    lance_dir: PathBuf,
) {
    let sink: Arc<dyn ProgressSink> = Arc::new(TauriSink {
        app: app.clone(),
        status: status.clone(),
        slot,
    });
    rt.spawn(async move {
        // Desktop indexing never summarizes - that's a central-build
        // responsibility (atlas-build) so users don't pay LLM cost.
        // HM docs (`hm_doc`) live in their own decoupled `guides` index
        // (see `crate::guides`); the desktop pipeline only handles
        // decompiled source.
        match indexer::run(
            embedder,
            slot,
            decompile_dir,
            index_dir,
            lance_dir,
            sink,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        {
            Ok(()) => {
                // Force the next search to reopen the fresh index.
                catalog.invalidate(slot);
            }
            Err(err) => {
                tracing::error!(?err, "indexer failed");
                status.set(IndexerStatus::Error {
                    slot: slot.as_str(),
                    message: format!("{err:#}"),
                });
                let _ = app.emit(
                    "index:error",
                    serde_json::json!({ "slot": slot.as_str(), "message": format!("{err:#}") }),
                );
            }
        }
    });
}

/// Result returned by `load_config`. Includes both what's persisted and what
/// the auto-detector thinks are reasonable defaults so the front-end can prompt
/// the user without a second round-trip.
#[derive(Debug, Serialize)]
pub struct ConfigSnapshot {
    pub config: AtlasConfig,
    pub default_release_candidate: Option<PathBuf>,
    pub default_prerelease_candidate: Option<PathBuf>,
    pub detected_release_path: Option<PathBuf>,
    pub detected_prerelease_path: Option<PathBuf>,
}

#[tauri::command]
pub fn load_config() -> Result<ConfigSnapshot, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    Ok(ConfigSnapshot {
        config: cfg,
        default_release_candidate: config::default_release_candidate(),
        default_prerelease_candidate: config::default_prerelease_candidate(),
        detected_release_path: config::detect_release_path(),
        detected_prerelease_path: config::detect_prerelease_path(),
    })
}

#[tauri::command]
pub fn save_config(config: AtlasConfig) -> Result<(), String> {
    config::save(&config).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn validate_hytale_path(path: PathBuf) -> HytalePathCheck {
    config::check_hytale_path(&path)
}

/// Full patcher overview: both slots plus detected IDEs. Called on app boot
/// and after every decompile/config change so the card can stay in sync
/// without the FE having to track per-field state.
#[derive(Debug, Serialize)]
pub struct PatcherOverview {
    pub release: SlotOverview,
    pub pre_release: SlotOverview,
    pub ides: Vec<DetectedIde>,
}

#[tauri::command]
pub fn patcher_overview() -> Result<PatcherOverview, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    let data_dir = data_dir()?;

    let release_workspace = patcher::workspace_for(data_dir.as_path(), Slot::Release);
    let prerelease_workspace = patcher::workspace_for(data_dir.as_path(), Slot::PreRelease);

    Ok(PatcherOverview {
        release: patcher::build_slot_overview(
            Slot::Release,
            config::configured_path(&cfg, Slot::Release),
            config::default_candidate(Slot::Release),
            release_workspace,
        ),
        pre_release: patcher::build_slot_overview(
            Slot::PreRelease,
            config::configured_path(&cfg, Slot::PreRelease),
            config::default_candidate(Slot::PreRelease),
            prerelease_workspace,
        ),
        ides: ide::detect_ides(),
    })
}

/// Kick off a decompile for the given slot. Returns the workspace path
/// immediately; progress flows via `decompile:*` events tagged with the slot.
#[tauri::command]
pub fn start_decompile(
    app: AppHandle,
    status: State<'_, SharedStatus>,
    runtime: State<'_, RuntimeHandle>,
    slot: Slot,
) -> Result<PathBuf, String> {
    if status.is_busy() {
        return Err("patcher is already running".to_string());
    }

    let cfg = config::load().map_err(|e| e.to_string())?;
    let install = config::configured_path(&cfg, slot)
        .ok_or_else(|| format!("no {} install configured", slot.as_str()))?;

    let user_server_jar = install.join("Server").join("HytaleServer.jar");
    if !user_server_jar.is_file() {
        return Err(format!(
            "HytaleServer.jar not found at {}",
            user_server_jar.display()
        ));
    }

    let data_dir = data_dir()?;

    // Copy the JAR into Atlas's own data dir so to decompile from a stable
    // snapshot rather than the live install. Two reasons:
    // 1. Hytale updates rewrite the JAR underneath us; the snapshot lets
    // decompile output reflect a known build until the user explicitly
    // re-decompiles.
    // 2. The decompiler reads the JAR repeatedly; pointing at the user's
    // install means a game update mid-decompile would corrupt output.
    // Re-running this command always re-copies, so "Re-decompile" after a
    // game patch is just calling `start_decompile` again.
    let snapshot_install = data_dir.as_path().join("jar-snapshots").join(slot.as_str());
    let snapshot_server_dir = snapshot_install.join("Server");
    std::fs::create_dir_all(&snapshot_server_dir).map_err(|e| {
        format!(
            "creating jar snapshot dir {}: {e}",
            snapshot_server_dir.display()
        )
    })?;
    let snapshot_jar = snapshot_server_dir.join("HytaleServer.jar");
    std::fs::copy(&user_server_jar, &snapshot_jar).map_err(|e| {
        format!(
            "copying {} -> {}: {e}",
            user_server_jar.display(),
            snapshot_jar.display()
        )
    })?;

    let workspace = patcher::workspace_for(data_dir.as_path(), slot);
    std::fs::create_dir_all(&workspace)
        .map_err(|e| format!("creating workspace {}: {e}", workspace.display()))?;

    patcher::spawn_decompile(
        &runtime.0,
        app,
        (*status).clone(),
        slot,
        snapshot_install,
        workspace.clone(),
    );

    Ok(workspace)
}

#[tauri::command]
pub fn patcher_status(status: State<'_, SharedStatus>) -> patcher::status::PatcherStatus {
    status.snapshot()
}

/// Delete a slot's decompile output + metadata (confirmation handled FE-side).
/// Also clears the index for that slot since it now points at files that
/// no longer exist.
#[tauri::command]
pub fn clear_decompile(slot: Slot, catalog: State<'_, Arc<SearchCatalog>>) -> Result<(), String> {
    let data_dir = data_dir()?;
    let workspace = patcher::workspace_for(data_dir.as_path(), slot);
    patcher::clear_slot(&workspace).map_err(|e| e.to_string())?;

    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    indexer::clear_slot(&index_dir).map_err(|e| e.to_string())?;
    let lance_dir = lance::lance_dir_for(data_dir.as_path(), slot);
    lance::clear_slot(&lance_dir).map_err(|e| e.to_string())?;
    catalog.invalidate(slot);
    Ok(())
}

/// Launch `path` in the selected IDE (or File Explorer).
#[tauri::command]
pub fn open_in_ide(ide_id: String, path: PathBuf) -> Result<(), String> {
    let id = IdeId::from_str(&ide_id).ok_or_else(|| format!("unknown IDE id: {ide_id}"))?;
    let ides = ide::detect_ides();
    let target = ides
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("{} is not installed on this machine", id.display_name()))?;
    ide::open_with(target, &path).map_err(|e| e.to_string())
}

// -----------------------------------------------------------------------
// Indexer
// -----------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct IndexOverview {
    pub release: SlotIndexSummary,
    pub pre_release: SlotIndexSummary,
}

/// Per-slot readiness for the Search page + branch card.
#[tauri::command]
pub fn index_overview() -> Result<IndexOverview, String> {
    let data_dir = data_dir()?;

    let release_index = indexer::index_dir_for(data_dir.as_path(), Slot::Release);
    let pre_release_index = indexer::index_dir_for(data_dir.as_path(), Slot::PreRelease);

    let release_decompile =
        patcher::workspace_for(data_dir.as_path(), Slot::Release).join("decompile");
    let pre_release_decompile =
        patcher::workspace_for(data_dir.as_path(), Slot::PreRelease).join("decompile");

    Ok(IndexOverview {
        release: indexer::summarize_slot(Slot::Release, &release_index, &release_decompile),
        pre_release: indexer::summarize_slot(
            Slot::PreRelease,
            &pre_release_index,
            &pre_release_decompile,
        ),
    })
}

/// Current in-flight indexing activity (one at a time, matches patcher model).
#[tauri::command]
pub fn index_status(status: State<'_, SharedIndexerStatus>) -> indexer::status::IndexerStatus {
    status.snapshot()
}

/// Kick off indexing for the given slot. Returns immediately; progress flows
/// via `index:*` events tagged with the slot.
#[tauri::command]
pub fn index_start(
    app: AppHandle,
    status: State<'_, SharedIndexerStatus>,
    catalog: State<'_, Arc<SearchCatalog>>,
    embedder: State<'_, Arc<SharedEmbedder>>,
    runtime: State<'_, RuntimeHandle>,
    slot: Slot,
) -> Result<(), String> {
    if status.is_busy() {
        return Err("indexer is already running".to_string());
    }
    let data_dir = data_dir()?;

    let decompile_dir = patcher::workspace_for(data_dir.as_path(), slot).join("decompile");
    if !decompile_dir.is_dir() {
        return Err(format!("{} has no decompile to index", slot.as_str()));
    }

    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    let lance_dir = lance::lance_dir_for(data_dir.as_path(), slot);
    let model_cache = data_dir.as_path().join("models");

    // Lazy-load BGE-small on first index run. Downloads ~80MB of weights
    // the first time; subsequent runs reuse the cached ONNX session.
    let embedder_instance = embedder
        .get_or_init(model_cache)
        .map_err(|e| format!("loading embedder: {e:#}"))?;

    spawn_index(
        &runtime.0,
        app,
        (*status).clone(),
        (*catalog).clone(),
        embedder_instance,
        slot,
        decompile_dir,
        index_dir,
        lance_dir,
    );
    Ok(())
}

/// Remove a slot's index. (Used only by internal cleanup - `clear_decompile`
/// already wipes the index.)
#[tauri::command]
pub fn clear_index(slot: Slot, catalog: State<'_, Arc<SearchCatalog>>) -> Result<(), String> {
    let data_dir = data_dir()?;
    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    indexer::clear_slot(&index_dir).map_err(|e| e.to_string())?;
    let lance_dir = lance::lance_dir_for(data_dir.as_path(), slot);
    lance::clear_slot(&lance_dir).map_err(|e| e.to_string())?;
    catalog.invalidate(slot);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub query: String,
    pub slot: &'static str,
    pub elapsed_ms: u64,
}

/// Run a hybrid keyword + semantic search against one slot. BM25
/// (Tantivy) and kNN (LanceDB) are blended with Reciprocal Rank
/// Fusion; weights are chosen per query from the shape heuristic in
/// [`crate::search::hybrid`]. Falls back to keyword-only if the
/// vector store hasn't been built for the slot yet.
#[tauri::command]
pub async fn search(
    catalog: State<'_, Arc<SearchCatalog>>,
    embedder: State<'_, Arc<SharedEmbedder>>,
    guides: State<'_, GuidesIndex>,
    slot: Slot,
    query: String,
    limit: Option<usize>,
    // `source_types`: restrict results to one or more sections. `None`
    // (or empty vec) means "all sections". Recognised values match the
    // `source_type` field on every chunk: `source`, `hm_doc`,
    // `hypixel_doc`, `asset`. Unknown values are tolerated (they just
    // match nothing).
    source_types: Option<Vec<String>>,
) -> Result<SearchResponse, String> {
    let limit = limit.unwrap_or(25).clamp(1, 100);
    // The section filter pushes into both Tantivy (via BooleanQuery
    // AND-clause on `source_type`) and Lance (via `only_if` predicate),
    // so the result list is already section-narrowed when it returns.
    // No over-fetch / post-filter needed.
    let section_filter: Option<Vec<String>> = source_types
        .map(|v| v.into_iter().filter(|s| !s.is_empty()).collect())
        .filter(|v: &Vec<String>| !v.is_empty());
    tracing::info!(
        slot = %slot.as_str(),
        query = %query,
        limit,
        section_filter = ?section_filter,
        "tauri search request",
    );
    let data_dir = data_dir()?;
    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    let lance_dir = lance::lance_dir_for(data_dir.as_path(), slot);
    let model_cache = data_dir.as_path().join("models");
    let _decompile_dir = patcher::workspace_for(data_dir.as_path(), slot).join("decompile");

    // Clone Arcs out of Tauri state before any await - State<'_, …> isn't
    // Send, so holding it across an await trips the compiler.
    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();
    let embedder_handle: Arc<SharedEmbedder> = (*embedder).clone();

    // Open the Lance store; missing = keyword-only.
    let lance_store = match lance::LanceStore::open_existing(&lance_dir).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(?err, "lance open failed, falling back to keyword-only");
            None
        }
    };

    // Only load the embedder if actually have a vector store to
    // query. Keeps keyword-only usage on un-indexed slots cheap.
    let embedder_instance: Option<Arc<dyn crate::embedder::Embedder>> = if lance_store.is_some() {
        match embedder_handle.get_or_init(model_cache.clone()) {
            Ok(e) => Some(e),
            Err(err) => {
                tracing::warn!(?err, "embedder load failed, falling back to keyword-only");
                None
            }
        }
    } else {
        None
    };

    let _ = model_cache;

    // Decide which backends to query based on the section filter.
    // - "hm_doc" routes to the decoupled guides backend (BM25-only,
    // independent index lifecycle - see `crate::guides`).
    // - Everything else routes to the hybrid source/Javadoc/asset path.
    let want_source_section = match &section_filter {
        None => true,
        Some(types) => types.iter().any(|t| t != "hm_doc"),
    };
    let want_guides = match &section_filter {
        None => true,
        Some(types) => types.iter().any(|t| t == "hm_doc"),
    };
    // The hybrid path no longer needs to see "hm_doc" because guides
    // live in their own index. Strip it so the BM25/Lance section
    // filter doesn't end up matching nothing.
    let hybrid_filter: Option<Vec<String>> = section_filter.as_ref().map(|types| {
        types
            .iter()
            .filter(|t| t.as_str() != "hm_doc")
            .cloned()
            .collect()
    });
    let hybrid_filter = hybrid_filter.filter(|v: &Vec<String>| !v.is_empty());

    let guides_handle: GuidesIndex = (*guides).clone();
    let query_for_guides = query.clone();

    let start = std::time::Instant::now();

    let mut hits: Vec<SearchHit> = if want_source_section {
        crate::search::hybrid::run(
            catalog_arc,
            lance_store,
            embedder_instance,
            slot,
            &index_dir,
            &query,
            limit,
            hybrid_filter,
        )
        .await
        .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    if want_guides {
        // Guides have their own lane in the result list (frontend splits
        // by `source_type == "hm_doc"`), so they get their own cap and
        // are NOT bounded by the source-lane `limit`. Otherwise a
        // chatty guides hit would push real source rows out of the
        // user's 10-slot top-N. Cap is a generous fixed value - the
        // guides lane scrolls - but bounded so a degenerate query
        // doesn't ship a 1000-hit payload over IPC.
        const GUIDES_LIMIT: usize = 50;
        let slot_label = slot.as_str().to_string();
        let guides_hits_res = tokio::task::spawn_blocking(move || {
            guides_handle.search(&query_for_guides, GUIDES_LIMIT, &slot_label)
        })
        .await
        .map_err(|e| format!("guides task panicked: {e}"))?;
        match guides_hits_res {
            Ok(mut g) => hits.append(&mut g),
            Err(err) => tracing::warn!(?err, "guides search failed"),
        }
        // No global sort/truncate: the frontend splits by
        // `source_type` into separate lanes, so source order is
        // preserved within its lane and guides are appended in their
        // own BM25-score order.
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    Ok(SearchResponse {
        hits,
        query,
        slot: slot.as_str(),
        elapsed_ms,
    })
}

/// Look up the cross-section sibling for a hit - given a source-code class
/// returns its Javadoc page (and vice versa). Returns `null` when no
/// pair exists in the index. Used by the file viewer's split sibling
/// pane (#4) so the user sees source + Javadoc side-by-side.
#[tauri::command]
pub fn find_sibling(
    catalog: State<'_, Arc<SearchCatalog>>,
    slot: Slot,
    fqn: String,
    source_type: String,
) -> Result<Option<SearchHit>, String> {
    let data_dir = data_dir()?;
    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();
    catalog_arc
        .find_sibling(slot, &index_dir, &fqn, &source_type)
        .map_err(|e| e.to_string())
}

/// One inline Javadoc card returned by [`get_inline_javadocs`]. Maps 1:1
/// to the frontend's `InlineAnchor`: the source viewer renders `prose`
/// inside a tinted card spliced just above `start_line`. `kind` lets the
/// frontend differentiate the class-level card from per-method cards.
#[derive(Debug, Clone, Serialize)]
pub struct InlineJavadoc {
    pub start_line: u32,
    pub kind: &'static str,
    pub header: String,
    pub prose: String,
    pub deprecated: bool,
}

/// Resolve every inline Javadoc card for a class FQN, ready to splice
/// into the source viewer. Returns the class-level card
/// at line 1 plus one card per method whose Javadoc heading matches a
/// declared method in the source. Unmatched Javadoc methods (overload
/// ambiguity, drift between docs and source) are dropped silently.
///
/// Returns `Ok(vec![])` when the class has no Javadoc cached - that's
/// the common "internal class with no public docs" case, not an error.
#[tauri::command]
pub fn get_inline_javadocs(
    catalog: State<'_, Arc<SearchCatalog>>,
    slot: Slot,
    class_fqn: String,
) -> Result<Vec<InlineJavadoc>, String> {
    let data_dir = data_dir()?;
    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    // Per-slot Javadoc HTML, populated at mount time from the artifact's
    // `javadocs/` payload. The dev-machine `atlas_cache_root()/javadocs/`
    // is no longer the runtime source - if a build shipped without docs
    // the user gets no inline cards, full stop, instead of accidentally
    // borrowing the dev box's cache.
    let cache_dir = indexer::javadocs_dir_for(data_dir.as_path(), slot);
    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();

    let parsed = catalog_arc
        .class_javadoc(slot, &index_dir, &cache_dir, &class_fqn)
        .map_err(|e| e.to_string())?;
    let Some((type_description, methods)) = parsed else {
        return Ok(Vec::new());
    };

    // Source-side methods. If symbols.sqlite isn't present (older
    // artifacts), bail to class-level only - no per-method anchors but
    // the class card still renders.
    let symbols_path = index_dir.join("symbols.sqlite");
    let source_methods: Vec<crate::indexer::symbols::MethodRow> = if symbols_path.exists() {
        match crate::indexer::symbols::SymbolsDb::open_read_only(&symbols_path) {
            Ok(db) => db.methods_for_class(&class_fqn).unwrap_or_default(),
            Err(err) => {
                tracing::debug!(?err, fqn = %class_fqn, "symbols db open failed");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let simple_class = class_fqn
        .rsplit('.')
        .next()
        .unwrap_or(&class_fqn)
        .to_string();
    let mut out: Vec<InlineJavadoc> = Vec::new();

    if !type_description.is_empty() {
        out.push(InlineJavadoc {
            start_line: 1,
            kind: "class",
            header: format!("Javadoc · {simple_class}"),
            prose: type_description,
            deprecated: false,
        });
    }

    // Pair each Javadoc method to a source method. First pass: exact
    // (name, param_simple_types) match. Second pass: fall back to
    // name-only when exactly one source method has that name. Methods
    // already paired are removed from the working set so a tied
    // overload doesn't silently consume the second match.
    let mut pool: Vec<crate::indexer::symbols::MethodRow> = source_methods
        .into_iter()
        .filter(|m| !m.is_constructor)
        .collect();

    let mut paired: Vec<(InlineJavadoc, ())> = Vec::new();

    // Pass 1: exact match.
    for jdoc in methods.iter() {
        let pos = pool
            .iter()
            .position(|m| m.name == jdoc.name && m.param_simple_types == jdoc.param_simple_types);
        if let Some(idx) = pos {
            let m = pool.remove(idx);
            let header = format!(
                "{simple_class}.{}({})",
                jdoc.name,
                jdoc.param_simple_types.join(", ")
            );
            paired.push((
                InlineJavadoc {
                    start_line: m.start_line.max(1),
                    kind: "method",
                    header,
                    prose: jdoc.prose.clone(),
                    deprecated: jdoc.deprecated,
                },
                (),
            ));
        }
    }

    // Pass 2: name-only fallback. Build a histogram of remaining method
    // names; only fall through when exactly one source method shares
    // the Javadoc name (so don't guess on ambiguous overloads).
    let mut name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for m in &pool {
        *name_counts.entry(m.name.clone()).or_insert(0) += 1;
    }
    for jdoc in methods.iter() {
        // Skip if already paired.
        if paired.iter().any(|(p, _)| {
            p.kind == "method"
                && p.header
                    .starts_with(&format!("{simple_class}.{}(", jdoc.name))
                && p.prose == jdoc.prose
        }) {
            continue;
        }
        if name_counts.get(&jdoc.name).copied().unwrap_or(0) != 1 {
            continue;
        }
        let pos = pool.iter().position(|m| m.name == jdoc.name);
        if let Some(idx) = pos {
            let m = pool.remove(idx);
            let header = format!(
                "{simple_class}.{}({})",
                jdoc.name,
                jdoc.param_simple_types.join(", ")
            );
            paired.push((
                InlineJavadoc {
                    start_line: m.start_line.max(1),
                    kind: "method",
                    header,
                    prose: jdoc.prose.clone(),
                    deprecated: jdoc.deprecated,
                },
                (),
            ));
            name_counts.insert(jdoc.name.clone(), 0);
        }
    }

    out.extend(paired.into_iter().map(|(p, _)| p));
    out.sort_by_key(|j| j.start_line);
    Ok(out)
}

/// Bulk variant of [`find_sibling`] for the search-result fold pass
///. Given a list of class FQNs from Javadoc-only hits,
/// resolve each to its source-code sibling so the result list can swap
/// the Javadoc row for the source row that renders inline Javadocs in
/// the viewer.
///
/// Missing siblings come back as `None` in the map - caller leaves the
/// Javadoc hit in place when source isn't indexed for that class.
#[tauri::command]
pub fn find_source_siblings(
    catalog: State<'_, Arc<SearchCatalog>>,
    slot: Slot,
    fqns: Vec<String>,
) -> Result<std::collections::HashMap<String, Option<SearchHit>>, String> {
    let data_dir = data_dir()?;
    let index_dir = indexer::index_dir_for(data_dir.as_path(), slot);
    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();

    let mut out: std::collections::HashMap<String, Option<SearchHit>> =
        std::collections::HashMap::with_capacity(fqns.len());
    for fqn in fqns {
        if out.contains_key(&fqn) {
            continue;
        }
        match catalog_arc.find_sibling(slot, &index_dir, &fqn, "hypixel_doc") {
            Ok(hit) => {
                out.insert(fqn, hit);
            }
            Err(err) => {
                tracing::debug!(?err, %fqn, "find_sibling failed; treating as no source");
                out.insert(fqn, None);
            }
        }
    }
    Ok(out)
}

/// Read the full source of a hit's underlying file for the right-panel
/// viewer. The base directory depends on which section the hit lives in:
///
/// * `source` (decompiled Java) → `<data_dir>/patcher/<slot>/decompile/`
/// * `hm_doc` (HM Modding markdown guides) → `<cache_root>/hm-docs/site/`
/// * `hypixel_doc` (Hypixel Javadoc HTML) → `<data_dir>/indexes/javadocs/<slot>/`
/// (the stored rel_path already includes the host segment, e.g.
/// `release.server.docs.hytale.com/com/hypixel/.../Foo.html`)
///
/// HM docs still live under the shared dev cache - they aren't shipped
/// in the artifact today. Hypixel Javadocs ship inside the artifact and
/// are unpacked per-slot at mount time, so the runtime path is bound to
/// the active slot rather than the dev-machine cache.
#[tauri::command]
pub fn read_source(slot: Slot, path: String, source_type: String) -> Result<String, String> {
    let data_dir = data_dir()?;
    let base = match source_type.as_str() {
        // Empty string is the legacy fallback for older artifacts that
        // pre-date the `source_type` field; treat as decompiled Java.
        "source" | "" => patcher::workspace_for(data_dir.as_path(), slot).join("decompile"),
        "hm_doc" => atlas_cache_root().join("hm-docs").join("site"),
        "hypixel_doc" => indexer::javadocs_dir_for(data_dir.as_path(), slot),
        other => return Err(format!("unsupported source_type: {other}")),
    };
    let raw = indexer::read_source(&base, &path).map_err(|e| format!("{e:#}"))?;
    // Javadoc pages are HTML and look like a wall of markup in the
    // viewer. Render through the same parser the indexer uses so the
    // user sees the same prose that got chunked + embedded.
    if source_type == "hypixel_doc" {
        if let Some(rendered) = indexer::hypixel_docs::render_class_page(&path, &raw) {
            return Ok(rendered);
        }
        // Fall through to raw HTML if parsing fails - better something
        // than nothing.
    }
    Ok(raw)
}

/// Resolve the shared content cache root (HM docs clone, Hypixel
/// Javadoc mirror, embedder model). Thin wrapper over `crate::cache_root`
/// kept for the local `read_source` call site.
fn atlas_cache_root() -> PathBuf {
    crate::cache_root()
}

/// Resolve the per-user data directory used for indexes, decompiled
/// trees, and the Atlas config file. Centralising this means commands
/// don't each repeat the `ProjectDirs::from("dev", "horizon", "Atlas")`
/// dance and the error message stays consistent.
fn data_dir() -> Result<PathBuf, String> {
    ProjectDirs::from("dev", "horizon", "Atlas")
        .map(|d| d.data_dir().to_path_buf())
        .ok_or_else(|| "no ProjectDirs for Atlas".to_string())
}

// -----------------------------------------------------------------------
// Fetcher - central-hosted artifact download + mount.
// -----------------------------------------------------------------------

/// Start fetching + mounting the artifact at `request.url`. Returns
/// immediately; progress flows via the `fetch:*` events tagged with
/// `buildId`, and the terminal state shows up in `index_fetch_status`.
///
/// The caller is responsible for turning a Hytale version into a URL
/// (the resolver is intentionally out-of-scope for so local
/// file:// URLs work end-to-end before HM's GH Releases flow is live).
#[tauri::command]
pub fn index_fetch(
    app: AppHandle,
    status: State<'_, SharedFetchStatus>,
    catalog: State<'_, Arc<SearchCatalog>>,
    runtime: State<'_, RuntimeHandle>,
    request: FetchRequest,
) -> Result<(), String> {
    if status.is_busy() {
        return Err("fetcher is already running".to_string());
    }
    let data_dir = data_dir()?;
    let indexes_root = fetcher::indexes_root(data_dir.as_path());

    // Transition to a non-Idle state synchronously so a poll right
    // after this call sees the fetch as in-flight.
    status.set(FetchStatus::Phase {
        build_id: request.build_id.clone(),
        phase: fetcher::status::FetchPhase::Resolving,
    });

    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();
    fetcher::spawn_fetch(
        &runtime.0,
        app,
        (*status).clone(),
        indexes_root,
        request,
        move |mounted| {
            tracing::info!(
                build_id = %mounted.build_id,
                path = %mounted.mounted_at.display(),
                "artifact mounted"
            );
            // The fetcher just rewrote `<indexes_root>/{tantivy,lance}/<slot>/`
            // under the hood; any cached `OpenedIndex` for that slot is now
            // stale and must be re-opened on the next search.
            let slot = match mounted.manifest.hytale_patchline.as_deref() {
                Some("pre-release") => Slot::PreRelease,
                _ => Slot::Release,
            };
            catalog_arc.invalidate(slot);
        },
    );
    Ok(())
}

/// Mount a `.tar.zst` artifact already on disk - no network, no
/// resolver. Used by the "Mount from file" button in the Index Catalog
/// so devs (and HM admins testing internal builds) can drop a freshly
/// `atlas-build`-produced artifact straight into the client.
///
/// Reuses the same status + events as `index_fetch` so the UI doesn't
/// need a parallel state machine.
#[tauri::command]
pub fn index_mount_local(
    app: AppHandle,
    status: State<'_, SharedFetchStatus>,
    catalog: State<'_, Arc<SearchCatalog>>,
    runtime: State<'_, RuntimeHandle>,
    tarball_path: String,
) -> Result<(), String> {
    if status.is_busy() {
        return Err("fetcher is already running".to_string());
    }
    let data_dir = data_dir()?;
    let indexes_root = fetcher::indexes_root(data_dir.as_path());

    let tarball = PathBuf::from(&tarball_path);
    let provisional_id = tarball
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("local-artifact")
        .to_string();

    // Same trick as `index_fetch`: flip out of Idle synchronously so a
    // poll right after this call sees the mount as in-flight.
    status.set(FetchStatus::Phase {
        build_id: provisional_id,
        phase: fetcher::status::FetchPhase::Verifying,
    });

    let catalog_arc: Arc<SearchCatalog> = (*catalog).clone();
    fetcher::spawn_mount_local(
        &runtime.0,
        app,
        (*status).clone(),
        indexes_root,
        tarball,
        move |mounted| {
            tracing::info!(
                build_id = %mounted.build_id,
                path = %mounted.mounted_at.display(),
                "artifact mounted from local file"
            );
            let slot = match mounted.manifest.hytale_patchline.as_deref() {
                Some("pre-release") => Slot::PreRelease,
                _ => Slot::Release,
            };
            catalog_arc.invalidate(slot);
        },
    );
    Ok(())
}

/// Snapshot of the current fetch state for the Index Catalog UX
/// and the status bar.
#[tauri::command]
pub fn index_fetch_status(status: State<'_, SharedFetchStatus>) -> FetchStatus {
    status.snapshot()
}

/// One mounted artifact as seen by the Index Catalog UX.
#[derive(Debug, Serialize)]
pub struct MountedIndexEntry {
    pub build_id: String,
    pub path: PathBuf,
    pub manifest: fetcher::manifest::Manifest,
    /// Total bytes on disk for the mounted dir.
    pub size_bytes: u64,
}

/// List everything currently mounted under `<data>/indexes/`. Only
/// dirs with a `.ok` marker are reported - in-flight or half-extracted
/// mounts are skipped.
#[tauri::command]
pub fn index_catalog() -> Result<Vec<MountedIndexEntry>, String> {
    let data_dir = data_dir()?;
    let root = fetcher::indexes_root(data_dir.as_path());
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let entries =
        std::fs::read_dir(&root).map_err(|e| format!("listing {}: {e}", root.display()))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !fetcher::mount::is_mounted(&path) {
            continue;
        }
        let manifest_bytes = match std::fs::read(path.join("manifest.json")) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let manifest = match fetcher::manifest::Manifest::from_bytes(&manifest_bytes) {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "skipping unreadable manifest");
                continue;
            }
        };
        let size_bytes = dir_size_bytes(&path);
        out.push(MountedIndexEntry {
            build_id: manifest.build_id.clone(),
            path,
            manifest,
            size_bytes,
        });
    }
    out.sort_by(|a, b| a.build_id.cmp(&b.build_id));
    Ok(out)
}

fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() {
            if let Ok(md) = entry.metadata() {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

// -----------------------------------------------------------------------
// Remote resolver, mount management, active-build selection.
// Three commands powering the IndexCatalog UX:
//   - index_resolve_remote: ask GH Releases what build is current
//   - index_remove        : delete a mounted build
//   - index_set_active    : pick which mounted build search uses
// -----------------------------------------------------------------------

/// What `index_resolve_remote` returns to the frontend. `None` means the
/// configured central repo has no published artifact for this patchline;
/// `Some` carries the URL the user can hand to `index_fetch`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteBuildResolution {
    pub build_id: String,
    pub url: String,
    pub release_tag: String,
    /// The Hytale `Implementation-Version` carried in the release body, if
    /// the workflow surfaced it. Surfaced for the UI's "Newer build available"
    /// card.
    pub hytale_impl_version: Option<String>,
}

/// Ask GitHub Releases for the latest signed artifact matching `patchline`
/// in the configured `central_repo`. Public-only API, no auth - we accept
/// the unauthenticated rate limit because resolves are rare (manual click
/// or once-on-launch).
#[tauri::command]
pub async fn index_resolve_remote(
    patchline: Slot,
) -> Result<Option<RemoteBuildResolution>, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    let repo = cfg.central_repo.trim();
    if repo.is_empty() {
        return Err("no central_repo configured".to_string());
    }

    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=30");
    let client = reqwest::Client::builder()
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("building http client: {e}"))?;

    let releases: Vec<GhRelease> = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GH releases returned non-2xx: {e}"))?
        .json()
        .await
        .map_err(|e| format!("decoding GH releases JSON: {e}"))?;

    // GH returns releases newest-first by published_at. Walk in order;
    // first asset matching `atlas-index-<patchline>-*.tar.zst` wins.
    let prefix = format!("atlas-index-{}-", patchline.as_str());
    for release in releases {
        if release.draft || release.prerelease_flag_unrelated_to_patchline() {
            // We use `prerelease` only when the user explicitly publishes a
            // GH Release as a prerelease (separate axis from Hytale's
            // pre-release patchline). Skip those.
            continue;
        }
        for asset in &release.assets {
            if asset.name.starts_with(&prefix) && asset.name.ends_with(".tar.zst") {
                let build_id = asset
                    .name
                    .trim_start_matches("atlas-index-")
                    .trim_end_matches(".tar.zst")
                    .to_string();
                return Ok(Some(RemoteBuildResolution {
                    build_id,
                    url: asset.browser_download_url.clone(),
                    release_tag: release.tag_name.clone(),
                    hytale_impl_version: extract_impl_version(&release.body),
                }));
            }
        }
    }
    Ok(None)
}

/// Remove a mounted build from disk. Refuses if it's the only build
/// mounted for its patchline (so the user can't accidentally lock
/// themselves out of search). If the removed build was the active one,
/// the active-build pointer in the config is cleared so the catalog
/// can fall back to whatever else is mounted.
#[tauri::command]
pub fn index_remove(
    catalog: State<'_, Arc<SearchCatalog>>,
    build_id: String,
) -> Result<(), String> {
    if build_id.trim().is_empty() {
        return Err("build_id is empty".to_string());
    }
    let data_dir = data_dir()?;
    let root = fetcher::indexes_root(data_dir.as_path());
    let target = root.join(&build_id);
    if !target.is_dir() {
        return Err(format!("no mounted build with id {build_id}"));
    }

    // Read the target's manifest to learn its patchline.
    let manifest_bytes = std::fs::read(target.join("manifest.json"))
        .map_err(|e| format!("reading manifest for {build_id}: {e}"))?;
    let manifest = fetcher::manifest::Manifest::from_bytes(&manifest_bytes)
        .map_err(|e| format!("parsing manifest for {build_id}: {e}"))?;
    let target_slot = match manifest.hytale_patchline.as_deref() {
        Some("pre-release") => Slot::PreRelease,
        _ => Slot::Release,
    };

    // Count how many other mounts share this patchline.
    let mounts = index_catalog()?;
    let same_patchline_count = mounts
        .iter()
        .filter(|m| match m.manifest.hytale_patchline.as_deref() {
            Some("pre-release") => target_slot == Slot::PreRelease,
            _ => target_slot == Slot::Release,
        })
        .count();
    if same_patchline_count <= 1 {
        return Err(format!(
            "refusing to remove the only mounted {} build (would leave search empty for that patchline)",
            target_slot.as_str()
        ));
    }

    // If this build is currently the active one for its patchline, clear
    // the pointer in the config so the next search falls back cleanly.
    let mut cfg = config::load().map_err(|e| e.to_string())?;
    let cleared = match target_slot {
        Slot::Release => {
            if cfg.active_release_build.as_deref() == Some(build_id.as_str()) {
                cfg.active_release_build = None;
                true
            } else {
                false
            }
        }
        Slot::PreRelease => {
            if cfg.active_pre_release_build.as_deref() == Some(build_id.as_str()) {
                cfg.active_pre_release_build = None;
                true
            } else {
                false
            }
        }
    };
    if cleared {
        config::save(&cfg).map_err(|e| e.to_string())?;
    }

    // Drop catalog handles before deleting from disk - Tantivy holds
    // file handles on Windows that would block the rmdir otherwise.
    catalog.invalidate(target_slot);
    catalog.invalidate_id(&indexer::IndexId::new(&build_id));

    std::fs::remove_dir_all(&target).map_err(|e| format!("removing {}: {e}", target.display()))?;
    Ok(())
}

/// Pick which mounted build the search engine should target for a given
/// patchline. The actual catalog read still happens at search time; this
/// command just records the user's choice so it persists across restarts.
#[tauri::command]
pub fn index_set_active(patchline: Slot, build_id: String) -> Result<(), String> {
    if build_id.trim().is_empty() {
        return Err("build_id is empty".to_string());
    }
    // Verify the build is actually mounted with the right patchline.
    let mounts = index_catalog()?;
    let entry = mounts
        .iter()
        .find(|m| m.build_id == build_id)
        .ok_or_else(|| format!("build {build_id} is not mounted"))?;
    let entry_slot = match entry.manifest.hytale_patchline.as_deref() {
        Some("pre-release") => Slot::PreRelease,
        _ => Slot::Release,
    };
    if entry_slot != patchline {
        return Err(format!(
            "build {build_id} is a {} build, can't be active for {}",
            entry_slot.as_str(),
            patchline.as_str()
        ));
    }

    let mut cfg = config::load().map_err(|e| e.to_string())?;
    match patchline {
        Slot::Release => cfg.active_release_build = Some(build_id),
        Slot::PreRelease => cfg.active_pre_release_build = Some(build_id),
    }
    config::save(&cfg).map_err(|e| e.to_string())
}

// --- GH Releases API decoding ------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default, rename = "prerelease")]
    is_gh_prerelease: bool,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

impl GhRelease {
    /// GitHub's "prerelease" flag is independent of Hytale's "pre-release"
    /// patchline. We use the GH flag only as an "in-flight, don't pick
    /// this yet" marker; the patchline comes from the asset filename.
    fn prerelease_flag_unrelated_to_patchline(&self) -> bool {
        self.is_gh_prerelease
    }
}

#[derive(Debug, serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Pull `Implementation-Version: <value>` out of the release body if the
/// workflow embedded it. Surfaced for the "newer build available" card.
fn extract_impl_version(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("Implementation-Version:") {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// =====================================================================
// Project mode (Step 4): per-user mod source registration + indexing.
// =====================================================================

use crate::project::{
    self, index::ProjectSink, ProjectId, ProjectRegistry, RegisteredProject, SharedProjectRegistry,
};

/// JSON view of a registered project for the frontend. Mirrors
/// `RegisteredProject` 1:1 plus a derived `index_ready` flag the UI
/// uses to decide between "Index now" and "Re-index" / "Search".
#[derive(Debug, Clone, Serialize)]
pub struct ProjectListEntry {
    pub id: String,
    pub name: String,
    pub source_path: String,
    pub created_at: String,
    pub last_indexed_at: Option<String>,
    pub index_ready: bool,
}

fn to_entry(reg: &ProjectRegistry, p: &RegisteredProject) -> ProjectListEntry {
    let index_dir = reg.project_index_dir(&p.id);
    // The same readiness probe SearchCatalog uses internally - presence
    // of `atlas-meta.json` is the canonical "this index is committed"
    // marker, written at the end of `indexer::run`.
    let index_ready = index_dir.join("atlas-meta.json").exists();
    ProjectListEntry {
        id: p.id.as_str().to_string(),
        name: p.name.clone(),
        source_path: p.source_path.to_string_lossy().into_owned(),
        created_at: p.created_at.clone(),
        last_indexed_at: p.last_indexed_at.clone(),
        index_ready,
    }
}

/// Register a folder as a project. Idempotent: re-registering the same
/// path returns the same id with no side effects beyond an updated
/// display name when one is supplied.
#[tauri::command]
pub fn project_register(
    registry: State<'_, Arc<SharedProjectRegistry>>,
    path: String,
    name: Option<String>,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    registry
        .with(|r| r.register(&p, name))
        .map(|id| id.into_string())
        .map_err(|e| format!("{e:#}"))
}

/// Snapshot of the current project list. Cheap; called on every
/// IndexCatalog / Settings render.
#[tauri::command]
pub fn project_list(registry: State<'_, Arc<SharedProjectRegistry>>) -> Vec<ProjectListEntry> {
    registry.with(|r| r.list().iter().map(|p| to_entry(r, p)).collect())
}

/// Drop the project from the registry AND wipe its index dir on disk.
/// Errors on unknown id; index-dir removal failures are swallowed (the
/// registry entry comes off either way - a stuck index dir shouldn't
/// strand the project as undeletable).
#[tauri::command]
pub fn project_unregister(
    catalog: State<'_, Arc<SearchCatalog>>,
    registry: State<'_, Arc<SharedProjectRegistry>>,
    id: String,
) -> Result<(), String> {
    let pid = ProjectId::from(id);
    registry
        .with(|r| {
            // Release the catalog's cached reader before rmtree so
            // Tantivy file handles don't pin the directory on Windows.
            // Mirrors the ordering used by `index_remove`.
            catalog.invalidate_id(&r.index_id(&pid));
            if let Err(err) = r.remove_index_dir(&pid) {
                tracing::warn!(?err, project_id = %pid, "removing project index dir failed");
            }
            r.unregister(&pid)
        })
        .map_err(|e| format!("{e:#}"))
}

/// Wipe just the index dir, keep the registry entry. UI shows the row
/// as "not indexed yet" afterwards. Doesn't error if the index doesn't
/// exist - this is a "reset to clean state" operation.
#[tauri::command]
pub fn project_remove_index(
    catalog: State<'_, Arc<SearchCatalog>>,
    registry: State<'_, Arc<SharedProjectRegistry>>,
    id: String,
) -> Result<(), String> {
    let pid = ProjectId::from(id);
    registry
        .with(|r| {
            catalog.invalidate_id(&r.index_id(&pid));
            r.remove_index_dir(&pid)
        })
        .map_err(|e| format!("{e:#}"))
}

/// Kick off a project index run on the shared runtime. Emits
/// `project:phase` / `project:progress` / `project:done` events tagged
/// with the project id; returns immediately so the UI can subscribe.
#[tauri::command]
pub fn project_index(
    app: AppHandle,
    rt: State<'_, RuntimeHandle>,
    catalog: State<'_, Arc<SearchCatalog>>,
    registry: State<'_, Arc<SharedProjectRegistry>>,
    embedder_state: State<'_, Arc<SharedEmbedder>>,
    id: String,
) -> Result<(), String> {
    let pid = ProjectId::from(id);

    // Resolve paths up front so the spawned task doesn't have to touch
    // the registry mutex from the indexer thread.
    let (source_path, index_dir, lance_dir, index_id) = registry.with(|r| {
        let p = r
            .get(&pid)
            .ok_or_else(|| format!("no project with id {pid}"))?;
        Ok::<_, String>((
            p.source_path.clone(),
            r.project_index_dir(&pid),
            r.project_lance_dir(&pid),
            r.index_id(&pid),
        ))
    })?;

    // Resolve the embedder model cache the same way `commands::index_start`
    // does so the BGE-small download is shared across desktop indexing
    // and project indexing.
    let model_cache = crate::cache_root().join("models");
    if let Err(err) = std::fs::create_dir_all(&model_cache) {
        return Err(format!(
            "creating embedder model cache {}: {err:#}",
            model_cache.display()
        ));
    }
    let embedder = embedder_state
        .get_or_init(model_cache)
        .map_err(|e| format!("loading embedder: {e:#}"))?;

    let sink = Arc::new(ProjectSink {
        app: app.clone(),
        project_id: pid.clone(),
    });

    let app_for_task = app.clone();
    let catalog_for_task: Arc<SearchCatalog> = (*catalog).clone();
    let registry_for_task: Arc<SharedProjectRegistry> = (*registry).clone();
    let project_id_for_task = pid.clone();
    let index_id_for_task = index_id;

    rt.0.spawn(async move {
        let result = project::index::run_project_index(
            embedder,
            project_id_for_task.clone(),
            source_path,
            index_dir,
            lance_dir,
            sink,
        )
        .await;
        match result {
            Ok(()) => {
                // Drop any stale cached reader so the next search opens
                // the freshly-built index. Same ordering rule as the
                // fetch / remove paths.
                catalog_for_task.invalidate_id(&index_id_for_task);
                registry_for_task.with(|r| {
                    if let Err(err) = r.mark_indexed(&project_id_for_task) {
                        tracing::warn!(?err, "mark_indexed failed");
                    }
                });
            }
            Err(err) => {
                tracing::error!(?err, project_id = %project_id_for_task, "project indexer failed");
                let _ = app_for_task.emit(
                    "project:error",
                    serde_json::json!({
                        "project_id": project_id_for_task.as_str(),
                        "message": format!("{err:#}"),
                    }),
                );
            }
        }
    });

    Ok(())
}

// =====================================================================
// Diff tracker (Step 5): "what would break in MY mod if Hytale shipped X".
// =====================================================================

/// Resolve a registered project id + two mounted build ids into a
/// [`crate::diff::DiffReport`]. Synchronous from the frontend's
/// perspective - the diff itself is fast (single-pass query loop) and
/// rarely worth streaming.
#[tauri::command]
pub fn diff_run(
    registry: State<'_, Arc<SharedProjectRegistry>>,
    project_id: String,
    baseline_build_id: String,
    target_build_id: String,
) -> Result<crate::diff::DiffReport, String> {
    if baseline_build_id == target_build_id {
        return Err("baseline and target are the same build".to_string());
    }

    // Project source dir comes from the registry.
    let pid = ProjectId::from(project_id);
    let project_dir = registry.with(|r| {
        r.get(&pid)
            .map(|p| p.source_path.clone())
            .ok_or_else(|| format!("no project with id {pid}"))
    })?;

    // Both builds must be currently mounted. Resolve symbols.sqlite via
    // the helper so we don't care whether it's at <root>/symbols.sqlite
    // (Hytale builds) or <root>/tantivy/symbols.sqlite (project nested).
    let mounts = index_catalog()?;
    let (baseline_root, baseline_label) = lookup_mount(&mounts, &baseline_build_id)?;
    let (target_root, target_label) = lookup_mount(&mounts, &target_build_id)?;

    let baseline_symbols = crate::diff::pick_symbols_path(&baseline_root).ok_or_else(|| {
        format!(
            "no symbols.sqlite in baseline at {}",
            baseline_root.display()
        )
    })?;
    let target_symbols = crate::diff::pick_symbols_path(&target_root)
        .ok_or_else(|| format!("no symbols.sqlite in target at {}", target_root.display()))?;

    crate::diff::run_project_diff(
        &project_dir,
        baseline_label,
        &baseline_symbols,
        target_label,
        &target_symbols,
    )
    .map_err(|e| format!("{e:#}"))
}

/// Corpus-wide compare between two mounted builds. Reuses the same
/// mount-lookup + symbols.sqlite probe that `diff_run` does.
#[tauri::command]
pub fn index_compare(
    baseline_build_id: String,
    target_build_id: String,
) -> Result<crate::diff::compare::CompareReport, String> {
    if baseline_build_id == target_build_id {
        return Err("baseline and target are the same build".to_string());
    }
    let mounts = index_catalog()?;
    let (baseline_root, baseline_label) = lookup_mount(&mounts, &baseline_build_id)?;
    let (target_root, target_label) = lookup_mount(&mounts, &target_build_id)?;

    let baseline_symbols = crate::diff::pick_symbols_path(&baseline_root).ok_or_else(|| {
        format!(
            "no symbols.sqlite in baseline at {}",
            baseline_root.display()
        )
    })?;
    let target_symbols = crate::diff::pick_symbols_path(&target_root)
        .ok_or_else(|| format!("no symbols.sqlite in target at {}", target_root.display()))?;

    let baseline_db = crate::indexer::symbols::SymbolsDb::open_read_only(&baseline_symbols)
        .map_err(|e| format!("opening baseline symbols.sqlite: {e:#}"))?;
    let target_db = crate::indexer::symbols::SymbolsDb::open_read_only(&target_symbols)
        .map_err(|e| format!("opening target symbols.sqlite: {e:#}"))?;

    crate::diff::compare::compare(&baseline_db, baseline_label, &target_db, target_label)
        .map_err(|e| format!("{e:#}"))
}

/// Find a mounted build by id and return `(root_path, friendly_label)`.
/// The label is what the report quotes in headings; we prefer the
/// human-readable patchline + Hytale impl-version, falling back to the
/// raw build id if the manifest is sparse.
fn lookup_mount(mounts: &[MountedIndexEntry], build_id: &str) -> Result<(PathBuf, String), String> {
    let entry = mounts
        .iter()
        .find(|m| m.build_id == build_id)
        .ok_or_else(|| format!("build {build_id} is not mounted"))?;
    let label = match entry.manifest.hytale_patchline.as_deref() {
        Some(p) if !entry.manifest.hytale_impl_version.is_empty() => {
            format!("{p} · {}", entry.manifest.hytale_impl_version)
        }
        Some(p) => p.to_string(),
        None if !entry.manifest.hytale_impl_version.is_empty() => {
            entry.manifest.hytale_impl_version.clone()
        }
        None => build_id.to_string(),
    };
    Ok((entry.path.clone(), label))
}

// =====================================================================
// User state (Step 7): pins, notes, recent files. Backed by state.sqlite.
// =====================================================================

use crate::state::{Pin, PinKind, RecentFile, StateDb};

/// Pin a file / query / symbol so the user can find it again later.
/// Idempotent: a second call for the same `(kind, target, build_id)`
/// returns the existing row.
#[tauri::command]
pub fn state_pin_add(
    db: State<'_, Arc<StateDb>>,
    kind: PinKind,
    target: String,
    build_id: Option<String>,
    label: Option<String>,
) -> Result<Pin, String> {
    db.pin_add(kind, &target, build_id.as_deref(), label.as_deref())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn state_pin_remove(db: State<'_, Arc<StateDb>>, id: i64) -> Result<(), String> {
    db.pin_remove(id).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn state_pin_list(db: State<'_, Arc<StateDb>>) -> Result<Vec<Pin>, String> {
    db.pin_list().map_err(|e| format!("{e:#}"))
}

/// Set or clear a note. Empty `body` deletes the note row.
#[tauri::command]
pub fn state_note_set(
    db: State<'_, Arc<StateDb>>,
    pin_id: i64,
    body: String,
) -> Result<(), String> {
    db.note_set(pin_id, &body).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn state_note_get(db: State<'_, Arc<StateDb>>, pin_id: i64) -> Result<Option<String>, String> {
    db.note_get(pin_id).map_err(|e| format!("{e:#}"))
}

/// Record that the user just opened `(path, build_id)`. Bounds the
/// recent_files table to the most recent N rows automatically.
#[tauri::command]
pub fn state_recent_file_record(
    db: State<'_, Arc<StateDb>>,
    path: String,
    build_id: String,
) -> Result<(), String> {
    db.recent_file_record(&path, &build_id)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn state_recent_files(db: State<'_, Arc<StateDb>>) -> Result<Vec<RecentFile>, String> {
    db.recent_files().map_err(|e| format!("{e:#}"))
}
