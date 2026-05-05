//! Lightweight, self-contained search backend for HM docs guides.
//!
//! Decoupled from the main hybrid (Tantivy + Lance) source index so guides
//! refresh cheaply without dragging the user through a multi-hour
//! decompile + embed cycle. Guides update on a HM-controlled cadence
//! (often hours or days); the source section updates on a Hytale-build
//! cadence (months). Mixing them into one rebuild was the wrong shape.
//!
//!   * Walks `<atlas-cache>/hm-docs/site/` for `.md` / `.mdx` files via
//!     the existing [`crate::indexer::hm_docs::walk_docs`].
//!   * Indexes each file as one Tantivy doc (title + body, BM25 only -
//!     no embeddings, no Lance, no chunker).
//!   * Persists a tiny JSON manifest (file count + max mtime) so an
//!     unchanged repo skips reindexing entirely.
//!   * Hot-swappable opened reader: a refresh can replace the live
//!     index without holding up concurrent searches.
//!
//! Index location: `<data_dir>/guides/index/`.
//! Manifest:       `<data_dir>/guides/manifest.json`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, Value, FAST, STORED,
    STRING,
};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument};

use crate::indexer::analyzer::{CodeTokenizer, CODE_TOKENIZER};
use crate::indexer::hm_docs::{walk_docs, DocFile};
use crate::indexer::SearchHit;

const WRITER_HEAP_BYTES: usize = 32 * 1024 * 1024;

/// Public, cheaply-cloneable handle held by Tauri managed state.
#[derive(Clone)]
pub struct GuidesIndex {
    inner: Arc<GuidesInner>,
}

struct GuidesInner {
    data_dir: PathBuf,
    repo_dir: PathBuf,
    /// `None` means "no index yet" - search returns empty until a sync
    /// runs successfully.
    opened: RwLock<Option<Opened>>,
}

struct Opened {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

#[derive(Clone, Copy)]
struct Fields {
    path: tantivy::schema::Field,
    title: tantivy::schema::Field,
    body: tantivy::schema::Field,
    line_count: tantivy::schema::Field,
    /// Comma-joined author names from the doc frontmatter; STORED only,
    /// not searchable. Empty string when the doc has no authors block.
    authors: tantivy::schema::Field,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    /// Number of `.md`/`.mdx` files indexed last run.
    file_count: u64,
    /// Maximum mtime, expressed as seconds since Unix epoch. Combined
    /// with `file_count` this catches every kind of change: additions,
    /// deletions, edits.
    max_mtime_secs: u64,
    /// Schema-format version, bumped if we ever change [`build_schema`].
    schema_version: u32,
}

/// Bumped to 2 when authors were added to the schema; old indexes get
/// rebuilt automatically on first launch under the new version.
const SCHEMA_VERSION: u32 = 2;

impl GuidesIndex {
    /// Build a handle without touching disk yet. Call
    /// [`sync_and_refresh`](Self::sync_and_refresh) to populate / refresh
    /// the index.
    pub fn new(data_dir: PathBuf, repo_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(GuidesInner {
                data_dir,
                repo_dir,
                opened: RwLock::new(None),
            }),
        }
    }

    fn index_dir(&self) -> PathBuf {
        self.inner.data_dir.join("guides").join("index")
    }

    fn manifest_path(&self) -> PathBuf {
        self.inner.data_dir.join("guides").join("manifest.json")
    }

