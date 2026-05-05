//! SQLite-backed structural symbol sidecar.
//!
//! Lives at `<index_dir>/symbols.sqlite` next to `tantivy/` and `lance/`.
//! Written once per indexer run, read later by:
//!
//! - **diff tracker** - resolves a user's mod's API references
//!   (imports, qualified method calls, field reads) against an older build
//!   to report what changed. Needs cheap `(class_fqn, name, param_types)`
//!   lookups, which is painful in JSON and natural in SQL.
//! - **`find_symbol` MCP tool** - fuzzy symbol search for agents.
//!   Uses FTS5 over `methods_fts` so a query like `"getComponent PageManager"`
//!   ranks matches across class + method name.
//!
//! The DB is write-once-per-run: the indexer deletes any previous file at
//! the start of `build_index`, creates a fresh schema, streams symbols in
//! within a single transaction, and commits. No incremental updates - if
//! the decompile changes, we rebuild from scratch, same as Tantivy + Lance.
//!
//! ## Schema shape
//!
//! Three tables mirroring the three symbol types emitted by the chunker:
//!
//! - `classes` - one row per class/interface/enum/record/annotation. Primary
//!   key is the dotted FQN so nested classes get a unique key without extra
//!   joining (`com.foo.Outer.Inner`).
//! - `methods` - one row per method and constructor. Keyed on an autoincrement
//!   `id` so two overloads can coexist. Lookup index on `(class_fqn, name)`.
//! - `fields` - one row per field/enum constant.
//!
//! One FTS5 virtual table (`methods_fts`) gives the `find_symbol` MCP tool a
//! ranked fuzzy search over method signatures. We populate it in the same
//! transaction as `methods`; because the indexer never updates individual
//! rows, we skip FTS triggers - fresh build each time.
//!
//! Modifier/interface/param lists are stored as JSON arrays in TEXT columns.
//! Keeps the schema flat (no N junction tables) at the cost of slightly
//! harder querying - fine because diff-tracker queries hit the flat columns
//! (FQN, name, param_types) and modifier lists only matter for display.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};

use super::chunker::{ClassSymbol, FieldSymbol, FileSymbols, MethodSymbol};

/// Owning handle to the symbols SQLite file. Not `Send` by itself - we
/// deliberately keep the connection single-threaded and construct it inside
/// the synchronous indexer worker, same place Tantivy + Lance writes happen.
pub struct SymbolsDb {
    conn: Connection,
}

