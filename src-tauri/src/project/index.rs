//! Project-mode indexing: thin wrapper around [`crate::indexer::run`]
//! that points the indexer at a user's mod source folder, tags every
//! chunk with `source_type = "project_source"`, and writes the
//! resulting Tantivy + Lance store under `<data_dir>/projects/<id>/`.
//!
//! The actual chunker, embedder, walker, schema, and progress-event
//! plumbing all come from the existing indexer - this module just
//! supplies the per-project IO paths and a project-scoped progress
//! sink so the frontend can subscribe to events tagged with the
//! project id (rather than a Hytale `slot`).
//!
//! What this skips on purpose:
//!   - Summarizer (LLM cost shouldn't apply to user code).
//!   - HM docs, Hypixel Javadocs (Hytale-specific corpora).
//!   - Embedding cache (one-shot, the disk overhead doesn't pay off
//!     for project-sized corpora).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::config::Slot;
use crate::embedder::Embedder;
use crate::indexer::{self, schema::SourceType, IndexEvent, ProgressSink};

use super::ProjectId;

/// Run the indexer against a registered project. `embedder` is shared
/// across runs (Tauri-managed); `sink` is a [`ProgressSink`] that emits
/// `project:phase` / `project:progress` / `project:done` events tagged
/// with `project_id`.
///
/// On success, the caller is expected to call
/// `ProjectRegistry::mark_indexed(id)` to persist the timestamp.
pub async fn run_project_index(
    embedder: Arc<dyn Embedder>,
    project_id: ProjectId,
    source_path: PathBuf,
    index_dir: PathBuf,
    lance_dir: PathBuf,
    sink: Arc<dyn ProgressSink>,
) -> Result<()> {
    if !source_path.is_dir() {
        return Err(anyhow!(
            "project source path is not a directory: {}",
            source_path.display()
        ));
    }
    // `Slot` is baked into chunk docs as a tag. For project mode it's
    // semantically meaningless - chunks won't be matched against a
    // slot filter at search time because the index lives at a
    // project-specific dir, not under the Hytale slot tree. Pin to
    // `Release` rather than introducing a `Slot::Project` variant
    // (which would leak into the wider config / fetcher state machine).
    indexer::run(
        embedder,
        Slot::Release,
        source_path,
        index_dir,
        lance_dir,
        sink,
        None, // no summarizer
        None, // no HM docs
        None, // no Hypixel docs
        None, // no embed cache
        Some(SourceType::ProjectSource),
    )
    .await
    .with_context(|| format!("indexing project {project_id}"))
}

/// Tauri-flavoured progress sink for project indexing. Mirror of
/// `commands::TauriSink` but emits `project:*` events tagged with the
/// project id rather than `index:*` events tagged with the slot. Lives
/// here (not in `commands.rs`) so a project-mode caller can stand it
/// up without dragging the rest of the slot-shaped status surface in.
pub struct ProjectSink {
    pub app: tauri::AppHandle,
    pub project_id: ProjectId,
}

impl ProgressSink for ProjectSink {
    fn emit(&self, event: IndexEvent) {
        use tauri::Emitter;
        let pid = self.project_id.as_str();
        match event {
            IndexEvent::Phase(phase) => {
                let _ = self.app.emit(
                    "project:phase",
                    serde_json::json!({
                        "project_id": pid,
                        "phase": phase.as_str(),
                    }),
                );
            }
            IndexEvent::Progress {
                current,
                total,
                chunks,
            } => {
                let _ = self.app.emit(
                    "project:progress",
                    serde_json::json!({
                        "project_id": pid,
                        "current": current,
                        "total": total,
                        "chunks": chunks,
                    }),
                );
            }
            IndexEvent::Done { docs } => {
                let _ = self.app.emit(
                    "project:done",
                    serde_json::json!({
                        "project_id": pid,
                        "docs": docs,
                    }),
                );
            }
        }
    }
}