    /// Walk the repo; if the on-disk fingerprint differs from the stored
    /// manifest, blow away + rebuild the Tantivy index and hot-swap the
    /// reader. Idempotent: repeated calls with no repo changes are
    /// cheap (one walkdir pass + one stat each).
    ///
    /// Errors short-circuit the rebuild but never poison the existing
    /// reader - the previous index keeps serving queries.
    pub fn sync_and_refresh(&self) -> Result<()> {
        if !self.inner.repo_dir.is_dir() {
            tracing::info!(
                repo = %self.inner.repo_dir.display(),
                "guides repo missing; skipping sync"
            );
            // If we have an opened index from a prior run with a
            // now-missing repo, leave it alone - the user can still
            // search the last-known guides until the repo reappears.
            return Ok(());
        }

        let docs = walk_docs(&self.inner.repo_dir)
            .with_context(|| format!("walking guides repo at {}", self.inner.repo_dir.display()))?;
        let fingerprint = compute_fingerprint(&self.inner.repo_dir, &docs);

        let stored = read_manifest(&self.manifest_path()).ok();
        let already_open = self
            .inner
            .opened
            .read()
            .expect("guides index lock poisoned")
            .is_some();
        if let Some(prev) = stored {
            if prev.schema_version == SCHEMA_VERSION
                && prev.file_count == fingerprint.file_count
                && prev.max_mtime_secs == fingerprint.max_mtime_secs
                && already_open
            {
                tracing::debug!(
                    file_count = fingerprint.file_count,
                    "guides index up to date"
                );
                return Ok(());
            }
            // Stored manifest matches but no opened reader yet (cold start) -
            // skip the rebuild, just open.
            if prev.schema_version == SCHEMA_VERSION
                && prev.file_count == fingerprint.file_count
                && prev.max_mtime_secs == fingerprint.max_mtime_secs
            {
                let opened = open_existing(&self.index_dir())?;
                *self
                    .inner
                    .opened
                    .write()
                    .expect("guides index lock poisoned") = Some(opened);
                return Ok(());
            }
        }

        // Need a rebuild. Build into a fresh tmp dir, then atomically
        // swap. Avoids a half-built index being visible if the writer
        // crashes mid-build.
        let final_dir = self.index_dir();
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating guides parent {}", parent.display()))?;
        }
        let tmp_dir = final_dir.with_extension("tmp");
        if tmp_dir.exists() {
            std::fs::remove_dir_all(&tmp_dir)
                .with_context(|| format!("clearing tmp guides dir {}", tmp_dir.display()))?;
        }
        std::fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("creating tmp guides dir {}", tmp_dir.display()))?;

        let started = std::time::Instant::now();
        build_index(&tmp_dir, &docs)?;

        if final_dir.exists() {
            // Drop the old reader before deleting its files (Windows
            // refuses to remove a directory whose mmap is still mapped).
            *self
                .inner
                .opened
                .write()
                .expect("guides index lock poisoned") = None;
            std::fs::remove_dir_all(&final_dir)
                .with_context(|| format!("removing old guides index {}", final_dir.display()))?;
        }
        std::fs::rename(&tmp_dir, &final_dir)
            .with_context(|| format!("rename {} -> {}", tmp_dir.display(), final_dir.display()))?;

        let opened = open_existing(&final_dir)?;
        *self
            .inner
            .opened
            .write()
            .expect("guides index lock poisoned") = Some(opened);

        write_manifest(
            &self.manifest_path(),
            &Manifest {
                file_count: fingerprint.file_count,
                max_mtime_secs: fingerprint.max_mtime_secs,
                schema_version: SCHEMA_VERSION,
            },
        )?;

        tracing::info!(
            file_count = fingerprint.file_count,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "guides index rebuilt"
        );
        Ok(())
    }

    /// True when there's a populated index ready to serve queries.
    pub fn is_ready(&self) -> bool {
        self.inner
            .opened
            .read()
            .expect("guides index lock poisoned")
            .is_some()
    }

    /// Search the guides index. `slot_label` is stamped onto every
    /// returned hit's `slot` field - guides aren't tied to a Hytale
    /// build, but the frontend's hit list expects a slot string so we
    /// echo back whatever the search request used.
    ///
    /// Returns an empty vec when the index hasn't been populated yet
    /// (cold start, missing repo, build error).
    pub fn search(&self, query: &str, limit: usize, slot_label: &str) -> Result<Vec<SearchHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let guard = self
            .inner
            .opened
            .read()
            .expect("guides index lock poisoned");
        let Some(opened) = guard.as_ref() else {
            return Ok(Vec::new());
        };

        let mut parser =
            QueryParser::for_index(&opened.index, vec![opened.fields.title, opened.fields.body]);
        parser.set_field_boost(opened.fields.title, 3.0);

        let parsed = match parser.parse_query(trimmed) {
            Ok(q) => q,
            Err(err) => {
                tracing::debug!(?err, query = %trimmed, "guides query parse failed");
                return Ok(Vec::new());
            }
        };

        let searcher = opened.reader.searcher();
        let top = searcher.search(&parsed, &TopDocs::with_limit(limit))?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            hits.push(build_hit(&opened.fields, &doc, score, slot_label));
        }
        Ok(hits)
    }
}

