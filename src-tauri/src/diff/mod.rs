//! Diff tracker: "what would break in the user's mod if Hytale shipped X".
//!
//! Given a user's mod source folder + two `symbols.sqlite` snapshots
//! (typically the active Release vs an upcoming Pre-release), the diff
//! engine extracts every external API reference in the user's code,
//! resolves it against both snapshots, and reports the deltas grouped
//! by severity.
//!
//! # Pipeline
//!
//! ```text
//! user .java files ─▶ extract::extract_refs ─▶ Vec<ApiRef>
//!     each ApiRef ─▶ resolve::resolve(baseline) ─▶ Resolution
//!     each ApiRef ─▶ resolve::resolve(target)   ─▶ Resolution
//!         pair    ─▶ report::compare_pair      ─▶ DiffEntry
//! ```
//!
//! # Scope (V1)
//!
//! The extractor is intentionally restricted: it tracks **imports** as
//! the canonical source of "what external types this file talks to",
//! plus **method invocations** and **field accesses** *qualified by an
//! imported class name*. Without type inference we can't reliably trace
//! `instance.method(...)` back to the instance's declaring class, so
//! we don't try - that's a deliberate V1 cut to ship something
//! actually correct rather than something that guesses. The extractor
//! still catches the high-value patterns that dominate real plugin
//! code: imports, static calls, and constants on imported types.
//!
//! # Severity model
//!
//! See [`DiffSeverity`]. Roughly:
//! - `Removed`: was found in baseline, not found in target.
//! - `SignatureChanged`: method name found in target but with different
//!   return type or parameter list.
//! - `Deprecated`: present in both, but `@Deprecated` was added in target.
//! - `RenamedLikely`: not found in target, but a similarly-named symbol
//!   exists on the same class (Levenshtein ≤ 3).
//! - `Unchanged`: same in both. Reported in `unchanged_count` only,
//!   not as individual entries (the UI doesn't need the noise).

pub mod compare;
pub mod extract;
pub mod report;
pub mod resolve;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::indexer::symbols::SymbolsDb;

/// Kind discriminator for a reference extracted from user code. Mirrors
/// the three symbol tables in `symbols.sqlite` (classes/methods/fields).
/// Constructors aren't a separate kind here - they show up as method refs
/// whose `name` equals the class simple name; resolution handles them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    Class,
    Method,
    Field,
}

/// A single external API reference extracted from one of the user's
/// `.java` files. `class_fqn` is the fully-qualified class name resolved
/// from the file's import list (for `Method`/`Field` refs) or the
/// imported FQN itself (for `Class` refs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ApiRef {
    pub kind: ApiKind,
    pub class_fqn: String,
    /// `None` for `Class` refs; required for `Method` / `Field`.
    pub member_name: Option<String>,
    /// Project-relative path of the user file containing this reference.
    pub source_path: String,
    pub line: u32,
}

/// Outcome of resolving one [`ApiRef`] against a single `symbols.sqlite`
/// snapshot. `Found` carries the resolved row(s); `NameOnly` means the
/// member name exists on the class but the recorded modifiers / signature
/// differ; `NotFound` means nothing matches.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Resolution {
    Found {
        modifiers: Vec<String>,
        signature: Option<String>,
        param_types: Vec<String>,
        return_type: Option<String>,
    },
    /// Used only for method refs: the name exists on the class but at
    /// least one row is present. Encodes all matching overloads so the
    /// reporter can compare signature lists.
    NameOnly {
        candidates: Vec<MethodCandidate>,
    },
    NotFound,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodCandidate {
    pub modifiers: Vec<String>,
    pub return_type: Option<String>,
    pub param_types: Vec<String>,
}

/// One row in the diff report. Severity decides which bucket the UI
/// drops it in; `note` is human-readable detail (old vs new signature,
/// rename suggestion, etc.).
#[derive(Debug, Clone, Serialize)]
pub struct DiffEntry {
    pub severity: DiffSeverity,
    pub api_ref: ApiRef,
    pub baseline: Resolution,
    pub target: Resolution,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSeverity {
    Removed,
    SignatureChanged,
    Deprecated,
    RenamedLikely,
    /// Skipped from the report body; counted in `unchanged_count`.
    Unchanged,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub baseline_label: String,
    pub target_label: String,
    /// Entries grouped by severity for cheap UI rendering. `unchanged`
    /// rows are excluded - their count lives in `unchanged_count`.
    pub removed: Vec<DiffEntry>,
    pub signature_changed: Vec<DiffEntry>,
    pub deprecated: Vec<DiffEntry>,
    pub renamed_likely: Vec<DiffEntry>,
    pub unchanged_count: usize,
    /// Refs that didn't resolve in *either* snapshot. Usually means the
    /// import points at a class outside the indexed corpus (third-party
    /// dep, `java.util.*`, etc.) - not actionable, but surfaced as a
    /// soft hint so the user knows we ignored them.
    pub external_count: usize,
}

/// Run the full diff pipeline. `project_dir` is the user's mod source
/// folder; the two `*_symbols_path` arguments point at `symbols.sqlite`
/// files belonging to the baseline and target builds respectively.
pub fn run_project_diff(
    project_dir: &Path,
    baseline_label: String,
    baseline_symbols_path: &Path,
    target_label: String,
    target_symbols_path: &Path,
) -> Result<DiffReport> {
    let refs = extract::extract_refs(project_dir)
        .with_context(|| format!("extracting refs from {}", project_dir.display()))?;

    let baseline_db = SymbolsDb::open_read_only(baseline_symbols_path)
        .with_context(|| format!("opening baseline at {}", baseline_symbols_path.display()))?;
    let target_db = SymbolsDb::open_read_only(target_symbols_path)
        .with_context(|| format!("opening target at {}", target_symbols_path.display()))?;

    let mut report = DiffReport {
        baseline_label,
        target_label,
        removed: Vec::new(),
        signature_changed: Vec::new(),
        deprecated: Vec::new(),
        renamed_likely: Vec::new(),
        unchanged_count: 0,
        external_count: 0,
    };

    for api_ref in refs {
        let baseline = resolve::resolve(&baseline_db, &api_ref)?;
        let target = resolve::resolve(&target_db, &api_ref)?;
        match report::compare_pair(api_ref, baseline, target, &target_db)? {
            Some(entry) => match entry.severity {
                DiffSeverity::Removed => report.removed.push(entry),
                DiffSeverity::SignatureChanged => report.signature_changed.push(entry),
                DiffSeverity::Deprecated => report.deprecated.push(entry),
                DiffSeverity::RenamedLikely => report.renamed_likely.push(entry),
                DiffSeverity::Unchanged => report.unchanged_count += 1,
            },
            None => report.external_count += 1,
        }
    }

    Ok(report)
}

/// Helper used by the Tauri command and tests: given a build root
/// directory (the value `MountedIndexEntry::path` already exposes), pick
/// the canonical `symbols.sqlite` path. Mounted Hytale builds store it
/// at `<root>/symbols.sqlite`; project indexes nest it under
/// `<root>/tantivy/symbols.sqlite`. We probe both so the diff command
/// doesn't care which shape it got.
pub fn pick_symbols_path(root: &Path) -> Option<PathBuf> {
    let direct = root.join("symbols.sqlite");
    if direct.is_file() {
        return Some(direct);
    }
    let nested = root.join("tantivy").join("symbols.sqlite");
    if nested.is_file() {
        return Some(nested);
    }
    None
}
