//! LanceDB vector store for Atlas semantic search.
//!
//! One LanceDB instance per slot lives at
//! `<data_dir>/indexes/lance/{slot}/`, with a single `chunks` table
//! holding one row per chunk. Schema mirrors the Tantivy schema so
//! hybrid RRF blending can match rows across the two stores on
//! `(slot, path, start_line)`.
//!
//! The embedding column is a `FixedSizeList<Float32, 384>` - the dim is
//! pinned to [`crate::embedder::EMBEDDING_DIM`] so the schema stays in
//! lockstep with the embedder trait's contract.
//!
//! Chunk *body text* is intentionally not stored. The vector store carries
//! only metadata + the embedding vector; embeddings are not reversible to
//! source. Snippet display is reconstituted from the user's local decompile
//! at query time, mirroring the Tantivy schema's same decision. This keeps
//! the published index artifact free of decompiled implementation, so it
//! can be distributed without redistributing source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use arrow::array::{
    types::Float32Type, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, StringArray,
    UInt64Array,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use futures_util::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{DistanceType, Connection, Table};
use serde::Serialize;

use crate::config::Slot;
use crate::embedder::EMBEDDING_DIM;

const TABLE_NAME: &str = "chunks";

/// Upper bound on cosine distance for a kNN hit to be returned. Cosine
/// distance is `1 - cosine_similarity`, range [0, 2]: 0 = identical
/// direction, 1 = orthogonal, 2 = opposite.
///
/// Calibrated empirically against the 29-query golden set on 2026-04-30:
/// legitimate top-1 hits sit at 0.16-0.28 (symbol queries tighter than
/// NL); negative queries ("blarg", "xyzqwerty") have no BM25 match and
/// their *closest* vector neighbour sits at 0.31-0.34. Cutting at 0.30
/// drops the gibberish slop entirely while still letting BM25 carry
/// any legitimate rank-2/3 chunk that happens to sit at vdist 0.30-0.35.
/// Prior value of 0.85 was a pre-eval guess that left every gibberish
/// query returning 10 hits.
const MAX_KNN_DISTANCE: f32 = 0.30;

/// Directory holding the LanceDB instance for one slot.
pub fn lance_dir_for(data_dir: &Path, slot: Slot) -> PathBuf {
    data_dir.join("indexes").join("lance").join(slot.as_str())
}

