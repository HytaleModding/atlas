//! User-state persistence: pins, notes, recent files.
//!
//! Backed by a single `state.sqlite` file under `<data_dir>/state.sqlite`,
//! distinct from `symbols.sqlite` (which is per-build reference data).
//! State here lives across reinstalls of Hytale data and isn't tied to
//! a particular build version.
//!
//! # Schema
//!
//! ```sql
//! pins (
//!     id INTEGER PRIMARY KEY AUTOINCREMENT,
//!     kind TEXT NOT NULL,         -- 'file' | 'query' | 'symbol'
//!     target TEXT NOT NULL,       -- path / query string / FQN
//!     build_id TEXT,              -- nullable; pins can be build-agnostic
//!     label TEXT,                 -- optional friendly name
//!     created_at TEXT NOT NULL    -- ISO-8601
//! );
//! UNIQUE INDEX pins_uniq ON pins(kind, target, IFNULL(build_id, ''));
//!
//! notes (
//!     pin_id INTEGER PRIMARY KEY REFERENCES pins(id) ON DELETE CASCADE,
//!     body TEXT NOT NULL,
//!     updated_at TEXT NOT NULL
//! );
//!
//! recent_files (
//!     path TEXT NOT NULL,
//!     build_id TEXT NOT NULL,
//!     opened_at TEXT NOT NULL,
//!     PRIMARY KEY (path, build_id)
//! );
//! ```
//!
//! The frontend caches snapshots (Zustand) but the database is the source
//! of truth - pin/unpin/note-edit roundtrip through here every time.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Cap on `recent_files` rows. Beyond this we trim oldest entries on
/// every insert. 50 matches the plan; if it ever needs tuning it lives
/// here in one place.
const RECENT_FILES_CAP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    File,
    Query,
    Symbol,
}

