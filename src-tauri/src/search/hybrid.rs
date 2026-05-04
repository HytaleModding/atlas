//! Hybrid ranker: BM25 (Tantivy) fused with vector kNN (LanceDB) via
//! Reciprocal Rank Fusion. See [module docs](super) for the bigger
//! picture.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::config::Slot;
use crate::embedder::Embedder;
use crate::indexer::{HitDebug, SearchCatalog, SearchHit};
use crate::lance::{LanceStore, SemanticHit};

/// k in the RRF formula. The paper default of 60 is calibrated for very
/// long ranked lists where you want top-1 to NOT dominate. Atlas search
/// surfaces ~5-25 hits per leg, and the desktop UX needs the top-1 to
/// visibly stand apart from rank-10 - otherwise everything reads as a
/// 0.02 tie and "100 results, none clearly right" is the felt outcome.
/// k=15 keeps the diminishing-returns shape but compresses the curve so
/// rank drops are felt: top-1 contrib ≈ 0.063, rank-5 ≈ 0.05, rank-20
/// ≈ 0.029. Validated 2026-04-30 on the 223-file sample.
const RRF_K: f32 = 15.0;

/// Over-fetch multiplier per ranker. The final output is capped at
/// `limit`, but each ranker gets `limit * OVERFETCH` rows so the blend
/// has room to promote a row one ranker missed. Calibrated 2026-04-30
/// against the corrected golden set: at OVERFETCH=4, the "damage
/// calculation" query missed `DamageCalculatorSystems.java` because its
/// method chunk landed outside both legs' top-40. Bumping to 8 (fetch=80
/// at limit=10) lets the dual-leg overlap promote it to rank 6. Cost is
/// a few extra rows hashed through `rrf_blend` - negligible.
const OVERFETCH: usize = 8;

/// Run hybrid search against one slot. Returns the top `limit`
/// chunk-level hits (one row per matching method/class/field). The
/// frontend groups by file at render time so multiple methods inside
/// the same class can each surface as their own ranked row.
pub async fn run(
    catalog: Arc<SearchCatalog>,
    lance_store: Option<LanceStore>,
    embedder: Option<Arc<dyn Embedder>>,
    slot: Slot,
    index_dir: &Path,
    query: &str,
    limit: usize,
    source_types: Option<Vec<String>>,
) -> Result<Vec<SearchHit>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let fetch = limit.saturating_mul(OVERFETCH).max(limit);

 // --- Tantivy leg (blocking) ----------------------------------------
    let bm25_hits = {
        let catalog = catalog.clone();
        let index_dir = index_dir.to_path_buf();
        let q = trimmed.to_string();
        let st = source_types.clone();
        tokio::task::spawn_blocking(move || {
            catalog.search_chunks(slot, &index_dir, &q, fetch, st.as_deref())
        })
        .await
        .context("tantivy task panicked")??
    };

 // --- Lance leg (async) - skip if either piece is missing -----------
    let sem_hits: Vec<SemanticHit> = match (lance_store, embedder) {
        (Some(store), Some(emb)) => {
            let q = trimmed.to_string();
 // fastembed is sync + CPU-bound - run on a blocking thread so
 // it doesn't block the runtime.
            let emb_clone = emb.clone();
            let vectors = tokio::task::spawn_blocking(move || {
                emb_clone.embed_batch(&[q.as_str()])
            })
            .await
            .context("embed task panicked")?
            .context("embedding query")?;
            let q_vec = vectors
                .first()
                .ok_or_else(|| anyhow::anyhow!("embedder returned no vectors"))?;
            store
                .knn_search(q_vec, fetch, source_types.as_deref())
                .await?
        }
        _ => Vec::new(),
    };

    let (w_bm25, w_vec) = pick_weights(trimmed, !sem_hits.is_empty());
    let sem_distances: Vec<f32> = sem_hits.iter().map(|h| h.distance).collect();

    let mut out = rrf_blend(
        bm25_hits,
        sem_hits,
        sem_distances,
        w_bm25,
        w_vec,
        limit,
    );
    out.truncate(limit);
    Ok(out)
}

