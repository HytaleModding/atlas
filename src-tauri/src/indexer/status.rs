//! Shared indexer status, mirroring `patcher::status` in shape.

use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexerPhase {
    Walking,
    Indexing,
    Committing,
}

impl IndexerPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            IndexerPhase::Walking => "walking",
            IndexerPhase::Indexing => "indexing",
            IndexerPhase::Committing => "committing",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum IndexerStatus {
    Idle,
    Phase {
        slot: &'static str,
        phase: IndexerPhase,
    },
    Progress {
        slot: &'static str,
        phase: IndexerPhase,
        current: usize,
        total: usize,
        /// Chunks embedded + written so far. The real bottleneck is
        /// embedding, so surfacing this lets the UI show progress even
        /// when a single file contributes many chunks and `current`
        /// hasn't advanced yet.
        #[serde(default)]
        chunks: u64,
    },
    Done {
        slot: &'static str,
        docs: u64,
    },
    Error {
        slot: &'static str,
        message: String,
    },
}

impl Default for IndexerStatus {
    fn default() -> Self {
        IndexerStatus::Idle
    }
}

#[derive(Clone, Default)]
pub struct SharedIndexerStatus(Arc<Mutex<IndexerStatus>>);

impl SharedIndexerStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, status: IndexerStatus) {
        let mut guard = self.0.lock().expect("indexer status poisoned");
        *guard = status;
    }

    pub fn snapshot(&self) -> IndexerStatus {
        self.0.lock().expect("indexer status poisoned").clone()
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.snapshot(),
            IndexerStatus::Phase { .. } | IndexerStatus::Progress { .. }
        )
    }
}
