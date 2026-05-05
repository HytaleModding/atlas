//! Artifact fetch + mount pipeline.
//!
//! The `fetcher` module owns the client-side half of the central-hosted
//! index pivot: it knows how to resolve, download, verify, extract, and
//! mount a `.tar.zst` artifact produced by `atlas-build`.
//!
//! Data-at-rest pieces (/3.E):
//!   - [`manifest`] - serde + SHA256SUMS + digest helpers.
//!   - [`artifact`] - tar.zst pack/verify.
//!   - [`signing`] - Ed25519 sign / verify + embedded pubkey.
//!
//! Live pieces:
//!   - [`status`] - `FetchStatus` + shared mutex, mirrors
//!     `patcher::status` / `indexer::status`.
//!   - [`download`] - streaming HTTP download with `Range` resume.
//!   - [`mount`]   - verify + extract + atomic rename + `.ok` marker.
//!
//! Events emitted to the front-end (all tagged with `buildId`):
//!   `fetch:phase`, `fetch:progress`, `fetch:done`, `fetch:error`.

pub mod artifact;
pub mod download;
pub mod manifest;
pub mod mount;
pub mod signing;
pub mod status;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use download::{partial_path, DownloadProgress, DownloadRequest};
use mount::{verify_and_mount, wire_legacy_slot, ExtractProgress, MountedArtifact};
use status::{FetchPhase, FetchStatus, SharedFetchStatus};

/// Root directory the client uses to house mounted indexes. Each
/// mounted index lives at `<indexes_root>/<build_id>/` with a `.ok`
/// marker; in-flight downloads/extracts live under `<indexes_root>/.tmp/`.
pub fn indexes_root(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes")
}

/// Payload accepted by the `index_fetch` Tauri command. `build_id`
/// drives event tagging and the final on-disk directory name. `url`
/// is the already-resolved artifact URL - the caller (frontend or the
/// future resolver) is responsible for turning a Hytale version into
/// a URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    pub build_id: String,
    pub url: String,
}

/// Kick off an async fetch job on the shared runtime. Returns immediately;
/// progress reports via `FetchStatus` + Tauri events.
pub fn spawn_fetch(
    rt: &tokio::runtime::Handle,
    app: AppHandle,
    status: SharedFetchStatus,
    indexes_root: PathBuf,
    request: FetchRequest,
    on_mounted: impl FnOnce(MountedArtifact) + Send + 'static,
) {
    rt.spawn(async move {
        let build_id = request.build_id.clone();
        match run_fetch(app.clone(), status.clone(), &indexes_root, request).await {
            Ok(mounted) => {
                status.set(FetchStatus::Done {
                    build_id: mounted.build_id.clone(),
                });
                let _ = app.emit(
                    "fetch:done",
                    serde_json::json!({
                        "buildId": mounted.build_id,
                        "mountedAt": mounted.mounted_at.to_string_lossy(),
                    }),
                );
                on_mounted(mounted);
            }
            Err(err) => {
                tracing::error!(?err, build_id = %build_id, "fetch failed");
                let msg = format!("{err:#}");
                status.set(FetchStatus::Error {
                    build_id: build_id.clone(),
                    message: msg.clone(),
                });
                let _ = app.emit(
                    "fetch:error",
                    serde_json::json!({
                        "buildId": build_id,
                        "message": msg,
                    }),
                );
            }
        }
    });
}