impl SymbolsDb {
    /// Create a fresh DB at `path`, overwriting any existing file. The
    /// schema is applied immediately; returned handle is ready for writes.
    ///
    /// The indexer already wipes the entire `<index_dir>` at the start of
    /// `build_index`, so in practice the file won't exist on call. We still
    /// defensively remove it first so invariants hold if the caller order
    /// changes.
    pub fn create(path: &Path) -> Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)
                .with_context(|| format!("removing stale {}", path.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("creating symbols db at {}", path.display()))?;

        // Pragmas tuned for the write-once pattern. Off-the-shelf defaults
        // are safe but slow: WAL + memory temp store + synchronous=NORMAL
        // cuts bulk-insert wall time roughly in half without risking data
        // loss for our use case (we re-run the whole indexer on failure).
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA temp_store = MEMORY;
            PRAGMA cache_size = -64000;
            ",
        )
        .context("tuning pragmas on fresh symbols db")?;

        apply_schema(&conn).context("applying symbols schema")?;
        Ok(Self { conn })
    }

    /// Open an existing symbols DB read-only. Used by the diff tracker
    /// and MCP `find_symbol` tool - neither mutates the file.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening symbols db read-only at {}", path.display()))?;
        Ok(Self { conn })
    }

    /// Begin a write transaction for bulk inserts. Caller streams
    /// per-file symbols into it and commits at the end.
    pub fn begin_write(&mut self) -> Result<WriteTx<'_>> {
        let tx = self
            .conn
            .transaction()
            .context("starting symbols write transaction")?;
        Ok(WriteTx { tx })
    }

    /// Look up symbols by fully-qualified name. Returns up to `limit`
    /// matches across classes / methods / fields, optionally filtered
    /// by [`SymbolKind`]. Powers the `find_symbol` MCP tool's exact-match
    /// path.
    pub fn find_by_fqn(
        &self,
        fqn: &str,
        kind: Option<SymbolKind>,
        limit: usize,
    ) -> Result<Vec<SymbolHit>> {
        let mut out = Vec::new();
        let want_class = matches!(kind, None | Some(SymbolKind::Class));
        let want_method = matches!(kind, None | Some(SymbolKind::Method));
        let want_field = matches!(kind, None | Some(SymbolKind::Field));

        // Classes match on their own FQN. Methods/fields keyed on
        // `class_fqn.name`.
        if want_class {
            let mut stmt = self.conn.prepare(
                "SELECT fqn, simple_name, modifiers, rel_path, start_line, end_line \
                 FROM classes WHERE fqn = ?1 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![fqn, limit as i64], |r| {
                Ok(SymbolHit {
                    kind: SymbolKind::Class,
                    fqn: r.get::<_, String>(0)?,
                    signature: None,
                    declaring_class: None,
                    modifiers: parse_json_array(&r.get::<_, String>(2)?),
                    rel_path: r.get(3)?,
                    start_line: r.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u64),
                    end_line: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                })
            })?;
            for row in rows.flatten() {
                out.push(row);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }

        let (parent, name) = split_fqn(fqn);
        if let (Some(parent), Some(name)) = (parent, name) {
            if want_method {
                let mut stmt = self.conn.prepare(
                    "SELECT m.class_fqn, m.name, m.modifiers, m.return_type, \
                            m.param_types, c.rel_path, m.start_line, m.end_line \
                     FROM methods m \
                     LEFT JOIN classes c ON c.fqn = m.class_fqn \
                     WHERE m.class_fqn = ?1 AND m.name = ?2 LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![parent, name, limit as i64], |r| {
                    let class_fqn: String = r.get(0)?;
                    let mname: String = r.get(1)?;
                    let return_type: Option<String> = r.get(3)?;
                    let param_types: String = r.get(4)?;
                    Ok(SymbolHit {
                        kind: SymbolKind::Method,
                        fqn: format!("{}.{}", class_fqn, mname),
                        signature: Some(format_signature(
                            &return_type,
                            &parse_json_array(&param_types),
                        )),
                        declaring_class: Some(class_fqn),
                        modifiers: parse_json_array(&r.get::<_, String>(2)?),
                        rel_path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        start_line: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
                        end_line: r.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
                    })
                })?;
                for row in rows.flatten() {
                    out.push(row);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
            if want_field {
                let mut stmt = self.conn.prepare(
                    "SELECT f.class_fqn, f.name, f.modifiers, f.type_text, \
                            c.rel_path, f.start_line, f.end_line \
                     FROM fields f \
                     LEFT JOIN classes c ON c.fqn = f.class_fqn \
                     WHERE f.class_fqn = ?1 AND f.name = ?2 LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![parent, name, limit as i64], |r| {
                    let class_fqn: String = r.get(0)?;
                    let fname: String = r.get(1)?;
                    let type_text: String = r.get(3)?;
                    Ok(SymbolHit {
                        kind: SymbolKind::Field,
                        fqn: format!("{}.{}", class_fqn, fname),
                        signature: Some(type_text),
                        declaring_class: Some(class_fqn),
                        modifiers: parse_json_array(&r.get::<_, String>(2)?),
                        rel_path: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        start_line: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                        end_line: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
                    })
                })?;
                for row in rows.flatten() {
                    out.push(row);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }

        Ok(out)
    }

    /// FTS5 fuzzy search over method signatures. Pass raw user tokens -
    /// we escape them as MATCH terms. Used by the `find_symbol` MCP tool's
    /// `signature` path.
    pub fn find_by_signature(&self, query: &str, limit: usize) -> Result<Vec<SymbolHit>> {
        let escaped = escape_fts_query(query);
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT m.class_fqn, m.name, m.modifiers, m.return_type, \
                    m.param_types, c.rel_path, m.start_line, m.end_line \
             FROM methods_fts fts \
             JOIN methods m ON m.id = fts.rowid \
             LEFT JOIN classes c ON c.fqn = m.class_fqn \
             WHERE methods_fts MATCH ?1 \
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![escaped, limit as i64], |r| {
            let class_fqn: String = r.get(0)?;
            let name: String = r.get(1)?;
            let return_type: Option<String> = r.get(3)?;
            let param_types: String = r.get(4)?;
            Ok(SymbolHit {
                kind: SymbolKind::Method,
                fqn: format!("{}.{}", class_fqn, name),
                signature: Some(format_signature(
                    &return_type,
                    &parse_json_array(&param_types),
                )),
                declaring_class: Some(class_fqn),
                modifiers: parse_json_array(&r.get::<_, String>(2)?),
                rel_path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                start_line: r.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u64),
                end_line: r.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// All methods declared on `class_fqn`, with simple-typed parameter
    /// lists ready to match against Javadoc-side method docs. Used by
    /// the inline-Javadoc resolver to pair each Javadoc method to its
    /// source line so the inline card lands above the right
    /// declaration. Constructors are excluded - Javadoc constructor
    /// headings are class-name-shaped and the inline anchor only
    /// targets methods.
    pub fn methods_for_class(&self, class_fqn: &str) -> Result<Vec<MethodRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, param_types, start_line, end_line, is_constructor \
             FROM methods WHERE class_fqn = ?1 ORDER BY start_line ASC",
        )?;
        let rows = stmt.query_map(params![class_fqn], |r| {
            let name: String = r.get(0)?;
            let param_types_json: String = r.get(1)?;
            let start_line: i64 = r.get(2)?;
            let end_line: i64 = r.get(3)?;
            let is_constructor: i64 = r.get(4)?;
            Ok(MethodRow {
                name,
                param_simple_types: parse_json_array(&param_types_json)
                    .into_iter()
                    .map(|t| crate::indexer::hypixel_docs::simple_type_name(&t))
                    .collect(),
                start_line: start_line.max(0) as u32,
                end_line: end_line.max(0) as u32,
                is_constructor: is_constructor != 0,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// Diff-tracker accessor: class modifiers list for the given FQN, or
    /// `None` if the class isn't in this snapshot. Used to detect
    /// `@Deprecated` annotations and other modifier-level changes.
    pub fn class_modifiers(&self, class_fqn: &str) -> Result<Option<Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT modifiers FROM classes WHERE fqn = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![class_fqn])?;
        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            return Ok(Some(parse_json_array(&json)));
        }
        Ok(None)
    }

    /// Diff-tracker accessor: every method overload matching
    /// `(class_fqn, name)`, with full modifiers / return / param info.
    /// Empty Vec means "no method by this name on this class".
    pub fn methods_by_name(&self, class_fqn: &str, name: &str) -> Result<Vec<DiffMethodRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT modifiers, return_type, param_types, is_constructor \
             FROM methods WHERE class_fqn = ?1 AND name = ?2",
        )?;
        let rows = stmt.query_map(params![class_fqn, name], |r| {
            Ok(DiffMethodRow {
                modifiers: parse_json_array(&r.get::<_, String>(0)?),
                return_type: r.get::<_, Option<String>>(1)?,
                param_types: parse_json_array(&r.get::<_, String>(2)?),
                is_constructor: r.get::<_, i64>(3)? != 0,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    /// Diff-tracker accessor: distinct method names declared on a class.
    /// Powers the "renamed_likely" heuristic - we Levenshtein-distance
    /// the user's referenced name against this list to suggest renames.
    pub fn method_names_on_class(&self, class_fqn: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT name FROM methods WHERE class_fqn = ?1")?;
        let rows = stmt.query_map(params![class_fqn], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Diff-tracker accessor: a field row by `(class_fqn, name)`, or
    /// `None` if the field isn't declared on that class.
    pub fn field_by_name(&self, class_fqn: &str, name: &str) -> Result<Option<DiffFieldRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT modifiers, type_text \
             FROM fields WHERE class_fqn = ?1 AND name = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![class_fqn, name])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(DiffFieldRow {
                modifiers: parse_json_array(&row.get::<_, String>(0)?),
                type_text: row.get::<_, String>(1)?,
            }));
        }
        Ok(None)
    }

    /// Diff-tracker accessor: distinct field names declared on a class.
    pub fn field_names_on_class(&self, class_fqn: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT name FROM fields WHERE class_fqn = ?1")?;
        let rows = stmt.query_map(params![class_fqn], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Cross-build compare accessor: every class FQN in this snapshot.
    /// Used by `index_compare` to compute the symmetric difference between
    /// two builds at the class level. Returned as a sorted list so the
    /// frontend can render diffs deterministically.
    pub fn all_class_fqns(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT fqn FROM classes ORDER BY fqn")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Number of rows across all three tables - useful for sanity checks
    /// and the manifest summary.
    pub fn row_counts(&self) -> Result<RowCounts> {
        let classes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM classes", [], |r| r.get(0))?;
        let methods: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM methods", [], |r| r.get(0))?;
        let fields: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fields", [], |r| r.get(0))?;
        Ok(RowCounts {
            classes: classes.max(0) as u64,
            methods: methods.max(0) as u64,
            fields: fields.max(0) as u64,
        })
    }
}

/// Scoped write transaction. Dropped without `commit()` rolls back.
pub struct WriteTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl<'a> WriteTx<'a> {
    /// Insert all symbols extracted from one source file. `rel_path` is the
    /// decompile-relative path (e.g. `com/hypixel/foo/Bar.java`) - stored
    /// on the `classes` table so the IDE-open and "jump to source" paths
    /// don't need to reconstruct it from FQN + package conventions.
    pub fn insert_file(&self, rel_path: &str, symbols: &FileSymbols) -> Result<()> {
        for c in &symbols.classes {
            insert_class(&self.tx, rel_path, c)?;
        }
        for m in &symbols.methods {
            insert_method(&self.tx, m)?;
        }
        for f in &symbols.fields {
            insert_field(&self.tx, f)?;
        }
        Ok(())
    }

    /// Commit the transaction. Returns the underlying connection intact
    /// inside `SymbolsDb`, ready for more writes or `row_counts()`.
    pub fn commit(self) -> Result<()> {
        self.tx.commit().context("committing symbols transaction")
    }
}

/// Counts surfaced to the manifest and the Index Catalog UX.
#[derive(Debug, Clone, Copy)]
pub struct RowCounts {
    pub classes: u64,
    pub methods: u64,
    pub fields: u64,
}

/// Output row for [`SymbolsDb::methods_for_class`]. Carries enough info
/// for the inline-Javadoc resolver to pair a Javadoc method to its
/// source line; `param_simple_types` is already reduced via
/// [`crate::indexer::hypixel_docs::simple_type_name`] so callers can
/// equality-match against the Javadoc side without re-reducing.
#[derive(Debug, Clone)]
pub struct MethodRow {
    pub name: String,
    pub param_simple_types: Vec<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub is_constructor: bool,
}

/// Output row for [`SymbolsDb::methods_by_name`]. Used by the diff
/// tracker; carries full modifier / signature info so signature changes
/// and `@Deprecated` additions can be detected.
#[derive(Debug, Clone)]
pub struct DiffMethodRow {
    pub modifiers: Vec<String>,
    pub return_type: Option<String>,
    pub param_types: Vec<String>,
    pub is_constructor: bool,
}

/// Output row for [`SymbolsDb::field_by_name`]. Diff-tracker only.
#[derive(Debug, Clone)]
pub struct DiffFieldRow {
    pub modifiers: Vec<String>,
    pub type_text: String,
}

/// Kind discriminator returned by [`SymbolsDb::find_by_fqn`] /
/// [`SymbolsDb::find_by_signature`] and mirrored in the MCP
/// `find_symbol` tool output (`docs/mcp-contract.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Method,
    Field,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Method => "method",
            SymbolKind::Field => "field",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "class" => Some(SymbolKind::Class),
            "method" => Some(SymbolKind::Method),
            "field" => Some(SymbolKind::Field),
            _ => None,
        }
    }
}

/// Row returned to the MCP layer. Flat, JSON-shape-compatible with the
/// `matches[]` schema in `docs/mcp-contract.md`.
#[derive(Debug, Clone)]
pub struct SymbolHit {
    pub kind: SymbolKind,
    pub fqn: String,
    pub signature: Option<String>,
    pub declaring_class: Option<String>,
    pub modifiers: Vec<String>,
    pub rel_path: String,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
}

fn parse_json_array(s: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
}

/// Render `(param1, param2) -> ret` for display. `ret` is omitted when
/// the method is a constructor (return_type is NULL in the schema).
fn format_signature(return_type: &Option<String>, params: &[String]) -> String {
    let params_part = format!("({})", params.join(", "));
    match return_type {
        Some(rt) => format!("{params_part} -> {rt}"),
        None => params_part,
    }
}

/// Split `foo.bar.Baz.method` → (`foo.bar.Baz`, `method`). Returns
/// (None, None) for dot-less input.
fn split_fqn(fqn: &str) -> (Option<&str>, Option<&str>) {
    match fqn.rsplit_once('.') {
        Some((parent, name)) => (Some(parent), Some(name)),
        None => (None, None),
    }
}

/// Build a safe FTS5 MATCH expression from raw user input. Tokens with
/// non-alphanumeric characters get double-quoted so FTS5 doesn't
/// interpret them as operators; terms are space-joined (implicit AND).
fn escape_fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .filter_map(|tok| {
            let cleaned: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE classes (
            fqn         TEXT NOT NULL PRIMARY KEY,
            simple_name TEXT NOT NULL,
            kind        TEXT NOT NULL,
            modifiers   TEXT NOT NULL,   -- JSON array
            superclass  TEXT,
            interfaces  TEXT NOT NULL,   -- JSON array
            rel_path    TEXT NOT NULL,
            start_line  INTEGER NOT NULL,
            end_line    INTEGER NOT NULL
        );
        CREATE INDEX idx_classes_simple_name ON classes(simple_name);
        CREATE INDEX idx_classes_rel_path    ON classes(rel_path);

        CREATE TABLE methods (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            class_fqn      TEXT NOT NULL,
            name           TEXT NOT NULL,
            is_constructor INTEGER NOT NULL,
            modifiers      TEXT NOT NULL,
            return_type    TEXT,
            param_types    TEXT NOT NULL,
            thrown         TEXT NOT NULL,
            start_line     INTEGER NOT NULL,
            end_line       INTEGER NOT NULL
        );
        CREATE INDEX idx_methods_class_name ON methods(class_fqn, name);

        CREATE TABLE fields (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            class_fqn   TEXT NOT NULL,
            name        TEXT NOT NULL,
            type_text   TEXT NOT NULL,
            modifiers   TEXT NOT NULL,
            start_line  INTEGER NOT NULL,
            end_line    INTEGER NOT NULL
        );
        CREATE INDEX idx_fields_class_name ON fields(class_fqn, name);

        -- External-content FTS5 table over the methods table. We populate
        -- it manually in the same transaction (no triggers) because the
        -- indexer is write-once-per-run.
        CREATE VIRTUAL TABLE methods_fts USING fts5(
            class_fqn, name, param_types, return_type,
            content='methods', content_rowid='id'
        );
        ",
    )
    .context("creating symbols tables + indexes")?;
    Ok(())
}

fn insert_class(tx: &rusqlite::Transaction<'_>, rel_path: &str, c: &ClassSymbol) -> Result<()> {
    // Duplicates can appear when two files declare types at the same FQN
    // (unlikely in decompiled source but possible with package-private
    // helpers). `INSERT OR REPLACE` keeps the last one seen - consistent
    // with Tantivy's behavior, which also naturally takes the most recent
    // write per docid.
    tx.execute(
        "INSERT OR REPLACE INTO classes
            (fqn, simple_name, kind, modifiers, superclass, interfaces,
             rel_path, start_line, end_line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            c.fqn,
            c.simple_name,
            c.kind.as_str(),
            serde_json::to_string(&c.modifiers).unwrap_or_else(|_| "[]".to_string()),
            c.superclass,
            serde_json::to_string(&c.interfaces).unwrap_or_else(|_| "[]".to_string()),
            rel_path,
            c.start_line as i64,
            c.end_line as i64,
        ],
    )
    .with_context(|| format!("inserting class {}", c.fqn))?;
    Ok(())
}

fn insert_method(tx: &rusqlite::Transaction<'_>, m: &MethodSymbol) -> Result<()> {
    tx.execute(
        "INSERT INTO methods
            (class_fqn, name, is_constructor, modifiers, return_type,
             param_types, thrown, start_line, end_line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            m.class_fqn,
            m.name,
            m.is_constructor as i64,
            serde_json::to_string(&m.modifiers).unwrap_or_else(|_| "[]".to_string()),
            m.return_type,
            serde_json::to_string(&m.param_types).unwrap_or_else(|_| "[]".to_string()),
            serde_json::to_string(&m.thrown).unwrap_or_else(|_| "[]".to_string()),
            m.start_line as i64,
            m.end_line as i64,
        ],
    )
    .with_context(|| format!("inserting method {}::{}", m.class_fqn, m.name))?;

    // Mirror into the FTS5 table. Insert at the same rowid as `methods`
    // so an eventual `MATCH` can join back. `last_insert_rowid` is cheap.
    let rowid = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO methods_fts (rowid, class_fqn, name, param_types, return_type)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            rowid,
            m.class_fqn,
            m.name,
            m.param_types.join(", "),
            m.return_type,
        ],
    )
    .context("inserting into methods_fts")?;

    Ok(())
}

fn insert_field(tx: &rusqlite::Transaction<'_>, f: &FieldSymbol) -> Result<()> {
    tx.execute(
        "INSERT INTO fields
            (class_fqn, name, type_text, modifiers, start_line, end_line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            f.class_fqn,
            f.name,
            f.type_text,
            serde_json::to_string(&f.modifiers).unwrap_or_else(|_| "[]".to_string()),
            f.start_line as i64,
            f.end_line as i64,
        ],
    )
    .with_context(|| format!("inserting field {}::{}", f.class_fqn, f.name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::chunker::{ClassSymbol, FieldSymbol, FileSymbols, MethodSymbol, TypeKind};
    use tempfile::tempdir;

    fn sample_symbols() -> FileSymbols {
        FileSymbols {
            classes: vec![ClassSymbol {
                fqn: "com.example.Foo".to_string(),
                simple_name: "Foo".to_string(),
                kind: TypeKind::Class,
                modifiers: vec!["public".to_string()],
                superclass: None,
                interfaces: vec!["Bar".to_string()],
                start_line: 1,
                end_line: 10,
            }],
            methods: vec![
                MethodSymbol {
                    class_fqn: "com.example.Foo".to_string(),
                    name: "doThing".to_string(),
                    is_constructor: false,
                    modifiers: vec!["public".to_string()],
                    return_type: Some("int".to_string()),
                    param_types: vec!["String".to_string(), "int".to_string()],
                    thrown: vec![],
                    start_line: 3,
                    end_line: 5,
                },
                MethodSymbol {
                    class_fqn: "com.example.Foo".to_string(),
                    name: "Foo".to_string(),
                    is_constructor: true,
                    modifiers: vec!["public".to_string()],
                    return_type: None,
                    param_types: vec![],
                    thrown: vec![],
                    start_line: 2,
                    end_line: 2,
                },
            ],
            fields: vec![FieldSymbol {
                class_fqn: "com.example.Foo".to_string(),
                name: "counter".to_string(),
                type_text: "int".to_string(),
                modifiers: vec!["private".to_string()],
                start_line: 7,
                end_line: 7,
            }],
        }
    }

    #[test]
    fn create_writes_and_commits_a_file_of_symbols() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();

        let tx = db.begin_write().unwrap();
        tx.insert_file("com/example/Foo.java", &sample_symbols())
            .unwrap();
        tx.commit().unwrap();

        let counts = db.row_counts().unwrap();
        assert_eq!(counts.classes, 1);
        assert_eq!(counts.methods, 2);
        assert_eq!(counts.fields, 1);
    }

    #[test]
    fn read_only_reopens_and_queries_existing_db() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");

        {
            let mut db = SymbolsDb::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            tx.insert_file("com/example/Foo.java", &sample_symbols())
                .unwrap();
            tx.commit().unwrap();
        }

        let db = SymbolsDb::open_read_only(&path).unwrap();
        let counts = db.row_counts().unwrap();
        assert_eq!(counts.methods, 2);
    }

    #[test]
    fn fts_lookup_finds_method_by_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.insert_file("com/example/Foo.java", &sample_symbols())
            .unwrap();
        tx.commit().unwrap();

        // Direct FTS query - not the final MCP surface, but proves the
        // virtual table got populated.
        let mut stmt = db
            .conn
            .prepare(
                "SELECT class_fqn, name FROM methods_fts
                 WHERE methods_fts MATCH ? ORDER BY rank LIMIT 5",
            )
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map(params!["doThing"], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            rows.iter()
                .any(|(c, n)| c == "com.example.Foo" && n == "doThing"),
            "expected doThing hit, got {rows:?}"
        );
    }

    #[test]
    fn duplicate_class_fqn_replaces_row() {
        // Decompiled source rarely has this but the schema tolerates it.
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();
        let tx = db.begin_write().unwrap();
        tx.insert_file("com/example/Foo.java", &sample_symbols())
            .unwrap();
        // Re-insert the same class FQN with a different file path.
        let mut sym = sample_symbols();
        sym.classes[0].end_line = 999;
        tx.insert_file("com/example/AltFoo.java", &sym).unwrap();
        tx.commit().unwrap();

        let row: (String, i64) = db
            .conn
            .query_row(
                "SELECT rel_path, end_line FROM classes WHERE fqn = ?1",
                params!["com.example.Foo"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // Last writer wins.
        assert_eq!(row.0, "com/example/AltFoo.java");
        assert_eq!(row.1, 999);
    }
}
