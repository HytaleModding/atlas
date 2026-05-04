//! Shared patcher status + progress.
//!
//! `SharedStatus` is cheap-to-clone; it wraps an `Arc<Mutex<PatcherStatus>>`
//! so the UI can poll via the `patcher_status` command while the pipeline
//! mutates state from a background task.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatcherPhase {
    EnsuringVineflower,
    DownloadingVineflower,
    DetectingJava,
    Extracting,
    Decompiling,
}

impl PatcherPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            PatcherPhase::EnsuringVineflower => "ensuring-vineflower",
            PatcherPhase::DownloadingVineflower => "downloading-vineflower",
            PatcherPhase::DetectingJava => "detecting-java",
            PatcherPhase::Extracting => "extracting",
            PatcherPhase::Decompiling => "decompiling",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PatcherStatus {
    Idle,
    Phase {
        phase: PatcherPhase,
    },
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Extracting {
        current: usize,
        total: usize,
    },
    Done {
        output_dir: PathBuf,
    },
    Error {
        message: String,
    },
}

impl Default for PatcherStatus {
    fn default() -> Self {
        PatcherStatus::Idle
    }
}

#[derive(Clone, Default)]
pub struct SharedStatus(Arc<Mutex<PatcherStatus>>);

impl SharedStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, status: PatcherStatus) {
        let mut guard = self.0.lock().expect("patcher status poisoned");
        *guard = status;
    }

    pub fn snapshot(&self) -> PatcherStatus {
        self.0.lock().expect("patcher status poisoned").clone()
    }

    /// True if the patcher is currently doing work and shouldn't accept a
    /// concurrent start request.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.snapshot(),
            PatcherStatus::Phase { .. }
                | PatcherStatus::Downloading { .. }
                | PatcherStatus::Extracting { .. }
        )
    }
}