async fn run_fetch(
    app: AppHandle,
    status: SharedFetchStatus,
    indexes_root: &Path,
    request: FetchRequest,
) -> Result<MountedArtifact> {
    let build_id = request.build_id.clone();

    let emit_phase = |phase: FetchPhase| {
        let _ = app.emit(
            "fetch:phase",
            serde_json::json!({ "buildId": build_id, "phase": phase.as_str() }),
        );
    };

    // --- Phase: Resolving -----------------------------------------------
    // The explicit URL flow skips real resolution, but we still emit
    // the phase event so the UI state machine stays in sync with the
    // eventual GH-releases-backed flow.
    status.set(FetchStatus::Phase {
        build_id: build_id.clone(),
        phase: FetchPhase::Resolving,
    });
    emit_phase(FetchPhase::Resolving);

    tokio::fs::create_dir_all(indexes_root)
        .await
        .with_context(|| format!("creating {}", indexes_root.display()))?;
    let tmp_dir = indexes_root.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .with_context(|| format!("creating {}", tmp_dir.display()))?;

    // --- Phase: Downloading ---------------------------------------------
    status.set(FetchStatus::Downloading {
        build_id: build_id.clone(),
        received: 0,
        total: None,
    });
    emit_phase(FetchPhase::Downloading);

    let dest = tmp_dir.join(format!("{}.tar.zst", build_id));
    let client = reqwest::Client::builder()
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client")?;
    let dl_req = DownloadRequest {
        build_id: build_id.clone(),
        url: request.url.clone(),
        dest: dest.clone(),
    };
    let progress = DownloadProgress::new();
    let status_for_download = status.clone();
    let app_for_download = app.clone();
    let build_id_for_download = build_id.clone();
    let mut last_emit_bytes: u64 = 0;
    download::download(
        &client,
        &dl_req,
        progress.clone(),
        move |received, total| {
            status_for_download.set(FetchStatus::Downloading {
                build_id: build_id_for_download.clone(),
                received,
                total,
            });
            // Throttle IPC to ~every 128 KiB to avoid flooding the bus.
            if received.saturating_sub(last_emit_bytes) > 128 * 1024
                || total.is_some_and(|t| received == t)
            {
                last_emit_bytes = received;
                let _ = app_for_download.emit(
                    "fetch:progress",
                    serde_json::json!({
                        "buildId": build_id_for_download,
                        "phase": FetchPhase::Downloading.as_str(),
                        "received": received,
                        "total": total,
                    }),
                );
            }
        },
    )
    .await?;

    // --- Phases: Verifying + Extracting + Mounting ----------------------
    // These are run together on a blocking task because tar-zstd
    // extraction is sync and CPU-bound.
    status.set(FetchStatus::Phase {
        build_id: build_id.clone(),
        phase: FetchPhase::Verifying,
    });
    emit_phase(FetchPhase::Verifying);

    let dest_for_blocking = dest.clone();
    let indexes_root_for_blocking = indexes_root.to_path_buf();
    let status_for_blocking = status.clone();
    let app_for_blocking = app.clone();
    let build_id_for_blocking = build_id.clone();

    let mounted = tokio::task::spawn_blocking(move || -> Result<MountedArtifact> {
        // The extract phase reports progress through this sink.
        let sink = FetchExtractProgress {
            app: app_for_blocking.clone(),
            status: status_for_blocking.clone(),
            build_id: build_id_for_blocking.clone(),
        };

        // verify_and_mount is ordered verify → extract → mount; we
        // emit the Extracting phase just before the blocking call so
        // the UI flips from "Verifying" to "Extracting" as soon as
        // control crosses the thread boundary.
        status_for_blocking.set(FetchStatus::Phase {
            build_id: build_id_for_blocking.clone(),
            phase: FetchPhase::Extracting,
        });
        let _ = app_for_blocking.emit(
            "fetch:phase",
            serde_json::json!({
                "buildId": build_id_for_blocking,
                "phase": FetchPhase::Extracting.as_str(),
            }),
        );

        let mounted = verify_and_mount(&dest_for_blocking, &indexes_root_for_blocking, &sink)?;

        status_for_blocking.set(FetchStatus::Phase {
            build_id: build_id_for_blocking.clone(),
            phase: FetchPhase::Mounting,
        });
        let _ = app_for_blocking.emit(
            "fetch:phase",
            serde_json::json!({
                "buildId": build_id_for_blocking,
                "phase": FetchPhase::Mounting.as_str(),
            }),
        );

        // Bridge into the legacy `<indexes_root>/{tantivy,lance}/<slot>/`
        // layout the desktop search path still reads from. Keeps the
        // existing `SearchCatalog` working while wires
        // build-id-addressed search through the rest of the stack.
        wire_legacy_slot(&mounted, &indexes_root_for_blocking)?;

        Ok(mounted)
    })
    .await
    .context("verify/extract/mount task panicked")??;

    // Drop the downloaded archive on disk - everything we need now
    // lives under `<indexes_root>/<build_id>/`. Intentionally
    // best-effort: if removal fails (file locked, etc.), the fetch
    // still succeeded.
    let _ = tokio::fs::remove_file(&dest).await;
    let _ = tokio::fs::remove_file(&partial_path(&dest)).await;

    Ok(mounted)
}

