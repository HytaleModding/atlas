//! Atlas - Tauri entry point.
//!
//! Boots the embedded Axum HTTP server on startup, then launches the Tauri
//! window. Future phases hang the indexer, search, and MCP surfaces off the
//! same Tokio runtime.

mod commands;
pub mod config;
// Modules consumed by the `atlas-build` bin target are
// `pub` so the CLI can orchestrate them through the library crate.
// Everything else stays `mod` to keep the desktop-only surface private.
pub mod diff;
pub mod embedder;
pub mod eval;
pub mod fetcher;
pub mod guides;
mod http;
pub mod indexer;
pub mod lance;
pub mod mcp;
pub mod patcher;
pub mod project;
pub mod search;
pub mod state;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Shared multi-threaded runtime that hosts the Axum server *and* any
/// background jobs spawned from Tauri commands (e.g. the decompile pipeline).
/// Tauri runs commands on its own threads, so commands access this via
/// `RuntimeHandle` in managed state and submit work with `handle.spawn(...)`.
#[derive(Clone)]
pub struct RuntimeHandle(pub tokio::runtime::Handle);

/// Shared content cache root used by all Atlas binaries (HM docs clone,
/// Hypixel Javadoc mirror, embedder model files). Resolution order:
/// 1. `ATLAS_CACHE_ROOT` env var if set.
/// 2. Platform cache dir from `directories::ProjectDirs` (e.g.
///    `%LOCALAPPDATA%\horizon\Atlas\cache` on Windows,
///    `~/Library/Caches/dev.horizon.Atlas` on macOS,
///    `~/.cache/atlas` on Linux).
/// 3. `./atlas-cache` as a last resort if `ProjectDirs` returns `None`,
///    which only happens on platforms without a meaningful HOME.
pub fn cache_root() -> PathBuf {
    if let Some(p) = std::env::var_os("ATLAS_CACHE_ROOT") {
        return PathBuf::from(p);
    }
    if let Some(dirs) = directories::ProjectDirs::from("dev", "horizon", "Atlas") {
        return dirs.cache_dir().to_path_buf();
    }
    PathBuf::from("./atlas-cache")
}

static RUNTIME: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("atlas-rt")
            .build()
            .expect("failed to build Tokio runtime"),
    );
    let handle = runtime.handle().clone();
    // Keep the runtime alive for the process lifetime.
    let _ = RUNTIME.set(runtime);

    // Shared state. Built here (not inside `.manage()`) so the same
    // Arcs back both the Tauri commands and the MCP surface. A search
    // over IPC and a search over MCP now hit the same cached readers.
    let catalog = Arc::new(indexer::SearchCatalog::new());
    let embedder = Arc::new(embedder::SharedEmbedder::new());
    let data_dir = directories::ProjectDirs::from("dev", "horizon", "Atlas")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // Project mode registry. Loaded once at boot; mutations re-serialize
    // `<data_dir>/projects.json` in place. We `expect` here on purpose:
    // a parse failure means the user has a registry on disk we can't
    // read, and silently starting empty would overwrite it on first
    // mutation. Loud failure is the safer default - the user can
    // inspect/repair the JSON and relaunch.
    let project_registry = Arc::new(project::SharedProjectRegistry::new(
        project::ProjectRegistry::load(&data_dir)
            .expect("failed to load project registry"),
    ));

    // User-state persistence (pins, notes, recent files). Same loud-fail
    // posture as the project registry: a corrupt state.sqlite is the
    // user's data and silently replacing it would lose pins.
    let state_db = Arc::new(
        state::StateDb::open_or_create(&data_dir).expect("opening state.sqlite"),
    );

    // Reap any half-extracted index dirs left behind by a prior crash
    // before SearchCatalog gets a chance to look at them. Cheap - only
    // directory scan + rm -rf of unmarked dirs.
    let indexes_root = fetcher::indexes_root(&data_dir);
    if let Err(err) = fetcher::mount::reap_stale(&indexes_root) {
        tracing::warn!(?err, "reap_stale failed at startup");
    }

    // Boot Axum + MCP on the shared runtime.
    let mcp_state = mcp::McpState {
        catalog: catalog.clone(),
        embedder: embedder.clone(),
        data_dir: data_dir.clone(),
    };
    handle.spawn(async move {
        match http::serve(mcp_state).await {
            Ok(port) => tracing::info!("atlas backend ready on port {port}"),
            Err(err) => tracing::error!("failed to start atlas backend: {err}"),
        }
    });

    // Lightweight, decoupled HM docs guides backend (BM25-only). Lives
    // outside the source-section index lifecycle so guides refresh
    // cheaply without forcing a full re-index.
    let guides_repo = cache_root().join("hm-docs").join("site");
    let guides_index = guides::GuidesIndex::new(data_dir.clone(), guides_repo);
    {
        // Sync at startup on the runtime so app boot isn't blocked on
        // the first index walk.
        let g = guides_index.clone();
        handle.spawn(async move {
            // walk_docs is blocking I/O; spawn_blocking keeps the
            // async runtime threads free.
            let res = tokio::task::spawn_blocking(move || g.sync_and_refresh()).await;
            match res {
                Ok(Ok(())) => tracing::info!("guides index ready"),
                Ok(Err(err)) => tracing::warn!(?err, "guides sync failed"),
                Err(err) => tracing::warn!(?err, "guides sync task panicked"),
            }
        });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(patcher::status::SharedStatus::new())
        .manage(indexer::status::SharedIndexerStatus::new())
        .manage(catalog)
        .manage(embedder)
        .manage(fetcher::status::SharedFetchStatus::new())
        .manage(guides_index)
        .manage(project_registry)
        .manage(state_db)
        .manage(RuntimeHandle(handle))
        .invoke_handler(tauri::generate_handler![
            commands::load_config,
            commands::save_config,
            commands::validate_hytale_path,
            commands::start_decompile,
            commands::patcher_status,
            commands::patcher_overview,
            commands::clear_decompile,
            commands::open_in_ide,
            commands::index_start,
            commands::index_status,
            commands::index_overview,
            commands::clear_index,
            commands::search,
            commands::find_sibling,
            commands::find_source_siblings,
            commands::read_source,
            commands::get_inline_javadocs,
            commands::index_fetch,
            commands::index_mount_local,
            commands::index_fetch_status,
            commands::index_catalog,
            commands::index_resolve_remote,
            commands::index_remove,
            commands::index_set_active,
            commands::project_register,
            commands::project_list,
            commands::project_unregister,
            commands::project_remove_index,
            commands::project_index,
            commands::diff_run,
            commands::index_compare,
            commands::state_pin_add,
            commands::state_pin_remove,
            commands::state_pin_list,
            commands::state_note_set,
            commands::state_note_get,
            commands::state_recent_file_record,
            commands::state_recent_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
