//! Patcher - Rust-native port of the Horizon patcher's decompile pipeline,
//! slot-aware (release + pre-release) and version-aware.
//!
//! Responsibilities:
//!   1. Ensure a known-good Vineflower JAR is cached on disk (SHA256 pinned).
//!   2. Ensure a usable Java (>= 17) is on PATH.
//!   3. Extract classes from the Hytale server JAR, skipping native libs and
//!      renaming `META-INF/LICENSE` -> `META-INF/LICENSE.renamed` for
//!      case-insensitive filesystems.
//!   4. Invoke Vineflower with the same flags Horizon's `common.py` uses.
//!   5. Persist decompile metadata (version, JAR mtime, timestamps) so the UI
//!      can show Up-to-date / Outdated without rescanning.
//!
//! Runs as a background Tokio task on the shared runtime; progress is reported
//! via `PatcherStatus` and Tauri events (`decompile:phase`, `decompile:progress`,
//! `decompile:done`, `decompile:error`). Every event carries the `slot` so
//! the UI can multiplex against the active branch.

pub mod decompile;
pub mod extract;
pub mod ide;
pub mod java;
pub mod metadata;
pub mod status;
pub mod version;
pub mod vineflower;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::Slot;
use metadata::{format_iso8601, SlotMetadata};
use status::{PatcherPhase, PatcherStatus, SharedStatus};

/// Kick off an async decompile job on the shared runtime. Returns immediately;
/// progress is reported via events + `patcher_status`.
pub fn spawn_decompile(
    rt: &tokio::runtime::Handle,
    app: AppHandle,
    status: SharedStatus,
    slot: Slot,
    install_path: PathBuf,
    workspace: PathBuf,
) {
    rt.spawn(async move {
        if let Err(err) =
            run_decompile(app.clone(), status.clone(), slot, install_path, workspace).await
        {
            tracing::error!(?err, "decompile failed");
            status.set(PatcherStatus::Error {
                message: format!("{err:#}"),
            });
            let _ = app.emit(
                "decompile:error",
                serde_json::json!({ "slot": slot.as_str(), "message": format!("{err:#}") }),
            );
        }
    });
}

async fn run_decompile(
    app: AppHandle,
    status: SharedStatus,
    slot: Slot,
    install_path: PathBuf,
    workspace: PathBuf,
) -> Result<()> {
    let server_jar = install_path.join("Server").join("HytaleServer.jar");
    if !server_jar.is_file() {
        return Err(anyhow!(
            "HytaleServer.jar not found at {}",
            server_jar.display()
        ));
    }

    let emit_phase = |phase: PatcherPhase| {
        let _ = app.emit(
            "decompile:phase",
            serde_json::json!({ "slot": slot.as_str(), "phase": phase.as_str() }),
        );
    };

    // --- Phase: ensure Vineflower ---------------------------------------
    status.set(PatcherStatus::Phase {
        phase: PatcherPhase::EnsuringVineflower,
    });
    emit_phase(PatcherPhase::EnsuringVineflower);

    let vineflower_jar = vineflower::ensure_vineflower(&app, &status)
        .await
        .context("ensuring Vineflower JAR")?;

    // --- Phase: ensure Java ---------------------------------------------
    status.set(PatcherStatus::Phase {
        phase: PatcherPhase::DetectingJava,
    });
    emit_phase(PatcherPhase::DetectingJava);

    let java_path = java::ensure_java()
        .await
        .context("detecting Java 17+ on PATH")?;

    // --- Capture JAR fingerprint before extraction ----------------------
    let jar_meta = tokio::fs::metadata(&server_jar)
        .await
        .with_context(|| format!("stat {}", server_jar.display()))?;
    let jar_size = jar_meta.len();
    let jar_mtime = jar_meta.modified().unwrap_or_else(|_| SystemTime::now());

    // --- Capture Hytale version from the manifest -----------------------
    let hytale_version = {
        let jar_for_version = server_jar.clone();
        tokio::task::spawn_blocking(move || version::read_from_jar(&jar_for_version))
            .await
            .ok()
            .and_then(|r| r.ok())
            .map(|v| v.implementation_version)
    };

    // --- Phase: extract --------------------------------------------------
    let classes_dir = workspace.join("classes");
    tokio::fs::create_dir_all(&classes_dir)
        .await
        .with_context(|| format!("creating {}", classes_dir.display()))?;

    status.set(PatcherStatus::Phase {
        phase: PatcherPhase::Extracting,
    });
    emit_phase(PatcherPhase::Extracting);

    let status_for_extract = status.clone();
    let app_for_extract = app.clone();
    let jar_for_extract = server_jar.clone();
    let classes_for_extract = classes_dir.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        extract::extract_server_jar(
            &jar_for_extract,
            &classes_for_extract,
            &ExtractProgress {
                app: app_for_extract,
                status: status_for_extract,
                slot,
            },
        )
    })
    .await
    .context("extract task panicked")??;

    tracing::info!(
        "extracted {} class files to {}",
        extracted,
        classes_dir.display()
    );

    // --- Phase: decompile ------------------------------------------------
    let decompile_out = workspace.join("decompile");
    tokio::fs::create_dir_all(&decompile_out)
        .await
        .with_context(|| format!("creating {}", decompile_out.display()))?;

    status.set(PatcherStatus::Phase {
        phase: PatcherPhase::Decompiling,
    });
    emit_phase(PatcherPhase::Decompiling);

    decompile::run_vineflower(&java_path, &vineflower_jar, &classes_dir, &decompile_out)
        .await
        .context("running Vineflower")?;

    // --- Persist metadata so the UI can show Up to date vs Outdated -----
    let meta = SlotMetadata {
        decompiled_at: format_iso8601(SystemTime::now()),
        jar_mtime: format_iso8601(jar_mtime),
        jar_size,
        hytale_version,
        vineflower_version: vineflower::VINEFLOWER_VERSION.to_string(),
    };
    if let Err(err) = meta.write(&workspace) {
        tracing::warn!(?err, "failed to write decompile metadata");
    }

    // --- Done ------------------------------------------------------------
    status.set(PatcherStatus::Done {
        output_dir: decompile_out.clone(),
    });
    let _ = app.emit(
        "decompile:done",
        serde_json::json!({
            "slot": slot.as_str(),
            "outputDir": decompile_out.to_string_lossy(),
        }),
    );
    Ok(())
}