/// Arrow schema for the chunks table. Keep fields and their order
/// parallel to the Tantivy schema - hybrid blending joins on
/// `(slot, path, start_line)`, so those three MUST be present.
pub fn chunk_schema() -> SchemaRef {
    let item = Arc::new(Field::new("item", DataType::Float32, true));
    Arc::new(Schema::new(vec![
        Field::new("slot", DataType::Utf8, false),
        Field::new("source_type", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("package", DataType::Utf8, false),
        Field::new("fqn", DataType::Utf8, false),
        Field::new("filename", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("chunk_kind", DataType::Utf8, false),
        Field::new("start_line", DataType::UInt64, false),
        Field::new("end_line", DataType::UInt64, false),
        Field::new("line_count", DataType::UInt64, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(item, EMBEDDING_DIM as i32),
            false,
        ),
    ]))
}

/// One chunk row pending write. Borrowed strings let the caller hand off
/// slices without cloning; the row is consumed into an Arrow batch before
/// the borrow ends.
#[derive(Debug)]
pub struct ChunkRow<'a> {
    pub slot: &'a str,
    pub source_type: &'a str,
    pub path: &'a str,
    pub package: &'a str,
    pub fqn: &'a str,
    pub filename: &'a str,
    pub symbol: &'a str,
    pub chunk_kind: &'a str,
    pub start_line: u64,
    pub end_line: u64,
    pub line_count: u64,
    pub embedding: &'a [f32],
}

/// Collect rows into an Arrow [`RecordBatch`] matching [`chunk_schema`].
/// Fails if any embedding has the wrong dim - better to crash the indexer
/// here than to silently corrupt the vector index.
pub fn batch_from_rows(rows: &[ChunkRow<'_>]) -> Result<RecordBatch> {
    let schema = chunk_schema();

    let slot = StringArray::from_iter_values(rows.iter().map(|r| r.slot));
    let source_type = StringArray::from_iter_values(rows.iter().map(|r| r.source_type));
    let path = StringArray::from_iter_values(rows.iter().map(|r| r.path));
    let package = StringArray::from_iter_values(rows.iter().map(|r| r.package));
    let fqn = StringArray::from_iter_values(rows.iter().map(|r| r.fqn));
    let filename = StringArray::from_iter_values(rows.iter().map(|r| r.filename));
    let symbol = StringArray::from_iter_values(rows.iter().map(|r| r.symbol));
    let chunk_kind = StringArray::from_iter_values(rows.iter().map(|r| r.chunk_kind));

    let start_line = UInt64Array::from_iter_values(rows.iter().map(|r| r.start_line));
    let end_line = UInt64Array::from_iter_values(rows.iter().map(|r| r.end_line));
    let line_count = UInt64Array::from_iter_values(rows.iter().map(|r| r.line_count));

    // Build the FixedSizeList<Float32> embedding column. Up-front dim
    // check keeps a bad embedder from writing rows that'd poison kNN.
    for r in rows {
        if r.embedding.len() != EMBEDDING_DIM {
            return Err(anyhow!(
                "embedding length mismatch: got {}, expected {}",
                r.embedding.len(),
                EMBEDDING_DIM
            ));
        }
    }
    let embedding = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter()
            .map(|r| Some(r.embedding.iter().map(|v| Some(*v)).collect::<Vec<_>>())),
        EMBEDDING_DIM as i32,
    );

    let arrays: Vec<ArrayRef> = vec![
        Arc::new(slot),
        Arc::new(source_type),
        Arc::new(path),
        Arc::new(package),
        Arc::new(fqn),
        Arc::new(filename),
        Arc::new(symbol),
        Arc::new(chunk_kind),
        Arc::new(start_line),
        Arc::new(end_line),
        Arc::new(line_count),
        Arc::new(embedding),
    ];

    RecordBatch::try_new(schema, arrays).context("building RecordBatch for Lance write")
}

/// Opened chunks table for one slot. Holds `Connection` alongside
/// `Table` to keep the underlying database handle alive for the caller.
pub struct LanceStore {
    #[allow(dead_code)]
    conn: Connection,
    table: Table,
}

impl LanceStore {
    /// Create a fresh chunks table at `lance_dir`, blowing away any prior
    /// rows. Matches the Tantivy rebuild contract - a new index run
    /// starts from an empty state.
    pub async fn reset(lance_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(lance_dir)
            .with_context(|| format!("creating lance dir {}", lance_dir.display()))?;
        let uri = lance_dir.to_string_lossy().to_string();
        let conn = lancedb::connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to LanceDB at {uri}"))?;

        let names = conn
            .table_names()
            .execute()
            .await
            .context("listing Lance tables")?;
        if names.iter().any(|n| n == TABLE_NAME) {
            conn.drop_table(TABLE_NAME, &[])
                .await
                .with_context(|| format!("dropping old `{TABLE_NAME}` table"))?;
        }

        let schema = chunk_schema();
        let table = conn
            .create_empty_table(TABLE_NAME, schema)
            .execute()
            .await
            .with_context(|| format!("creating `{TABLE_NAME}` table"))?;

        Ok(Self { conn, table })
    }

    /// Open an existing chunks table. Returns `Ok(None)` if the directory
    /// or table isn't present (so the UI can prompt for an index rebuild
    /// instead of erroring).
    pub async fn open_existing(lance_dir: &Path) -> Result<Option<Self>> {
        if !lance_dir.exists() {
            return Ok(None);
        }
        let uri = lance_dir.to_string_lossy().to_string();
        let conn = lancedb::connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to LanceDB at {uri}"))?;
        let names = conn
            .table_names()
            .execute()
            .await
            .context("listing Lance tables")?;
        if !names.iter().any(|n| n == TABLE_NAME) {
            return Ok(None);
        }
        let table = conn
            .open_table(TABLE_NAME)
            .execute()
            .await
            .with_context(|| format!("opening `{TABLE_NAME}` table"))?;
        Ok(Some(Self { conn, table }))
    }

    pub async fn add_batch(&self, batch: RecordBatch) -> Result<()> {
        self.table
            .add(batch)
            .execute()
            .await
            .context("adding rows to Lance table")?;
        Ok(())
    }

    /// Delete every row matching a SQL `WHERE`-style predicate. Used by
    /// `atlas-build add-section` to wipe one source type's rows before
    /// re-ingesting them, leaving the rest of the table untouched.
    /// Caller is responsible for sanitizing string literals - current
    /// callers only embed `SourceType::as_str()` values, which are
    /// hard-coded ASCII identifiers.
    pub async fn delete_where(&self, predicate: &str) -> Result<()> {
        self.table
            .delete(predicate)
            .await
            .with_context(|| format!("deleting rows where `{predicate}`"))?;
        Ok(())
    }

    pub async fn count_rows(&self) -> Result<usize> {
        self.table
            .count_rows(None)
            .await
            .context("counting rows in Lance table")
    }

    /// Nearest-neighbour search over the embedding column. Returns the
    /// top `limit` rows ordered by distance (ascending). When
    /// `source_types` is non-empty, the search is restricted to rows
    /// whose `source_type` matches one of the listed sections.
    pub async fn knn_search(
        &self,
        query: &[f32],
        limit: usize,
        source_types: Option<&[String]>,
    ) -> Result<Vec<SemanticHit>> {
        if query.len() != EMBEDDING_DIM {
            return Err(anyhow!(
                "query vector length mismatch: got {}, expected {}",
                query.len(),
                EMBEDDING_DIM
            ));
        }
        let mut builder = self
            .table
            .vector_search(query)
            .context("building vector search")?
            .distance_type(DistanceType::Cosine)
            .distance_range(None, Some(MAX_KNN_DISTANCE))
            .limit(limit);
        if let Some(types) = source_types {
            if !types.is_empty() {
                // SQL-shape predicate, single-quoted string literals.
                // SourceType values are short ASCII identifiers
                // (`source`, `hm_doc`, `hypixel_doc`, `asset`) - no
                // injection surface, but escape quotes defensively.
                let list: Vec<String> = types
                    .iter()
                    .map(|t| format!("'{}'", t.replace('\'', "''")))
                    .collect();
                let pred = format!("source_type IN ({})", list.join(","));
                builder = builder.only_if(pred);
            }
        }
        let stream = builder
            .execute()
            .await
            .context("running vector search")?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .context("collecting vector search stream")?;
        let mut hits = Vec::new();
        for batch in batches {
            extract_hits(&batch, &mut hits)?;
        }
        Ok(hits)
    }
}

/// Row returned by [`LanceStore::knn_search`]. Mirrors the Tantivy
/// [`crate::indexer::SearchHit`] fields that hybrid blending needs.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticHit {
    pub slot: String,
    pub source_type: String,
    pub path: String,
    pub fqn: String,
    pub package: String,
    pub filename: String,
    pub symbol: String,
    pub chunk_kind: String,
    pub start_line: u64,
    pub end_line: u64,
    pub line_count: u64,
    pub distance: f32,
}

