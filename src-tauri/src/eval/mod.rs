//! Search-quality evaluation harness.
//!
//! Runs a hand-curated golden query set (`eval/queries.json`) through
//! the same `search::hybrid` pipeline the desktop app uses, then scores
//! results against expected hits. Output is a JSON report with
//! Top-1 / Top-3 / MRR plus per-query breakdowns. Two reports can be
//! diffed so any pipeline change (threshold tweak, weight tweak, new
//! reranker stage) becomes a measurable delta instead of vibes.
//!
//! Match rule: a result counts as a hit if any path in
//! `expected_top_paths` is a SUFFIX of the actual hit's path. Suffix
//! matching lets curators write `command/.../InventorySeeCommand.java`
//! without the `com/hypixel/hytale/server/core/` prefix that varies
//! across slots.
//!
//! Negative queries (empty `expected_top_paths`) flip the success
//! condition: a "blarg" query passes if the pipeline returns few or
//! zero hits. These are tracked separately from positive metrics so
//! a flood of irrelevant results on a single gibberish query can't
//! tank Top-1 / MRR.

use std::path::Path;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Slot;
use crate::embedder::Embedder;
use crate::indexer::metadata::format_iso8601;
use crate::indexer::SearchCatalog;
use crate::lance::LanceStore;
use crate::search::hybrid;

/// Golden query file loaded from `eval/queries.json`. Schema is
/// versioned so we can grow the file format without breaking older
/// reports.
#[derive(Debug, Deserialize)]
pub struct GoldenSet {
    pub schema_version: u32,
    #[serde(default)]
    pub description: String,
    pub queries: Vec<GoldenQuery>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GoldenQuery {
    pub query: String,
    /// One or more acceptable hit paths. Suffix-matched against actual
    /// result paths. Empty list = negative query (expect ~zero hits).
    pub expected_top_paths: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

/// Pipeline knobs surfaced in the report so a regression can be traced
/// back to a specific config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalConfig {
    /// How many hits to fetch per query. Top-1 / Top-3 are computed
    /// against this slice; MRR also uses it as the rank ceiling.
    pub top_k: usize,
    /// Threshold below which a "negative" query is considered to have
    /// passed. With the kNN distance floor in place, gibberish should
    /// produce 0; we allow a small tolerance for noise.
    pub negative_max_hits: usize,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            negative_max_hits: 3,
        }
    }
}

/// Per-query result. Captures everything needed to diff two reports
/// without re-running the search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub query: String,
    pub expected_top_paths: Vec<String>,
    pub actual_top_paths: Vec<String>,
    /// 1-indexed rank of the best-matching expected path within
    /// `top_k`. `None` if no expected path appeared.
    pub best_expected_rank: Option<usize>,
    pub top1_hit: bool,
    pub top3_hit: bool,
    /// `1 / best_expected_rank` for positive queries; `0.0` if no
    /// expected path appeared. Always `0.0` for negative queries (they
    /// don't contribute to MRR).
    pub reciprocal_rank: f32,
    /// True for negative queries where actual_top_paths.len() <=
    /// `negative_max_hits`.
    pub negative_pass: Option<bool>,
    pub elapsed_ms: u64,
}

impl QueryResult {
    pub fn is_negative(&self) -> bool {
        self.expected_top_paths.is_empty()
    }
}

/// Aggregate metrics + per-query detail. Serialised verbatim as the
/// report file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub schema_version: u32,
    pub ran_at: String,
    pub staging: String,
    pub queries_path: String,
    pub config: EvalConfig,
    pub summary: Summary,
    pub per_query: Vec<QueryResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub n_queries: usize,
    pub n_positive: usize,
    pub n_negative: usize,
    pub top1_hit_rate: f32,
    pub top3_hit_rate: f32,
    pub mrr: f32,
    pub negative_pass_rate: f32,
    pub mean_elapsed_ms: f32,
}