// -----------------------------------------------------------------------
// Schema + build
// -----------------------------------------------------------------------

fn build_schema() -> (Schema, Fields) {
    let mut builder: SchemaBuilder = Schema::builder();

    let code_indexing = TextFieldIndexing::default()
        .set_tokenizer(CODE_TOKENIZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let code_stored: TextOptions = TextOptions::default()
        .set_indexing_options(code_indexing.clone())
        .set_stored();
    let code_only: TextOptions = TextOptions::default().set_indexing_options(code_indexing);

    let path = builder.add_text_field("path", STRING | STORED);
    let title = builder.add_text_field("title", code_stored);
    let body = builder.add_text_field("body", code_only);
    let line_count = builder.add_u64_field("line_count", STORED | FAST);
    let authors = builder.add_text_field("authors", STRING | STORED);

    let schema = builder.build();
    (
        schema,
        Fields {
            path,
            title,
            body,
            line_count,
            authors,
        },
    )
}

fn register_tokenizers(index: &Index) {
    index.tokenizers().register(CODE_TOKENIZER, CodeTokenizer);
}

fn build_index(index_dir: &Path, docs: &[DocFile]) -> Result<()> {
    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)
        .with_context(|| format!("creating guides index at {}", index_dir.display()))?;
    register_tokenizers(&index);

    let mut writer = index
        .writer(WRITER_HEAP_BYTES)
        .context("creating guides Tantivy writer")?;

    for doc in docs {
        let mut td = TantivyDocument::default();
        td.add_text(fields.path, &doc.rel_path);
        td.add_text(fields.title, &doc.title);
        td.add_text(fields.body, &doc.body);
        td.add_u64(fields.line_count, doc.line_count);
        td.add_text(fields.authors, doc.authors.as_deref().unwrap_or(""));
        writer.add_document(td).context("writing guides doc")?;
    }
    writer.commit().context("committing guides writer")?;
    Ok(())
}

fn open_existing(index_dir: &Path) -> Result<Opened> {
    let (_, fields) = build_schema();
    let index = Index::open_in_dir(index_dir)
        .with_context(|| format!("opening guides index at {}", index_dir.display()))?;
    register_tokenizers(&index);
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .context("building guides reader")?;
    Ok(Opened {
        index,
        reader,
        fields,
    })
}

fn build_hit(fields: &Fields, doc: &TantivyDocument, score: f32, slot_label: &str) -> SearchHit {
    let path = doc
        .get_first(fields.path)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = doc
        .get_first(fields.title)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let line_count = doc
        .get_first(fields.line_count)
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let authors_raw = doc
        .get_first(fields.authors)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let authors = if authors_raw.is_empty() {
        None
    } else {
        Some(authors_raw)
    };

    // Filename is the title for guides - the path is just `content/docs/...`
    // and the user thinks of guides by their human-readable heading.
    SearchHit {
        slot: slot_label.to_string(),
        source_type: "hm_doc".to_string(),
        path,
        fqn: String::new(),
        package: String::new(),
        filename: title.clone(),
        score,
        line_count,
        preview_line: None,
        preview: None,
        chunk_kind: "file".to_string(),
        symbol_name: title,
        start_line: None,
        end_line: None,
        debug: None,
        authors,
    }
}

// -----------------------------------------------------------------------
// Fingerprint + manifest persistence
// -----------------------------------------------------------------------

struct Fingerprint {
    file_count: u64,
    max_mtime_secs: u64,
}

fn compute_fingerprint(repo_dir: &Path, docs: &[DocFile]) -> Fingerprint {
    let mut max_mtime = 0u64;
    for doc in docs {
        let path = repo_dir.join(&doc.rel_path);
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                    let secs = dur.as_secs();
                    if secs > max_mtime {
                        max_mtime = secs;
                    }
                }
            }
        }
    }
    Fingerprint {
        file_count: docs.len() as u64,
        max_mtime_secs: max_mtime,
    }
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading guides manifest {}", path.display()))?;
    let parsed: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("parsing guides manifest {}", path.display()))?;
    Ok(parsed)
}

