//! Per-user mod project registry.
//!
//! Atlas's "project mode" lets a modder point Atlas at the source folder
//! of a plugin they're working on. Atlas then runs the same indexer
//! (`crate::indexer::run`) against the project's `.java` files and
//! mounts the resulting index alongside the fetched Hytale indexes.
//!
//! This module owns the *registry* - the user-level list of "I told
//! Atlas about these folders" pointers - persisted as a small JSON file
//! at `<data_dir>/projects.json`. Indexing itself lives in
//! [`index`]; this module only tracks identity, paths, and last-indexed
//! timestamps.
//!
//! # Identity
//!
//! `ProjectId` is `sha256(canonical_source_path)[..12]` (hex). Derived,
//! not random, so:
//!   - Re-registering the same folder yields the same id - the existing
//!     index dir on disk is reused without an orphaned ghost.
//!   - The id can be reconstructed from the source path alone, useful
//!     for IDE-side integrations later.
//!
//! Trade-off: renaming/moving a project folder produces a new id. That's
//! fine for V1 - the user can re-register the new path and remove the
//! stale entry. A "rename" affordance is a follow-up.
//!
//! # On-disk layout
//!
//! ```text
//! <data_dir>/projects.json                  - this registry
//! <data_dir>/projects/<id>/tantivy/         - per-project Tantivy index
//! <data_dir>/projects/<id>/lance/           - per-project Lance store
//! <data_dir>/projects/<id>/tantivy/symbols.sqlite - symbols sidecar
//! ```

pub mod index;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::indexer::IndexId;

/// Stable identifier for a registered project. Newtype over `String` so
/// callers can't mix it up with a random string. Derived from the
/// canonicalized source path (see module docs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(String);

impl ProjectId {
    /// Hex-encoded sha256 prefix of the canonicalized path. 12 chars =
    /// 48 bits, more than enough collision resistance for a per-user
    /// list that will almost never exceed double digits.
    pub fn from_path(path: &Path) -> Result<Self> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("canonicalizing project path {}", path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(12);
        for b in digest.iter().take(6) {
            hex.push_str(&format!("{b:02x}"));
        }
        Ok(Self(hex))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ProjectId {
    /// Trust caller-supplied ids (typically forwarded from the frontend
    /// after a previous `project_register`). Validation happens
    /// implicitly when the registry looks the id up - unknown ids
    /// surface as a 404-shaped error from the consuming command.
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// One row in the registry. Keeps just enough state for the catalog UI
/// to render rows and decide whether a re-index is overdue. Anything
/// schema-shaped (chunks, embeddings) lives in the actual index dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredProject {
    pub id: ProjectId,
    /// User-friendly name. Defaults to the folder's basename when the
    /// caller doesn't supply one.
    pub name: String,
    /// Canonicalized absolute path to the source folder. Stored so the
    /// UI can show "where is this on disk" without recomputing.
    pub source_path: PathBuf,
    /// ISO-8601 of when the user added the project.
    pub created_at: String,
    /// ISO-8601 of the last successful index run, or `None` if the
    /// folder was registered but never indexed yet.
    pub last_indexed_at: Option<String>,
}

/// Wire shape of `<data_dir>/projects.json`. Top-level object so future
/// fields (e.g. `schema_version`, per-project flags) can be added
/// without breaking older clients.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    projects: Vec<RegisteredProject>,
}

/// Tauri-managed registry handle. Loaded once at app boot, mutations
/// re-serialize the JSON file in place. Not behind a Mutex here because
/// the [`crate::commands`] commands wrap it in `parking_lot_like::Mutex`
/// at the state-management layer - keep this struct itself plain so it
/// stays cheap to clone the in-memory state for read-only views.
#[derive(Debug, Default)]
pub struct ProjectRegistry {
    projects: Vec<RegisteredProject>,
    data_dir: PathBuf,
}

impl ProjectRegistry {
    /// Open (or create) the registry at `<data_dir>/projects.json`.
    /// Missing file is fine - returns an empty registry that will write
    /// itself out on first mutation.
    pub fn load(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let path = registry_path(data_dir);
        if !path.exists() {
            return Ok(Self {
                projects: Vec::new(),
                data_dir: data_dir.to_path_buf(),
            });
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: RegistryFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            projects: file.projects,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// Snapshot of currently-registered projects. Cheap (small Vec).
    pub fn list(&self) -> Vec<RegisteredProject> {
        self.projects.clone()
    }

    pub fn get(&self, id: &ProjectId) -> Option<&RegisteredProject> {
        self.projects.iter().find(|p| p.id == *id)
    }

    /// Add a project. If `name` is `None`, the folder basename is used.
    /// Re-registering the same path is idempotent: returns the existing
    /// entry's id and leaves it in place. The source path is canonicalized
    /// before hashing so `./foo` and `/abs/path/foo` collapse to one entry.
    pub fn register(
        &mut self,
        source_path: &Path,
        name: Option<String>,
    ) -> Result<ProjectId> {
        if !source_path.is_dir() {
            return Err(anyhow!(
                "project path is not a directory: {}",
                source_path.display()
            ));
        }
        let id = ProjectId::from_path(source_path)?;
        if self.projects.iter().any(|p| p.id == id) {
            // Idempotent: already registered.
            return Ok(id);
        }
        let canonical = source_path
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", source_path.display()))?;
        let resolved_name = name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled project".to_string())
            });
        self.projects.push(RegisteredProject {
            id: id.clone(),
            name: resolved_name,
            source_path: canonical,
            created_at: now_iso(),
            last_indexed_at: None,
        });
        self.persist()?;
        Ok(id)
    }