/// Pick blend weights from the query's shape. Returns
/// `(bm25_weight, vector_weight)` - both ≥ 0.
///
/// If the semantic leg didn't run, weights default to BM25-only.
fn pick_weights(query: &str, has_semantic: bool) -> (f32, f32) {
    if !has_semantic {
        return (1.0, 0.0);
    }

    let tokens: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return (1.0, 1.0);
    }

    let identifier_like = |t: &str| -> bool {
        let first = t.chars().next();
        let starts_ok = matches!(first, Some(c) if c.is_ascii_alphabetic() || c == '_');
        starts_ok
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$')
    };
    let has_uppercase = |t: &str| t.chars().any(|c| c.is_ascii_uppercase());

 // Code-shaped: has caps OR contains a separator/sigil that English
 // words don't (`_`, `.`, `$`). Lowercase English words slip past
 // `identifier_like` because they're alphanumeric, so without this
 // check NL queries like "send a chat message" never trip the
 // vector-favoring branch below.
    let code_shaped = |t: &str| -> bool {
        has_uppercase(t) || t.contains('_') || t.contains('.') || t.contains('$')
    };

    let all_ids = tokens.iter().all(|t| identifier_like(t));
    let any_caps = tokens.iter().any(|t| has_uppercase(t));
    let all_code_shaped = tokens.iter().all(|t| code_shaped(t));

 // Symbol-shaped: 1-2 identifier-like tokens, or anything camelCase.
 // Lean hard on BM25 - vectors tend to pull in near-synonyms which
 // dilute an exact-name lookup.
    if all_ids && (any_caps || tokens.len() <= 2) {
        return (2.0, 1.0);
    }

 // Natural-language-shaped: 3+ tokens that aren't all code-shaped.
 // Lean on the vector ranker - semantic similarity shines here.
    if tokens.len() >= 3 && !all_code_shaped {
        return (1.0, 2.0);
    }

 // Mixed / unclear - even split.
    (1.0, 1.0)
}

/// Combine two ranked lists into one using Reciprocal Rank Fusion.
/// Ranking is chunk-level (via `(slot, path, start_line)`), then
/// collapsed to one hit per file (highest-scoring chunk wins).
///
/// `sem_distances` is parallel to `sem_hits` (consumed) so the dedup
/// logic in this function can attach the raw cosine distance to each
/// hit's `HitDebug` field. Pass it explicitly rather than reading
/// it off the hit because `SemanticHit` is moved into the `Acc` struct
/// before otherwise are able to read it.
fn rrf_blend(
    bm25_hits: Vec<SearchHit>,
    sem_hits: Vec<SemanticHit>,
    sem_distances: Vec<f32>,
    w_bm25: f32,
    w_vec: f32,
    limit: usize,
) -> Vec<SearchHit> {
    struct Acc {
        score: f32,
        hit: SearchHit,
        bm25_rank: Option<u32>,
        bm25_score: Option<f32>,
        vector_rank: Option<u32>,
        vector_distance: Option<f32>,
    }
    let key = |slot: &str, path: &str, start: u64| -> String {
        format!("{slot}\0{path}\0{start}")
    };

    let mut acc: HashMap<String, Acc> = HashMap::new();

    for (rank, hit) in bm25_hits.into_iter().enumerate() {
        let start = hit.start_line.unwrap_or(0);
        let k = key(&hit.slot, &hit.path, start);
        let bm25_score = hit.score;
        let contrib = w_bm25 / (RRF_K + rank as f32 + 1.0);
        let rank_u32 = (rank + 1) as u32;
        acc.entry(k)
            .and_modify(|a| {
                a.score += contrib;
                a.bm25_rank = Some(rank_u32);
                a.bm25_score = Some(bm25_score);
            })
            .or_insert(Acc {
                score: contrib,
                hit,
                bm25_rank: Some(rank_u32),
                bm25_score: Some(bm25_score),
                vector_rank: None,
                vector_distance: None,
            });
    }
    for (rank, sh) in sem_hits.into_iter().enumerate() {
        let k = key(&sh.slot, &sh.path, sh.start_line);
        let contrib = w_vec / (RRF_K + rank as f32 + 1.0);
        let rank_u32 = (rank + 1) as u32;
        let distance = sem_distances.get(rank).copied();
        acc.entry(k)
            .and_modify(|a| {
                a.score += contrib;
                a.vector_rank = Some(rank_u32);
                a.vector_distance = distance;
            })
            .or_insert_with(|| Acc {
                score: contrib,
                hit: semantic_to_search_hit(sh),
                bm25_rank: None,
                bm25_score: None,
                vector_rank: Some(rank_u32),
                vector_distance: distance,
            });
    }

 // Flatten and roll the RRF score back onto the hit so the UI can
 // show something meaningful ("score 0.03" is meaningless, but the
 // ordering it produces is the real signal). Same pass attaches the
 // per-leg breakdown to each hit's `debug` field.
    let mut chunk_hits: Vec<SearchHit> = acc
        .into_values()
        .map(|a| {
            let mut h = a.hit;
            h.score = a.score;
            h.debug = Some(HitDebug {
                bm25_rank: a.bm25_rank,
                bm25_score: a.bm25_score,
                vector_rank: a.vector_rank,
                vector_distance: a.vector_distance,
                rrf_score: a.score,
                weight_bm25: w_bm25,
                weight_vector: w_vec,
            });
            h
        })
        .collect();

 // Sort all chunk hits by score and truncate to the caller's limit.
 // No per-file dedup: the frontend groups by file and ranks every
 // method-level chunk so a class with three high-scoring methods
 // surfaces three rows under one file group, not one.
    chunk_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    chunk_hits.truncate(limit);
    chunk_hits
}