fn write_manifest(path: &Path, m: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating guides manifest dir {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(m).context("serializing guides manifest")?;
    std::fs::write(path, raw)
        .with_context(|| format!("writing guides manifest {}", path.display()))?;
    Ok(())
}

/// Upstream HM docs repo. The producer-side `atlas-build --hm-docs-fetch`
/// flag references the same URL; kept in sync so consumer + producer
/// pull from identical content.
const HM_DOCS_REPO_URL: &str = "https://github.com/HytaleModding/site";

/// Shallow-clone the HM docs repo into `repo_dir` if it doesn't already
/// exist. The guides backend needs a populated `<cache_root>/hm-docs/site/`
/// to walk; without this step the desktop client would silently serve
/// zero `hm_doc` results because the repo was only ever fetched on the
/// producer side (during `atlas-build`).
///
/// Idempotent: returns `Ok(())` immediately if the directory exists.
/// Refresh / update is a separate concern handled by the user explicitly
/// (or a future periodic task) - we don't want to nuke and re-clone on
/// every launch.
pub fn ensure_repo_cloned(repo_dir: &Path) -> Result<()> {
    if repo_dir.is_dir() {
        return Ok(());
    }
    let parent = repo_dir
        .parent()
        .with_context(|| format!("repo dir {} has no parent", repo_dir.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating HM docs cache dir {}", parent.display()))?;

    tracing::info!(
        repo = HM_DOCS_REPO_URL,
        target = %repo_dir.display(),
        "cloning HM docs repo"
    );
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", HM_DOCS_REPO_URL])
        .arg(repo_dir)
        .status()
        .context("running `git clone` (is git installed and on PATH?)")?;
    if !status.success() {
        bail!("git clone {HM_DOCS_REPO_URL} failed (exit {})", status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_doc(repo: &Path, rel: &str, body: &str) {
        let path = repo.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn search_returns_empty_when_repo_missing() {
        let data = tempdir().unwrap();
        let repo = data.path().join("nonexistent");
        let g = GuidesIndex::new(data.path().to_path_buf(), repo);
        g.sync_and_refresh().unwrap();
        assert!(!g.is_ready());
        let hits = g.search("anything", 10, "release").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn indexes_and_searches_markdown_files() {
        let data = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write_doc(
            repo.path(),
            "intro.md",
            "# Welcome\n\nGetting started with mods.",
        );
        write_doc(
            repo.path(),
            "content/docs/en/camera.mdx",
            "# Camera\n\nThe camera component renders the world.",
        );
        write_doc(
            repo.path(),
            "content/docs/de/camera.mdx",
            "# Kamera\n\nDie Kamera-Komponente rendert die Welt.",
        );

        let g = GuidesIndex::new(data.path().to_path_buf(), repo.path().to_path_buf());
        g.sync_and_refresh().unwrap();
        assert!(g.is_ready());

        let hits = g.search("camera", 10, "release").unwrap();
        assert!(hits.iter().any(|h| h.filename == "Camera"));
        assert!(!hits.iter().any(|h| h.filename == "Kamera"));
        assert!(hits.iter().all(|h| h.source_type == "hm_doc"));
        assert!(hits.iter().all(|h| h.slot == "release"));
    }

    #[test]
    fn unchanged_repo_skips_rebuild() {
        let data = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write_doc(repo.path(), "intro.md", "# Welcome\n\nbody");

        let g = GuidesIndex::new(data.path().to_path_buf(), repo.path().to_path_buf());
        g.sync_and_refresh().unwrap();
        let manifest_before = fs::read_to_string(g.manifest_path()).unwrap();

        // Second sync with no changes - manifest content identical.
        g.sync_and_refresh().unwrap();
        let manifest_after = fs::read_to_string(g.manifest_path()).unwrap();
        assert_eq!(manifest_before, manifest_after);
    }
}