    /// Drop the project from the registry. Does NOT delete the index
    /// directory on disk - that's [`Self::remove_index`]'s job (callers
    /// usually do both, but unregister-without-removing is supported so
    /// the user can preserve a stale index across registry edits).
    pub fn unregister(&mut self, id: &ProjectId) -> Result<()> {
        let before = self.projects.len();
        self.projects.retain(|p| &p.id != id);
        if self.projects.len() == before {
            return Err(anyhow!("no project with id {id}"));
        }
        self.persist()?;
        Ok(())
    }

    /// Update the last-indexed timestamp after a successful index run.
    pub fn mark_indexed(&mut self, id: &ProjectId) -> Result<()> {
        let p = self
            .projects
            .iter_mut()
            .find(|p| p.id == *id)
            .ok_or_else(|| anyhow!("no project with id {id}"))?;
        p.last_indexed_at = Some(now_iso());
        self.persist()?;
        Ok(())
    }

    /// Remove the on-disk index directory (if any) for a project. Called
    /// by both `project_remove_index` (keep registry entry, drop the
    /// index) and `project_unregister` (cascade cleanup).
    pub fn remove_index_dir(&self, id: &ProjectId) -> Result<()> {
        let dir = self.project_root(id);
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .with_context(|| format!("removing project index dir {}", dir.display()))?;
        }
        Ok(())
    }

    /// `<data_dir>/projects/<id>/`. Where the per-project index lives.
    pub fn project_root(&self, id: &ProjectId) -> PathBuf {
        self.data_dir.join("projects").join(id.as_str())
    }

    pub fn project_index_dir(&self, id: &ProjectId) -> PathBuf {
        self.project_root(id).join("tantivy")
    }

    pub fn project_lance_dir(&self, id: &ProjectId) -> PathBuf {
        self.project_root(id).join("lance")
    }

    /// `IndexId` for the SearchCatalog when this project gets opened
    /// for cross-corpus search. Body shape `user-project-<id>` is
    /// documented on `IndexId` itself.
    pub fn index_id(&self, id: &ProjectId) -> IndexId {
        IndexId::new(format!("user-project-{}", id.as_str()))
    }

    fn persist(&self) -> Result<()> {
        let path = registry_path(&self.data_dir);
        let file = RegistryFile {
            projects: self.projects.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)
            .context("serializing project registry")?;
        // tmp + rename so a partial write can't leave the registry empty.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &json)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

fn registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join("projects.json")
}

fn now_iso() -> String {
    crate::indexer::metadata::format_iso8601(std::time::SystemTime::now())
}

/// Tauri-managed wrapper around `ProjectRegistry`. Plain
/// `std::sync::Mutex` is sufficient: registry mutations are rare
/// (user-initiated clicks) and contention is effectively zero.
pub struct SharedProjectRegistry(std::sync::Mutex<ProjectRegistry>);

impl SharedProjectRegistry {
    pub fn new(registry: ProjectRegistry) -> Self {
        Self(std::sync::Mutex::new(registry))
    }

    /// Run a closure with mutable registry access. Panics on a poisoned
    /// mutex (mirrors the pattern used by `indexer::SearchCatalog`'s
    /// internal mutex shim).
    pub fn with<R>(&self, f: impl FnOnce(&mut ProjectRegistry) -> R) -> R {
        let mut guard = self.0.lock().expect("project registry poisoned");
        f(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_stable_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = ProjectId::from_path(dir.path()).unwrap();
        let id2 = ProjectId::from_path(dir.path()).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(id1.as_str().len(), 12);
    }

    #[test]
    fn register_then_list_then_unregister() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let mut reg = ProjectRegistry::load(data.path()).unwrap();
        assert!(reg.list().is_empty());

        let id = reg.register(project.path(), Some("My Plugin".into())).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].name, "My Plugin");

        // Idempotent re-register returns same id.
        let id_again = reg.register(project.path(), None).unwrap();
        assert_eq!(id, id_again);
        assert_eq!(reg.list().len(), 1);

        // Persistence round-trip.
        let reloaded = ProjectRegistry::load(data.path()).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, id);

        reg.unregister(&id).unwrap();
        assert!(reg.list().is_empty());
    }

    #[test]
    fn unregister_unknown_id_errors() {
        let data = tempfile::tempdir().unwrap();
        let mut reg = ProjectRegistry::load(data.path()).unwrap();
        let id = ProjectId("doesnotexist".to_string());
        assert!(reg.unregister(&id).is_err());
    }
}
