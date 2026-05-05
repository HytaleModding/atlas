//! Per-slot index readiness metadata, persisted next to the Tantivy index.
//!
//! This lets the UI answer "is search usable for this slot?" without having
//! to open the index.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// Distinct from Tantivy's own `meta.json` (which tracks segments). That
// file lives in the same directory, so a collision here would stomp the
// segment metadata and make the index unopenable.
const META_FILENAME: &str = "atlas-meta.json";

/// Chunker logic version. Bump whenever the chunker starts emitting
/// different chunk boundaries, symbol shapes, or per-chunk metadata.
/// Independent of Vineflower - a chunker bump does NOT require a
/// decompile re-run but DOES invalidate cached chunks/embeddings.
pub const CHUNKER_VERSION: &str = "1.0.0";

/// Artifact format version. Bumped when the artifact tarball layout
/// changes (e.g., new required file, renamed directory). Independent
/// of chunker or embedder churn.
pub const SCHEMA_VERSION: u32 = 1;

/// Minimum Atlas client that can mount an artifact at the current
/// `SCHEMA_VERSION`. Clients older than this refuse to mount.
pub const MIN_CLIENT_VERSION: &str = "0.1.0";

/// Embedder identifier - model + quantization. Bumped if we switch
/// to BGE-small-Q or a different 384-dim model. Older artifacts with
/// a different `embedder_id` stay mountable so long as dim matches.
pub const EMBEDDER_ID: &str = "bge-small-en-v1.5";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetadata {
    pub indexed_at: String,
    pub docs: u64,
    /// ISO-8601 mtime of the decompile output directory at the moment we
    /// indexed it. Used to flag "stale" when the decompile has since been
    /// rewritten.
    pub decompile_mtime: String,

    // --- Compound version key --------------------------------
    // Every field below is `default`-able so older `atlas-meta.json` files
    // written before this struct grew still deserialize cleanly. New
    // artifacts always populate all of them.
    /// Hytale `Implementation-Version`, e.g. `2026.03.26-89796e57b`.
    #[serde(default)]
    pub hytale_impl_version: String,
    /// Hytale `Implementation-Patchline`, e.g. `release` or `pre-release`.
    #[serde(default)]
    pub hytale_patchline: Option<String>,
    /// Vineflower jar version used to produce the decompile, e.g. `1.11.2`.
    /// Sourced from [`crate::patcher::vineflower::VINEFLOWER_VERSION`].
    #[serde(default)]
    pub vineflower_version: String,
    /// Chunker logic version - see [`CHUNKER_VERSION`].
    #[serde(default)]
    pub chunker_version: String,
    /// Embedder model identifier - see [`EMBEDDER_ID`].
    #[serde(default)]
    pub embedder_id: String,
    /// Embedding dimensionality. 384 for BGE-small; pinned to
    /// [`crate::embedder::EMBEDDING_DIM`].
    #[serde(default)]
    pub embedder_dim: u32,
    /// Artifact format version - see [`SCHEMA_VERSION`].
    #[serde(default)]
    pub schema_version: u32,
    /// Earliest Atlas client that can mount this artifact - see
    /// [`MIN_CLIENT_VERSION`].
    #[serde(default)]
    pub min_client_version: String,
    /// ISO-8601 timestamp the artifact was created at.
    #[serde(default)]
    pub created_at: String,
    /// Hex-encoded first 16 bytes of the Ed25519 signing pubkey. Empty
    /// for locally-built (unsigned) indexes; populated by `atlas-build`.
    #[serde(default)]
    pub signing_pubkey_fingerprint: String,
}

impl IndexMetadata {
    pub fn path(index_dir: &Path) -> std::path::PathBuf {
        index_dir.join(META_FILENAME)
    }

    pub fn read(index_dir: &Path) -> Option<Self> {
        let path = Self::path(index_dir);
        let bytes = fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&self, index_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(index_dir)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(Self::path(index_dir), bytes)
    }

    pub fn delete(index_dir: &Path) -> io::Result<()> {
        let path = Self::path(index_dir);
        if path.is_file() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

pub fn format_iso8601(ts: SystemTime) -> String {
    let duration = ts
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total = duration.as_secs();
    let millis = duration.subsec_millis();

    // Plain UTC ISO-8601; good enough for UI display and stale-checks.
    let days = (total / 86_400) as i64;
    let (year, month, day) = days_to_ymd(days);
    let secs_in_day = total % 86_400;
    let hour = secs_in_day / 3600;
    let minute = (secs_in_day % 3600) / 60;
    let second = secs_in_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn days_to_ymd(mut days: i64) -> (i64, u32, u32) {
    // Days since 1970-01-01 → (Y, M, D). Handles leap years via Howard
    // Hinnant's civil-from-days algorithm.
    days += 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