/// Minimal SemanticHit → SearchHit conversion for rows that only the
/// vector ranker surfaced. Lose the BM25 score but keep all
/// per-chunk metadata (symbol, line range, chunk_kind) so the UI
/// renders the same row shape.
fn semantic_to_search_hit(sh: SemanticHit) -> SearchHit {
    let preview_line = if sh.start_line > 0 {
        Some(sh.start_line as usize)
    } else {
        None
    };
    SearchHit {
        slot: sh.slot,
        source_type: sh.source_type,
        path: sh.path,
        fqn: sh.fqn,
        package: sh.package,
        filename: sh.filename,
        score: 0.0, // overwritten with RRF score in rrf_blend
        line_count: sh.line_count,
        preview_line,
        preview: None,
        chunk_kind: sh.chunk_kind,
        symbol_name: sh.symbol,
        start_line: Some(sh.start_line),
        end_line: Some(sh.end_line),
        debug: None, // populated by rrf_blend
        authors: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_shaped_queries_favor_bm25() {
        assert_eq!(pick_weights("PageManager", true), (2.0, 1.0));
        assert_eq!(pick_weights("getComponent", true), (2.0, 1.0));
        assert_eq!(pick_weights("com.hytale.Foo", true), (2.0, 1.0));
    }

    #[test]
    fn nl_shaped_queries_favor_vectors() {
        assert_eq!(pick_weights("send a chat message", true), (1.0, 2.0));
        assert_eq!(pick_weights("how do players join a world", true), (1.0, 2.0));
    }

    #[test]
    fn fallback_to_bm25_when_semantic_missing() {
        assert_eq!(pick_weights("anything here", false), (1.0, 0.0));
    }

    #[test]
    fn rrf_promotes_rows_seen_in_both_rankers() {
 // Row A: rank 5 in BM25, rank 1 in vector.
 // Row B: rank 1 in BM25 only.
 // With equal weights, A should outscore B because it benefits
 // from both rankers.
        let mk_hit = |path: &str, start: u64, score: f32| SearchHit {
            slot: "release".into(),
            source_type: "source".into(),
            path: path.into(),
            fqn: String::new(),
            package: String::new(),
            filename: path.into(),
            score,
            line_count: 0,
            preview_line: None,
            preview: None,
            chunk_kind: "type".into(),
            symbol_name: String::new(),
            start_line: Some(start),
            end_line: Some(start),
            debug: None,
            authors: None,
        };
        let mk_sem = |path: &str, start: u64| SemanticHit {
            slot: "release".into(),
            source_type: "source".into(),
            path: path.into(),
            fqn: String::new(),
            package: String::new(),
            filename: path.into(),
            symbol: String::new(),
            chunk_kind: "type".into(),
            start_line: start,
            end_line: start,
            line_count: 0,
            distance: 0.1,
        };

 // BM25: B at rank 1, then 4 others, then A at rank 6.
        let bm25 = vec![
            mk_hit("B.java", 1, 10.0),
            mk_hit("X1.java", 1, 9.0),
            mk_hit("X2.java", 1, 8.0),
            mk_hit("X3.java", 1, 7.0),
            mk_hit("X4.java", 1, 6.0),
            mk_hit("A.java", 1, 5.0),
        ];
 // Vectors: A at rank 1.
        let sem = vec![mk_sem("A.java", 1)];
        let sem_distances = vec![0.1];

        let out = rrf_blend(bm25, sem, sem_distances, 1.0, 1.0, 10);
        assert!(!out.is_empty());
        assert_eq!(out[0].path, "A.java", "A should win hybrid ranking");
    }
}
