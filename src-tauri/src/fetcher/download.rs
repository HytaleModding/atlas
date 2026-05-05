//! Streaming HTTP download with Range-based resume.
//!
//! The fetcher writes to `<indexes>/.tmp/<build_id>.tar.zst.partial`. If
//! the partial exists when a fetch starts, we ask the server for
//! `Range: bytes=<len>-` and append bytes to the existing file. Servers
//! that don't honor Range (respond with 200 instead of 206) cause us to
//! truncate and restart from 0 - the download completes correctly, the
//! resume was just wasted. Verification still catches any real
//! corruption.
//!
//! We deliberately do NOT hash the partial mid-flight. The tarball's
//! `SHA256SUMS` + manifest signature are the source of truth; if the
//! resumed bytes are off-by-one, `artifact::verify` will reject the
//! assembled file. This keeps download fast and makes the verify stage
//! the sole trust boundary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_LENGTH, RANGE};
use reqwest::{Client, StatusCode};
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;

/// Describes what the caller wants pulled. Kept small so it's easy to
/// build from either the resolver (hosted GH Releases) or a local
/// `file://` URL used by tests.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub build_id: String,
    pub url: String,
    /// Final destination for the completed `.tar.zst`. The function
    /// appends to `<dest>.partial` and renames on success.
    pub dest: PathBuf,
}

/// Progress handle passed to callers so they can emit events /
/// poll-friendly status without the downloader knowing about Tauri.
#[derive(Clone)]
pub struct DownloadProgress {
    received: Arc<AtomicU64>,
    total: Arc<AtomicU64>, // 0 sentinel = unknown
}

impl DownloadProgress {
    pub fn new() -> Self {
        Self {
            received: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }
    pub fn total(&self) -> Option<u64> {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            None
        } else {
            Some(t)
        }
    }
}

impl Default for DownloadProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming download with Range resume. `on_progress` is called after
/// each chunk is flushed; callers typically throttle event emission
/// inside the callback to avoid flooding Tauri IPC.
pub async fn download<F>(
    client: &Client,
    req: &DownloadRequest,
    progress: DownloadProgress,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>),
{
    if let Some(parent) = req.dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating download parent dir {}", parent.display()))?;
    }
    let partial = partial_path(&req.dest);

    let existing_len = match tokio::fs::metadata(&partial).await {
        Ok(m) if m.is_file() => m.len(),
        _ => 0,
    };

    // HEAD-free approach: ask for `Range: bytes=<existing_len>-`. The
    // server either honours it (206 Partial Content, we append) or
    // ignores it (200 OK, we truncate and start over).
    let mut builder = client.get(&req.url);
    if existing_len > 0 {
        builder = builder.header(RANGE, format!("bytes={}-", existing_len));
    }
    let resp = builder
        .send()
        .await
        .with_context(|| format!("GET {}", req.url))?;

    let status = resp.status();
    if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
        bail!("artifact fetch returned {} for {}", status, req.url);
    }

    // Open the partial file in the correct mode based on whether the
    // server honored our Range request.
    let mut file = if status == StatusCode::PARTIAL_CONTENT && existing_len > 0 {
        // Append to existing bytes.
        progress.received.store(existing_len, Ordering::Relaxed);
        OpenOptions::new()
            .append(true)
            .open(&partial)
            .await
            .with_context(|| format!("opening partial {} for append", partial.display()))?
    } else {
        // Server ignored Range (or nothing to resume) - start over.
        progress.received.store(0, Ordering::Relaxed);
        File::create(&partial)
            .await
            .with_context(|| format!("creating {}", partial.display()))?
    };

    // Total = Content-Length + what we already had on disk (for 206).
    let reported_total = resp
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let total = match (status == StatusCode::PARTIAL_CONTENT, reported_total) {
        (true, Some(n)) => Some(n + existing_len),
        (_, Some(n)) => Some(n),
        (_, None) => None,
    };
    progress.total.store(total.unwrap_or(0), Ordering::Relaxed);
    on_progress(progress.received(), total);

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading response chunk")?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing to {}", partial.display()))?;
        let new_received = progress
            .received
            .fetch_add(chunk.len() as u64, Ordering::Relaxed)
            + chunk.len() as u64;
        on_progress(new_received, total);
    }
    file.flush().await.context("flushing partial download")?;
    drop(file);

    // Success → rename into final place. Anyone observing `<dest>`
    // without `.partial` can assume the download completed, though
    // they still need to run `artifact::verify` before trusting it.
    tokio::fs::rename(&partial, &req.dest)
        .await
        .with_context(|| format!("renaming {} → {}", partial.display(), req.dest.display()))?;
    Ok(())
}

/// Returns `<dest>.partial` - where the in-flight download lives.
pub fn partial_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_os_string();
    p.push(".partial");
    PathBuf::from(p)
}