fn extract_hits(batch: &RecordBatch, out: &mut Vec<SemanticHit>) -> Result<()> {
    use arrow::array::Array;

    let schema = batch.schema();
    let get_str = |name: &str| -> Result<&StringArray> {
        let idx = schema
            .index_of(name)
            .with_context(|| format!("batch missing column `{name}`"))?;
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("column `{name}` is not Utf8"))
    };
    let get_u64 = |name: &str| -> Result<&UInt64Array> {
        let idx = schema
            .index_of(name)
            .with_context(|| format!("batch missing column `{name}`"))?;
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| anyhow!("column `{name}` is not UInt64"))
    };

    let slot = get_str("slot")?;
    let source_type = get_str("source_type")?;
    let path = get_str("path")?;
    let package = get_str("package")?;
    let fqn = get_str("fqn")?;
    let filename = get_str("filename")?;
    let symbol = get_str("symbol")?;
    let chunk_kind = get_str("chunk_kind")?;
    let start_line = get_u64("start_line")?;
    let end_line = get_u64("end_line")?;
    let line_count = get_u64("line_count")?;

    // LanceDB appends a `_distance` column on vector search results. If
    // it's missing (plain scan), fill 0.0 so the hit type stays uniform.
    let distance: Option<&Float32Array> = schema.index_of("_distance").ok().and_then(|i| {
        batch.column(i).as_any().downcast_ref::<Float32Array>()
    });

    for row in 0..batch.num_rows() {
        out.push(SemanticHit {
            slot: slot.value(row).to_string(),
            source_type: source_type.value(row).to_string(),
            path: path.value(row).to_string(),
            package: package.value(row).to_string(),
            fqn: fqn.value(row).to_string(),
            filename: filename.value(row).to_string(),
            symbol: symbol.value(row).to_string(),
            chunk_kind: chunk_kind.value(row).to_string(),
            start_line: start_line.value(row),
            end_line: end_line.value(row),
            line_count: line_count.value(row),
            distance: distance.map(|a| a.value(row)).unwrap_or(0.0),
        });
    }
    Ok(())
}

/// Delete the on-disk Lance directory for a slot. Mirrors
/// [`crate::indexer::clear_slot`] for symmetry - callers wipe both
/// stores together.
pub fn clear_slot(lance_dir: &Path) -> std::io::Result<()> {
    if lance_dir.is_dir() {
        std::fs::remove_dir_all(lance_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_384_dim_embedding() {
        let s = chunk_schema();
        let field = s.field_with_name("embedding").unwrap();
        match field.data_type() {
            DataType::FixedSizeList(inner, dim) => {
                assert_eq!(*dim, EMBEDDING_DIM as i32);
                assert_eq!(inner.data_type(), &DataType::Float32);
            }
            other => panic!("embedding should be FixedSizeList, got {other:?}"),
        }
    }

    #[test]
    fn batch_roundtrips_rows() {
        let emb = vec![0.1_f32; EMBEDDING_DIM];
        let rows = vec![ChunkRow {
            slot: "release",
            source_type: "source",
            path: "Foo.java",
            package: "com.x",
            fqn: "com.x.Foo",
            filename: "Foo.java",
            symbol: "Foo",
            chunk_kind: "type",
            start_line: 1,
            end_line: 20,
            line_count: 30,
            embedding: &emb,
        }];
        let batch = batch_from_rows(&rows).unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), chunk_schema().fields().len());
    }
}