/// Kick off a verify+mount job against an artifact already on disk -
/// no network, no resolver. Used by the "Mount from file" path in the
/// Index Catalog so developers (and HM admins testing internal builds)
/// can drop a freshly-built `.tar.zst` straight into the client without
/// running `python -m http.server` first.
///
/// Reuses the same status / events as `spawn_fetch` so the UI doesn't
/// need a parallel state machine - it just sees a fetch that skips the
/// Resolving + Downloading phases.
pub fn spawn_mount_local(
    rt: &tokio::runtime::Handle,
    app: AppHandle,
    status: SharedFetchStatus,
    indexes_root: PathBuf,
    tarball_path: PathBuf,
    on_mounted: impl FnOnce(MountedArtifact) + Send + 'static,
) {
    rt.spawn(async move {
        // We don't know the build_id until verify() has parsed the
        // manifest. Use the file stem as a placeholder for the early
        // event-tagging window so the UI has *some* identifier; the
        // real build_id replaces it once mount completes.
        let provisional_id = tarball_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local-artifact")
            .to_string();

        match run_mount_local(
            app.clone(),
            status.clone(),
            indexes_root,
            tarball_path,
            provisional_id.clone(),
        )
        .await
        {
            Ok(mounted) => {
                status.set(FetchStatus::Done {
                    build_id: mounted.build_id.clone(),
                });
                let _ = app.emit(
                    "fetch:done",
                    serde_json::json!({
                        "buildId": mounted.build_id,
                        "mountedAt": mounted.mounted_at.to_string_lossy(),
                    }),
                );
                on_mounted(mounted);
            }
            Err(err) => {
                tracing::error!(?err, "local-mount failed");
                let msg = format!("{err:#}");
                status.set(FetchStatus::Error {
                    build_id: provisional_id.clone(),
                    message: msg.clone(),
                });
                let _ = app.emit(
                    "fetch:error",
                    serde_json::json!({
                        "buildId": provisional_id,
                        "message": msg,
                    }),
                );
            }
        }
    });
}

async fn run_mount_local(
    app: AppHandle,
    status: SharedFetchStatus,
    indexes_root: PathBuf,
    tarball_path: PathBuf,
    provisional_id: String,
) -> Result<MountedArtifact> {
    if !tarball_path.is_file() {
        anyhow::bail!(
            "no artifact at {} - pick a .tar.zst that exists on disk",
            tarball_path.display()
        );
    }

    let emit_phase = {
        let app = app.clone();
        let id = provisional_id.clone();
        move |phase: FetchPhase| {
            let _ = app.emit(
                "fetch:phase",
                serde_json::json!({ "buildId": id, "phase": phase.as_str() }),
            );
        }
    };

    tokio::fs::create_dir_all(&indexes_root)
        .await
        .with_context(|| format!("creating {}", indexes_root.display()))?;

    // Verifying + Extracting + Mounting - same blocking task as the
    // HTTP path, just no Downloading phase up front.
    status.set(FetchStatus::Phase {
        build_id: provisional_id.clone(),
        phase: FetchPhase::Verifying,
    });
    emit_phase(FetchPhase::Verifying);

    let tarball_for_blocking = tarball_path.clone();
    let indexes_root_for_blocking = indexes_root.clone();
    let status_for_blocking = status.clone();
    let app_for_blocking = app.clone();
    let provisional_for_blocking = provisional_id.clone();

    let mounted = tokio::task::spawn_blocking(move || -> Result<MountedArtifact> {
        let sink = FetchExtractProgress {
            app: app_for_blocking.clone(),
            status: status_for_blocking.clone(),
            build_id: provisional_for_blocking.clone(),
        };

        status_for_blocking.set(FetchStatus::Phase {
            build_id: provisional_for_blocking.clone(),
            phase: FetchPhase::Extracting,
        });
        let _ = app_for_blocking.emit(
            "fetch:phase",
            serde_json::json!({
                "buildId": provisional_for_blocking,
                "phase": FetchPhase::Extracting.as_str(),
            }),
        );

        let mounted = verify_and_mount(&tarball_for_blocking, &indexes_root_for_blocking, &sink)?;

        status_for_blocking.set(FetchStatus::Phase {
            build_id: provisional_for_blocking.clone(),
            phase: FetchPhase::Mounting,
        });
        let _ = app_for_blocking.emit(
            "fetch:phase",
            serde_json::json!({
                "buildId": provisional_for_blocking,
                "phase": FetchPhase::Mounting.as_str(),
            }),
        );

        wire_legacy_slot(&mounted, &indexes_root_for_blocking)?;
        Ok(mounted)
    })
    .await
    .context("verify/extract/mount task panicked")??;

    Ok(mounted)
}

/// Progress sink piping tar-extract progress to FetchStatus + events.
struct FetchExtractProgress {
    app: AppHandle,
    status: SharedFetchStatus,
    build_id: String,
}

impl ExtractProgress for FetchExtractProgress {
    fn report(&self, current: usize, total: usize) {
        self.status.set(FetchStatus::Extracting {
            build_id: self.build_id.clone(),
            current,
            total,
        });
        let _ = self.app.emit(
            "fetch:progress",
            serde_json::json!({
                "buildId": self.build_id,
                "phase": FetchPhase::Extracting.as_str(),
                "current": current,
                "total": total,
            }),
        );
    }
}
