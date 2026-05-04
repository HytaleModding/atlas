//! Per-slot decompile metadata persisted alongside the output.
//!
//! Written to `workspace/{slot}/metadata.json` when a decompile finishes.
//! Used by the overview command to tell the UI whether a slot is Up to date,
//! Outdated, or Not decompiled without having to re-scan the source tree.

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotMetadata {
    /// ISO-8601 timestamp when the decompile finished.
    pub decompiled_at: String,
    /// ISO-8601 mtime of the server JAR at decompile time.
    pub jar_mtime: String,
    /// Size in bytes of the server JAR at decompile time.
    pub jar_size: u64,
    /// Hytale's `Implementation-Version` string at decompile time.
    pub hytale_version: Option<String>,
    /// Vineflower version used for the decompile.
    pub vineflower_version: String,
}

impl SlotMetadata {
    pub fn path(workspace: &Path) -> PathBuf {
        workspace.join("metadata.json")
    }

    pub fn read(workspace: &Path) -> Option<SlotMetadata> {
        let bytes = fs::read(Self::path(workspace)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&self, workspace: &Path) -> io::Result<()> {
        fs::create_dir_all(workspace)?;
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(Self::path(workspace), json)
    }

    pub fn delete(workspace: &Path) -> io::Result<()> {
        let p = Self::path(workspace);
        if p.exists() {
            fs::remove_file(p)?;
        }
        Ok(())
    }
}

/// Convert a SystemTime to an ISO-8601 / RFC3339 string with UTC offset.
/// Avoids a chrono dep for this one use: format is `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_iso8601(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Pure civil-calendar conversion from Unix epoch seconds to Y/M/D H:M:S (UTC).
/// Based on the public-domain algorithm in Howard Hinnant's date library:
/// https://howardhinnant.github.io/date_algorithms.html#civil_from_days
///
/// Note the +719468 offset: Hinnant's algorithm is rooted at 0000-03-01,
/// not 1970-01-01, so days-since-epoch must be shifted forward by that
/// many days before feeding it in.
fn epoch_to_ymd_hms(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days_since_epoch = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;

    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;

    (y, m, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_known_timestamp() {
        // 2025-01-01T00:00:00Z - commonly-quoted epoch second 1735689600.
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_735_689_600);
        assert_eq!(format_iso8601(t), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn formats_post_2026_timestamp() {
        // 2026-04-21T08:12:44Z.
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_776_759_164);
        assert_eq!(format_iso8601(t), "2026-04-21T08:12:44Z");
    }

    #[test]
    fn roundtrip_epoch_boundary() {
        let t = SystemTime::UNIX_EPOCH;
        assert_eq!(format_iso8601(t), "1970-01-01T00:00:00Z");
    }
}