/// Progress sink plumbed into the blocking extract loop so it can emit events.
pub struct ExtractProgress {
    app: AppHandle,
    status: SharedStatus,
    slot: Slot,
}

impl extract::ProgressSink for ExtractProgress {
    fn report(&self, current: usize, total: usize) {
        self.status
            .set(PatcherStatus::Extracting { current, total });
        // Only emit every ~1% or every 50 files to avoid flooding the IPC bus.
        if total == 0
            || current == total
            || current % std::cmp::max(1, total / 100) == 0
            || current % 50 == 0
        {
            let _ = self.app.emit(
                "decompile:progress",
                serde_json::json!({
                    "slot": self.slot.as_str(),
                    "phase": PatcherPhase::Extracting.as_str(),
                    "current": current,
                    "total": total,
                }),
            );
        }
    }
}

/// Convenience: compute a workspace directory rooted at the Atlas data dir.
pub fn workspace_for(data_dir: &Path, slot: Slot) -> PathBuf {
    data_dir.join("patcher").join(slot.as_str())
}

/// Delete the decompile output + metadata for a slot (used by "Re-decompile"
/// and by a future "Forget" action).
pub fn clear_slot(workspace: &Path) -> std::io::Result<()> {
    let _ = SlotMetadata::delete(workspace);
    let decompile = workspace.join("decompile");
    let classes = workspace.join("classes");
    if decompile.is_dir() {
        std::fs::remove_dir_all(&decompile)?;
    }
    if classes.is_dir() {
        std::fs::remove_dir_all(&classes)?;
    }
    Ok(())
}

/// What the UI renders for one slot.
#[derive(Debug, Clone, Serialize)]
pub struct SlotOverview {
    pub slot: &'static str,
    pub configured: bool,
    pub install_path: Option<PathBuf>,
    pub default_path: Option<PathBuf>,
    pub jar_path: Option<PathBuf>,
    pub jar_exists: bool,
    pub jar_size: Option<u64>,
    pub jar_mtime: Option<String>,
    pub hytale_version: Option<String>,
    pub decompile: Option<DecompileOverview>,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecompileOverview {
    pub output_dir: PathBuf,
    pub decompiled_at: String,
    pub jar_mtime_at_decompile: String,
    pub hytale_version: Option<String>,
    /// True when the current JAR matches the JAR we last decompiled.
    pub fresh: bool,
}

pub fn build_slot_overview(
    slot: Slot,
    install_path: Option<PathBuf>,
    default_path: Option<PathBuf>,
    workspace: PathBuf,
) -> SlotOverview {
    let configured = install_path.is_some();
    let jar_path = install_path
        .as_ref()
        .map(|p| p.join("Server").join("HytaleServer.jar"));
    let jar_exists = jar_path.as_ref().map(|p| p.is_file()).unwrap_or(false);

    let (jar_size, jar_mtime) = match jar_path.as_ref().filter(|p| p.is_file()) {
        Some(p) => match std::fs::metadata(p) {
            Ok(m) => {
                let mtime = m.modified().ok().map(format_iso8601);
                (Some(m.len()), mtime)
            }
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    let hytale_version = jar_path
        .as_ref()
        .filter(|p| p.is_file())
        .and_then(|p| version::read_from_jar(p).ok())
        .map(|v| v.implementation_version);

    let decompile_out = workspace.join("decompile");
    let decompile = SlotMetadata::read(&workspace).and_then(|m| {
        if !decompile_out.is_dir() {
            return None;
        }
        let fresh = match (&jar_mtime, m.jar_size == jar_size.unwrap_or(0)) {
            (Some(current_mtime), true) => current_mtime == &m.jar_mtime,
            _ => false,
        };
        Some(DecompileOverview {
            output_dir: decompile_out.clone(),
            decompiled_at: m.decompiled_at,
            jar_mtime_at_decompile: m.jar_mtime,
            hytale_version: m.hytale_version,
            fresh,
        })
    });

    SlotOverview {
        slot: slot.as_str(),
        configured,
        install_path,
        default_path,
        jar_path,
        jar_exists,
        jar_size,
        jar_mtime,
        hytale_version,
        decompile,
        output_dir: decompile_out,
    }
}
