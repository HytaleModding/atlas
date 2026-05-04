//! Tantivy schema for the decompile index.
//!
//! One document per **chunk** (not per file). The walker + chunker emit zero
//! or more chunks per `.java` file; each chunk becomes its own Tantivy
//! document. The frontend collapses multiple chunk-level hits back down to
//! one row per file, so the UI still reads as "file results" even though
//! ranking happens at chunk granularity.
//!
//! Field design:
//!   slot         - STRING | STORED - filter on release vs pre-release
//!   source_type  - STRING | STORED - "source" | "hm_doc" | "hypixel_doc" | "asset"
//!                                    multi-source discriminator.
//!   path         - STRING | STORED - relative path (used for dedup + read_source)
//!   package      - TEXT (code) | STORED - dotted Java package, tokenized per dot
//!   fqn          - TEXT (code) | STORED - `package.ClassName`, tokenized
//!   filename     - TEXT (code) | STORED - class name stem (no .java)
//!   symbol       - TEXT (code) | STORED - the chunk's symbol name (class or method)
//!   chunk_kind   - STRING | STORED - "type" | "method" | "file"
//!   start_line   - U64 | STORED - 1-based inclusive
//!   end_line     - U64 | STORED - 1-based inclusive
//!   line_count   - U64 | STORED - file total line count (for UI readout)
//!   content      - TEXT (code) - chunk body, not stored (we reconstitute from disk)

use tantivy::schema::{
    IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, FAST, INDEXED,
    STORED, STRING,
};

use super::analyzer::CODE_TOKENIZER;

/// Discriminator for the kind of content in a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Source,
    HmDoc,
    HypixelDoc,
    Asset,
}

impl SourceType {
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceType::Source => "source",
            SourceType::HmDoc => "hm_doc",
            SourceType::HypixelDoc => "hypixel_doc",
            SourceType::Asset => "asset",
        }
    }

    /// Parse a string back to a [`SourceType`]. Returns `None` for unknown
    /// values so callers can treat them as partial-availability instead of
    /// hard-erroring.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "source" => Some(SourceType::Source),
            "hm_doc" => Some(SourceType::HmDoc),
            "hypixel_doc" => Some(SourceType::HypixelDoc),
            "asset" => Some(SourceType::Asset),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IndexFields {
    pub slot: tantivy::schema::Field,
    pub source_type: tantivy::schema::Field,
    pub path: tantivy::schema::Field,
    pub package: tantivy::schema::Field,
    pub fqn: tantivy::schema::Field,
    pub filename: tantivy::schema::Field,
    pub symbol: tantivy::schema::Field,
    pub chunk_kind: tantivy::schema::Field,
    pub start_line: tantivy::schema::Field,
    pub end_line: tantivy::schema::Field,
    pub line_count: tantivy::schema::Field,
    pub content: tantivy::schema::Field,
}

pub fn build() -> (Schema, IndexFields) {
    let mut builder: SchemaBuilder = Schema::builder();

    let code_indexing = TextFieldIndexing::default()
        .set_tokenizer(CODE_TOKENIZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let code_stored: TextOptions = TextOptions::default()
        .set_indexing_options(code_indexing.clone())
        .set_stored();
    let code_only: TextOptions = TextOptions::default().set_indexing_options(code_indexing);

    let slot = builder.add_text_field("slot", STRING | STORED);
    let source_type = builder.add_text_field("source_type", STRING | STORED);
    let path = builder.add_text_field("path", STRING | STORED);
    let package = builder.add_text_field("package", code_stored.clone());
    let fqn = builder.add_text_field("fqn", code_stored.clone());
    let filename = builder.add_text_field("filename", code_stored.clone());
    let symbol = builder.add_text_field("symbol", code_stored);
    let chunk_kind = builder.add_text_field("chunk_kind", STRING | STORED);
    let start_line = builder.add_u64_field("start_line", STORED | INDEXED | FAST);
    let end_line = builder.add_u64_field("end_line", STORED | INDEXED | FAST);
    let line_count = builder.add_u64_field("line_count", STORED | INDEXED | FAST);
    let content = builder.add_text_field("content", code_only);

    let schema = builder.build();
    (
        schema,
        IndexFields {
            slot,
            source_type,
            path,
            package,
            fqn,
            filename,
            symbol,
            chunk_kind,
            start_line,
            end_line,
            line_count,
            content,
        },
    )
}