/// Run the full golden set against a hybrid search pipeline. The
/// caller passes the staging directory's `lance/` path (or `None` for
/// keyword-only evaluation); the eval re-opens the store per query
/// because `LanceStore` is not `Clone` and `hybrid::run` consumes it.
/// Re-open cost is negligible compared to the kNN search itself, and
/// keeps eval's perf shape identical to the live Tauri command (which
/// also re-opens per `search` invocation).
pub async fn run_eval(
    set: &GoldenSet,
    catalog: Arc<SearchCatalog>,
    lance_dir: Option<&Path>,
    embedder: Option<Arc<dyn Embedder>>,
    slot: Slot,
    index_dir: &Path,
    config: &EvalConfig,
    staging_label: String,
    queries_label: String,
) -> Result<EvalReport> {
    let mut per_query: Vec<QueryResult> = Vec::with_capacity(set.queries.len());

    for gq in &set.queries {
        let start = Instant::now();
        let hits = run_one(
            catalog.clone(),
            lance_dir,
            embedder.clone(),
            slot,
            index_dir,
            &gq.query,
            config.top_k,
        )
        .await
        .with_context(|| format!("running query {:?}", gq.query))?;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        let actual_top_paths: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();

        let best_expected_rank = if gq.expected_top_paths.is_empty() {
            None
        } else {
            actual_top_paths.iter().enumerate().find_map(|(i, actual)| {
                let matches = gq
                    .expected_top_paths
                    .iter()
                    .any(|exp| actual_path_matches(actual, exp));
                if matches {
                    Some(i + 1)
                } else {
                    None
                }
            })
        };

        let top1_hit = best_expected_rank == Some(1);
        let top3_hit = matches!(best_expected_rank, Some(r) if r <= 3);
        let reciprocal_rank = best_expected_rank
            .map(|r| 1.0_f32 / r as f32)
            .unwrap_or(0.0);

        let negative_pass = if gq.expected_top_paths.is_empty() {
            Some(actual_top_paths.len() <= config.negative_max_hits)
        } else {
            None
        };

        per_query.push(QueryResult {
            query: gq.query.clone(),
            expected_top_paths: gq.expected_top_paths.clone(),
            actual_top_paths,
            best_expected_rank,
            top1_hit,
            top3_hit,
            reciprocal_rank,
            negative_pass,
            elapsed_ms,
        });
    }

    let summary = summarize(&per_query);

    Ok(EvalReport {
        schema_version: 1,
        ran_at: format_iso8601(SystemTime::now()),
        staging: staging_label,
        queries_path: queries_label,
        config: config.clone(),
        summary,
        per_query,
    })
}

