//! Cache + integrity-check the pinned Vineflower decompiler JAR.
//!
//! We pin to Vineflower 1.11.2 because that's what Horizon's patcher uses
//! and we want bit-identical decompilation output. The SHA256 below was
//! computed locally against the file Horizon ships and verified against
//! the official GitHub release.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

use super::status::{PatcherPhase, PatcherStatus, SharedStatus};

pub const VINEFLOWER_VERSION: &str = "1.11.2";
pub const VINEFLOWER_SHA256: &str =
    "e1e2415e7f78b34960402c4beddfc88e033d7842a23ecd132a8ec2eadd54f6bf";
pub const VINEFLOWER_URL: &str =
    "https://github.com/Vineflower/vineflower/releases/download/1.11.2/vineflower-1.11.2.jar";

fn cache_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "horizon", "Atlas")
        .ok_or_else(|| anyhow!("no ProjectDirs for Atlas"))?;
    let tools = dirs.data_dir().join("tools");
    Ok(tools.join(format!("vineflower-{VINEFLOWER_VERSION}.jar")))
}

/// Ensure a known-good Vineflower JAR exists on disk and return its path.
/// If the cached copy is missing or the hash doesn't match, re-downloads.
pub async fn ensure_vineflower(app: &AppHandle, status: &SharedStatus) -> Result<PathBuf> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    if path.exists() {
        match verify_sha256(&path).await {
            Ok(true) => {
                tracing::info!("vineflower cached at {}", path.display());
                return Ok(path);
            }
            Ok(false) => {
                tracing::warn!(
                    "vineflower at {} failed hash check, re-downloading",
                    path.display()
                );
                let _ = tokio::fs::remove_file(&path).await;
            }
            Err(err) => {
                tracing::warn!(?err, "vineflower hash check errored, re-downloading");
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    status.set(PatcherStatus::Phase {
        phase: PatcherPhase::DownloadingVineflower,
    });
    let _ = app.emit(
        "decompile:phase",
        serde_json::json!({ "phase": PatcherPhase::DownloadingVineflower.as_str() }),
    );

    download(&path, app, status)
        .await
        .with_context(|| format!("downloading Vineflower to {}", path.display()))?;

    if !verify_sha256(&path).await? {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(anyhow!(
            "downloaded Vineflower failed SHA256 integrity check; expected {VINEFLOWER_SHA256}"
        ));
    }

    tracing::info!("vineflower downloaded and verified at {}", path.display());
    Ok(path)
}

/// CLI variant of [`ensure_vineflower`] used by `atlas-build decompile`.
/// Caller passes the directory where the JAR should live (typically
/// `<cache_root>/tools/`). No Tauri AppHandle, no event emission - just
/// tracing logs. Hash check + re-download semantics match the desktop
/// flow.
pub async fn ensure_vineflower_at(cache_dir: &Path) -> Result<PathBuf> {
    tokio::fs::create_dir_all(cache_dir)
        .await
        .with_context(|| format!("creating {}", cache_dir.display()))?;
    let path = cache_dir.join(format!("vineflower-{VINEFLOWER_VERSION}.jar"));

    if path.exists() {
        match verify_sha256(&path).await {
            Ok(true) => {
                tracing::info!("vineflower cached at {}", path.display());
                return Ok(path);
            }
            Ok(false) => {
                tracing::warn!(
                    "vineflower at {} failed hash check, re-downloading",
                    path.display()
                );
                let _ = tokio::fs::remove_file(&path).await;
            }
            Err(err) => {
                tracing::warn!(?err, "vineflower hash check errored, re-downloading");
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    tracing::info!("downloading vineflower → {}", path.display());
    download_quiet(&path)
        .await
        .with_context(|| format!("downloading Vineflower to {}", path.display()))?;

    if !verify_sha256(&path).await? {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(anyhow!(
            "downloaded Vineflower failed SHA256 integrity check; expected {VINEFLOWER_SHA256}"
        ));
    }

    tracing::info!("vineflower downloaded and verified at {}", path.display());
    Ok(path)
}

async fn download_quiet(dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let bytes = client
        .get(VINEFLOWER_URL)
        .send()
        .await
        .context("GET vineflower release")?
        .error_for_status()
        .context("vineflower release returned non-2xx")?
        .bytes()
        .await
        .context("downloading vineflower body")?;
    tokio::fs::write(dest, &bytes)
        .await
        .with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

async fn download(dest: &Path, app: &AppHandle, status: &SharedStatus) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client
        .get(VINEFLOWER_URL)
        .send()
        .await
        .context("GET vineflower release")?
        .error_for_status()
        .context("vineflower release returned non-2xx")?;

    let total = resp.content_length();
    let mut received: u64 = 0;
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest).await?;

    let mut last_emit: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading vineflower chunk")?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;

        status.set(PatcherStatus::Downloading { received, total });

        // Throttle progress events to ~every 128 KiB.
        if received - last_emit > 128 * 1024 {
            last_emit = received;
            let _ = app.emit(
                "decompile:progress",
                serde_json::json!({
                    "phase": PatcherPhase::DownloadingVineflower.as_str(),
                    "received": received,
                    "total": total,
                }),
            );
        }
    }
    file.flush().await?;
    Ok(())
}

async fn verify_sha256(path: &Path) -> Result<bool> {
    let bytes = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = format!("{:x}", hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(VINEFLOWER_SHA256))
}