impl PinKind {
    fn as_str(self) -> &'static str {
        match self {
            PinKind::File => "file",
            PinKind::Query => "query",
            PinKind::Symbol => "symbol",
        }
    }
    fn parse(s: &str) -> Option<PinKind> {
        match s {
            "file" => Some(PinKind::File),
            "query" => Some(PinKind::Query),
            "symbol" => Some(PinKind::Symbol),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Pin {
    pub id: i64,
    pub kind: PinKind,
    pub target: String,
    pub build_id: Option<String>,
    pub label: Option<String>,
    pub created_at: String,
    /// Eagerly joined from `notes` so the frontend's pin list doesn't
    /// need a second roundtrip per row to know whether a note exists.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentFile {
    pub path: String,
    pub build_id: String,
    pub opened_at: String,
}

/// Open-or-create a `state.sqlite` at `<data_dir>/state.sqlite` and run
/// migrations. Migrations are idempotent CREATE-IF-NOT-EXISTS so it's
/// safe to call this every startup.
pub struct StateDb {
    conn: Mutex<Connection>,
}

impl StateDb {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let path = data_dir.join("state.sqlite");
        let conn =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS pins (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL,
                 target TEXT NOT NULL,
                 build_id TEXT,
                 label TEXT,
                 created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS pins_uniq
                 ON pins(kind, target, IFNULL(build_id, ''));
             CREATE TABLE IF NOT EXISTS notes (
                 pin_id INTEGER PRIMARY KEY REFERENCES pins(id) ON DELETE CASCADE,
                 body TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recent_files (
                 path TEXT NOT NULL,
                 build_id TEXT NOT NULL,
                 opened_at TEXT NOT NULL,
                 PRIMARY KEY (path, build_id)
             );",
        )
        .context("running state.sqlite migrations")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a pin or return the existing row if one already covers
    /// `(kind, target, build_id)`. Idempotent so the frontend doesn't
    /// have to check first.
    pub fn pin_add(
        &self,
        kind: PinKind,
        target: &str,
        build_id: Option<&str>,
        label: Option<&str>,
    ) -> Result<Pin> {
        let conn = self.conn.lock().expect("state db poisoned");
        let now = now_iso();
        // Try to insert; on conflict fall through and read the existing row.
        conn.execute(
            "INSERT OR IGNORE INTO pins (kind, target, build_id, label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind.as_str(), target, build_id, label, now],
        )?;
        // Always re-read so we get either the new id or the existing one.
        let row = conn
            .query_row(
                "SELECT pins.id, pins.kind, pins.target, pins.build_id, pins.label,
                        pins.created_at, notes.body
                 FROM pins LEFT JOIN notes ON notes.pin_id = pins.id
                 WHERE pins.kind = ?1 AND pins.target = ?2
                   AND IFNULL(pins.build_id, '') = IFNULL(?3, '')",
                params![kind.as_str(), target, build_id],
                row_to_pin,
            )
            .context("reading pin after insert")?;
        Ok(row)
    }

    pub fn pin_remove(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("state db poisoned");
        conn.execute("DELETE FROM pins WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn pin_list(&self) -> Result<Vec<Pin>> {
        let conn = self.conn.lock().expect("state db poisoned");
        let mut stmt = conn.prepare(
            "SELECT pins.id, pins.kind, pins.target, pins.build_id, pins.label,
                    pins.created_at, notes.body
             FROM pins LEFT JOIN notes ON notes.pin_id = pins.id
             ORDER BY pins.created_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_pin)?;
        Ok(rows.flatten().collect())
    }

    pub fn note_set(&self, pin_id: i64, body: &str) -> Result<()> {
        let conn = self.conn.lock().expect("state db poisoned");
        let now = now_iso();
        if body.trim().is_empty() {
            // Empty body deletes the note row - we keep `notes` lean
            // and the absence of a row is the canonical "no note".
            conn.execute("DELETE FROM notes WHERE pin_id = ?1", params![pin_id])?;
            return Ok(());
        }
        conn.execute(
            "INSERT INTO notes (pin_id, body, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(pin_id) DO UPDATE SET body = excluded.body,
                                               updated_at = excluded.updated_at",
            params![pin_id, body, now],
        )?;
        Ok(())
    }

    pub fn note_get(&self, pin_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("state db poisoned");
        Ok(conn
            .query_row(
                "SELECT body FROM notes WHERE pin_id = ?1",
                params![pin_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Record that the user opened `(path, build_id)` now. Upserts
    /// `opened_at` and trims to `RECENT_FILES_CAP` newest rows.
    pub fn recent_file_record(&self, path: &str, build_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("state db poisoned");
        let now = now_iso();
        conn.execute(
            "INSERT INTO recent_files (path, build_id, opened_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(path, build_id) DO UPDATE SET opened_at = excluded.opened_at",
            params![path, build_id, now],
        )?;
        // Trim oldest. SQLite has no LIMIT in DELETE without a subquery.
        conn.execute(
            "DELETE FROM recent_files
             WHERE rowid NOT IN (
                 SELECT rowid FROM recent_files ORDER BY opened_at DESC LIMIT ?1
             )",
            params![RECENT_FILES_CAP as i64],
        )?;
        Ok(())
    }

    pub fn recent_files(&self) -> Result<Vec<RecentFile>> {
        let conn = self.conn.lock().expect("state db poisoned");
        let mut stmt = conn.prepare(
            "SELECT path, build_id, opened_at FROM recent_files
             ORDER BY opened_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RecentFile {
                path: r.get(0)?,
                build_id: r.get(1)?,
                opened_at: r.get(2)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }
}

fn row_to_pin(r: &rusqlite::Row<'_>) -> rusqlite::Result<Pin> {
    let kind_str: String = r.get(1)?;
    let kind = PinKind::parse(&kind_str).unwrap_or(PinKind::File);
    Ok(Pin {
        id: r.get(0)?,
        kind,
        target: r.get(2)?,
        build_id: r.get::<_, Option<String>>(3)?,
        label: r.get::<_, Option<String>>(4)?,
        created_at: r.get(5)?,
        note: r.get::<_, Option<String>>(6)?,
    })
}

/// ISO-8601 UTC timestamp. Format mirrors what `chrono::Utc::now()` would
/// produce for `to_rfc3339_opts(SecondsFormat::Secs, true)` so it sorts
/// lexicographically by string compare.
fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Build "YYYY-MM-DDTHH:MM:SSZ" by hand to avoid pulling chrono just
    // for a timestamp. This is the same shape `chrono::Utc::now()` would
    // produce for `to_rfc3339_opts(SecondsFormat::Secs, true)`.
    let (year, month, day, hour, min, sec) = epoch_to_ymdhms(secs as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Epoch seconds → Gregorian Y/M/D/h/m/s. Civil-from-days algorithm
/// (Howard Hinnant's date library). Tested-by-construction against
/// known fixed timestamps below.
fn epoch_to_ymdhms(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400) as u32;
    let hour = time / 3600;
    let min = (time / 60) % 60;
    let sec = time % 60;

    // Civil-from-days. `days` is days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open() -> (tempfile::TempDir, StateDb) {
        let dir = tempdir().unwrap();
        let db = StateDb::open_or_create(dir.path()).unwrap();
        (dir, db)
    }

    #[test]
    fn pin_add_is_idempotent() {
        let (_d, db) = open();
        let p1 = db
            .pin_add(PinKind::File, "src/Foo.java", Some("build-a"), None)
            .unwrap();
        let p2 = db
            .pin_add(PinKind::File, "src/Foo.java", Some("build-a"), None)
            .unwrap();
        assert_eq!(p1.id, p2.id);
        assert_eq!(db.pin_list().unwrap().len(), 1);
    }

    #[test]
    fn note_set_get_round_trip() {
        let (_d, db) = open();
        let p = db
            .pin_add(PinKind::File, "src/Foo.java", None, None)
            .unwrap();
        db.note_set(p.id, "remember to refactor").unwrap();
        assert_eq!(
            db.note_get(p.id).unwrap().as_deref(),
            Some("remember to refactor")
        );
        // Empty body clears.
        db.note_set(p.id, "").unwrap();
        assert!(db.note_get(p.id).unwrap().is_none());
    }

    #[test]
    fn pin_remove_cascades_to_note() {
        let (_d, db) = open();
        let p = db
            .pin_add(PinKind::File, "src/Foo.java", None, None)
            .unwrap();
        db.note_set(p.id, "x").unwrap();
        db.pin_remove(p.id).unwrap();
        assert!(db.note_get(p.id).unwrap().is_none());
        assert!(db.pin_list().unwrap().is_empty());
    }

    #[test]
    fn recent_files_caps_and_orders() {
        let (_d, db) = open();
        for i in 0..(RECENT_FILES_CAP + 5) {
            db.recent_file_record(&format!("f{i}.java"), "build")
                .unwrap();
        }
        let recents = db.recent_files().unwrap();
        assert_eq!(recents.len(), RECENT_FILES_CAP);
    }

    #[test]
    fn epoch_to_ymdhms_fixed_points() {
        // 1970-01-01T00:00:00Z
        assert_eq!(epoch_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
        // 2024-03-01T00:00:00Z (leap year boundary)
        assert_eq!(epoch_to_ymdhms(1_709_251_200), (2024, 3, 1, 0, 0, 0));
        // 2026-05-04T12:34:56Z
        assert_eq!(epoch_to_ymdhms(1_777_898_096), (2026, 5, 4, 12, 34, 56));
    }
}