/// Single-query wrapper. Re-opens the Lance store from disk per call
/// because `LanceStore` is not `Clone` and `hybrid::run` consumes it.
/// Same shape as the live Tauri `search` command.
async fn run_one(
    catalog: Arc<SearchCatalog>,
    lance_dir: Option<&Path>,
    embedder: Option<Arc<dyn Embedder>>,
    slot: Slot,
    index_dir: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<crate::indexer::SearchHit>> {
    let lance_for_call = match lance_dir {
        Some(p) => LanceStore::open_existing(p).await?,
        None => None,
    };
    hybrid::run(
        catalog,
        lance_for_call,
        embedder,
        slot,
        index_dir,
        query,
        limit,
        None,
    )
    .await
}

fn actual_path_matches(actual: &str, expected_suffix: &str) -> bool {
    // Normalize both to forward-slash so a curator can write Unix-style
    // paths even when the index stores Windows-style.
    let a = actual.replace('\\', "/");
    let e = expected_suffix.replace('\\', "/");
    a.ends_with(&e)
}

fn summarize(per_query: &[QueryResult]) -> Summary {
    let n_queries = per_query.len();
    let positives: Vec<&QueryResult> = per_query.iter().filter(|q| !q.is_negative()).collect();
    let negatives: Vec<&QueryResult> = per_query.iter().filter(|q| q.is_negative()).collect();
    let n_positive = positives.len();
    let n_negative = negatives.len();

    let top1 = positives.iter().filter(|q| q.top1_hit).count() as f32;
    let top3 = positives.iter().filter(|q| q.top3_hit).count() as f32;
    let mrr_sum: f32 = positives.iter().map(|q| q.reciprocal_rank).sum();
    let neg_pass = negatives
        .iter()
        .filter(|q| q.negative_pass.unwrap_or(false))
        .count() as f32;

    let safe_div = |num: f32, den: usize| if den == 0 { 0.0 } else { num / den as f32 };
    let mean_elapsed = if n_queries == 0 {
        0.0
    } else {
        per_query.iter().map(|q| q.elapsed_ms as f32).sum::<f32>() / n_queries as f32
    };

    Summary {
        n_queries,
        n_positive,
        n_negative,
        top1_hit_rate: safe_div(top1, n_positive),
        top3_hit_rate: safe_div(top3, n_positive),
        mrr: safe_div(mrr_sum, n_positive),
        negative_pass_rate: safe_div(neg_pass, n_negative),
        mean_elapsed_ms: mean_elapsed,
    }
}

/// Print a human-readable report to stdout. Stable formatting so CI
/// log diffs are tractable.
pub fn print_report(report: &EvalReport) {
    let s = &report.summary;
    println!("Atlas search eval - {}", report.ran_at);
    println!("  staging: {}", report.staging);
    println!("  queries: {}", report.queries_path);
    println!(
        "  config:  top_k={} negative_max_hits={}",
        report.config.top_k, report.config.negative_max_hits
    );
    println!();
    println!(
        "Summary ({} queries: {} positive, {} negative)",
        s.n_queries, s.n_positive, s.n_negative
    );
    println!("  Top-1 hit rate         {:>5.1}%", s.top1_hit_rate * 100.0);
    println!("  Top-3 hit rate         {:>5.1}%", s.top3_hit_rate * 100.0);
    println!("  MRR                    {:>5.3}", s.mrr);
    println!(
        "  Negative pass rate     {:>5.1}%",
        s.negative_pass_rate * 100.0
    );
    println!("  Mean latency           {:>5.0}ms", s.mean_elapsed_ms);
    println!();
    println!("Per-query:");
    for q in &report.per_query {
        let kind = if q.is_negative() { "neg" } else { "pos" };
        let rank_str = match q.best_expected_rank {
            Some(r) => format!("rank {r}"),
            None if q.is_negative() => match q.negative_pass {
                Some(true) => "PASS".to_string(),
                Some(false) => format!("FAIL ({} hits)", q.actual_top_paths.len()),
                None => "-".to_string(),
            },
            None => "MISS".to_string(),
        };
        println!(
            "  [{kind}] {:<48} {:<20} {:>4}ms",
            truncate(&q.query, 48),
            rank_str,
            q.elapsed_ms
        );
    }
}

/// Diff two reports and print the deltas. Uses the older report's
/// per-query list as the spine so the diff is stable even if queries
/// are added between runs (added queries are listed at the bottom).
pub fn print_diff(prev: &EvalReport, curr: &EvalReport) {
    println!();
    println!("Diff vs {} (prev) → {} (curr)", prev.ran_at, curr.ran_at);

    let dp = curr.summary.top1_hit_rate - prev.summary.top1_hit_rate;
    let dt = curr.summary.top3_hit_rate - prev.summary.top3_hit_rate;
    let dm = curr.summary.mrr - prev.summary.mrr;
    let dn = curr.summary.negative_pass_rate - prev.summary.negative_pass_rate;
    println!("  Top-1 hit rate         {:>+6.1}%", dp * 100.0);
    println!("  Top-3 hit rate         {:>+6.1}%", dt * 100.0);
    println!("  MRR                    {:>+6.3}", dm);
    println!("  Negative pass rate     {:>+6.1}%", dn * 100.0);
    println!();

    let by_query_prev: std::collections::HashMap<&str, &QueryResult> = prev
        .per_query
        .iter()
        .map(|q| (q.query.as_str(), q))
        .collect();

    let mut improved: Vec<String> = Vec::new();
    let mut regressed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    for c in &curr.per_query {
        match by_query_prev.get(c.query.as_str()) {
            None => added.push(c.query.clone()),
            Some(p) => {
                let p_rank = p.best_expected_rank;
                let c_rank = c.best_expected_rank;
                let line = format!(
                    "  {:<48} {:?} → {:?}",
                    truncate(&c.query, 48),
                    p_rank,
                    c_rank
                );
                if rank_improved(p_rank, c_rank) {
                    improved.push(line);
                } else if rank_regressed(p_rank, c_rank) {
                    regressed.push(line);
                }
            }
        }
    }

    if !improved.is_empty() {
        println!("Improved ({}):", improved.len());
        for l in &improved {
            println!("{l}");
        }
        println!();
    }
    if !regressed.is_empty() {
        println!("Regressed ({}):", regressed.len());
        for l in &regressed {
            println!("{l}");
        }
        println!();
    }
    if !added.is_empty() {
        println!("Added queries ({}):", added.len());
        for q in &added {
            println!("  {}", truncate(q, 48));
        }
    }
}

fn rank_improved(prev: Option<usize>, curr: Option<usize>) -> bool {
    match (prev, curr) {
        (Some(p), Some(c)) => c < p,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn rank_regressed(prev: Option<usize>, curr: Option<usize>) -> bool {
    match (prev, curr) {
        (Some(p), Some(c)) => c > p,
        (Some(_), None) => true,
        _ => false,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}
