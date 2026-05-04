//! Hybrid keyword + semantic search.
//!
//! Fuses the Tantivy BM25 ranker with a LanceDB vector ranker using
//! Reciprocal Rank Fusion (RRF). The blend weights are chosen per-query
//! from a lightweight shape heuristic - symbol-like queries lean on
//! BM25, natural-language queries lean on the vector ranker, and mixed
//! queries treat the two evenly. No user-facing toggle, per plan.md.

pub mod hybrid;
