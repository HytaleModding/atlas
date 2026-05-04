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
        match indexer::run(embedder, slot, decompile_dir, index_dir, lance_dir, sink, None, None, None).await {
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
pub fn patcher_status(
    status: State<'_, SharedStatus>,
) -> patcher::status::PatcherStatus {
    status.snapshot()
}

/// Delete a slot's decompile output + metadata (confirmation handled FE-side).
/// Also clears the index for that slot since it now points at files that
/// no longer exist.
#[tauri::command]
pub fn clear_decompile(
    slot: Slot,
    catalog: State<'_, Arc<SearchCatalog>>,
) -> Result<(), String> {
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

    let release_decompile = patcher::workspace_for(data_dir.as_path(), Slot::Release).join("decompile");
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
pub fn index_status(
    status: State<'_, SharedIndexerStatus>,
) -> indexer::status::IndexerStatus {
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
        return Err(format!(
            "{} has no decompile to index",
            slot.as_str()
        ));
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
pub fn clear_index(
    slot: Slot,
    catalog: State<'_, Arc<SearchCatalog>>,
) -> Result<(), String> {
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
    let cache_dir = atlas_cache_root().join("javadocs");
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

    let simple_class = class_fqn.rsplit('.').next().unwrap_or(&class_fqn).to_string();
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
        let pos = pool.iter().position(|m| {
            m.name == jdoc.name
                && m.param_simple_types == jdoc.param_simple_types
        });
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
                && p.header.starts_with(&format!("{simple_class}.{}(", jdoc.name))
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
/// * `hypixel_doc` (Hypixel Javadoc HTML) → `<cache_root>/javadocs/`
/// (the stored rel_path already includes the host segment, e.g.
/// `release.server.docs.hytale.com/com/hypixel/.../Foo.html`)
///
/// `<cache_root>` is the same shared cache `atlas-build` writes into;
/// see `crate::cache_root` for the resolution order.
#[tauri::command]
pub fn read_source(slot: Slot, path: String, source_type: String) -> Result<String, String> {
    let data_dir = data_dir()?;
    let base = match source_type.as_str() {
 // Empty string is the legacy fallback for older artifacts that
 // pre-date the `source_type` field; treat as decompiled Java.
        "source" | "" => patcher::workspace_for(data_dir.as_path(), slot).join("decompile"),
        "hm_doc" => atlas_cache_root().join("hm-docs").join("site"),
        "hypixel_doc" => atlas_cache_root().join("javadocs"),
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
    let entries = std::fs::read_dir(&root).map_err(|e| format!("listing {}: {e}", root.display()))?;
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
