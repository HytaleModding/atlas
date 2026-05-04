//! Shared fetch status + progress for the central-artifact fetch flow
//!. Mirrors `patcher::status` / `indexer::status` so the UI
//! can render download/extraction progress using the same idiom it
//! already uses for local decompile + local indexing.
//!
//! Every event the UI sees carries `build_id` (rather than `slot`)
//! because the fetcher is keyed by which central artifact is being
//! pulled - a single client can be fetching a pre-release while the
//! release artifact is already mounted.

use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FetchPhase {
    /// Looking up the best artifact for the client's Hytale version.
    Resolving,
    /// Streaming `.tar.zst` bytes from the hosted artifact URL.
    Downloading,
    /// Running `fetcher::artifact::verify` + Ed25519 signature check.
    Verifying,
    /// Decompressing + extracting to `<indexes>/.tmp/<build_id>/`.
    Extracting,
    /// Atomic rename + `.ok` marker + `SearchCatalog` mount.
    Mounting,
}

impl FetchPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchPhase::Resolving => "resolving",
            FetchPhase::Downloading => "downloading",
            FetchPhase::Verifying => "verifying",
            FetchPhase::Extracting => "extracting",
            FetchPhase::Mounting => "mounting",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FetchStatus {
    Idle,
    Phase {
        build_id: String,
        phase: FetchPhase,
    },
    Downloading {
        build_id: String,
        received: u64,
        total: Option<u64>,
    },
    Extracting {
        build_id: String,
        current: usize,
        total: usize,
    },
    Done {
        build_id: String,
    },
    Error {
        build_id: String,
        message: String,
    },
}

impl Default for FetchStatus {
    fn default() -> Self {
        FetchStatus::Idle
    }
}

#[derive(Clone, Default)]
pub struct SharedFetchStatus(Arc<Mutex<FetchStatus>>);

impl SharedFetchStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, status: FetchStatus) {
        let mut guard = self.0.lock().expect("fetch status poisoned");
        *guard = status;
    }

    pub fn snapshot(&self) -> FetchStatus {
        self.0.lock().expect("fetch status poisoned").clone()
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.snapshot(),
            FetchStatus::Phase { .. }
                | FetchStatus::Downloading { .. }
                | FetchStatus::Extracting { .. }
        )
    }
}
