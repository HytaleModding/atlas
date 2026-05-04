//! Extract class files from the Hytale server JAR into `classes_dir`.
//!
//! Mirrors Horizon's `common.py` extract step:
//!   - Skip native libraries prefixed `darwin|linux|freebsd|win`. These are
//!     zstd / other native bindings that we don't need for decompilation.
//!   - Rename `META-INF/LICENSE` -> `META-INF/LICENSE.renamed` to avoid
//!     collisions on case-insensitive filesystems where a `license/`
//!     directory also exists.
//!
//! Synchronous by design - runs under `tokio::task::spawn_blocking` so it
//! doesn't starve the async runtime while chewing through ~thousands of
//! entries.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use zip::ZipArchive;

pub trait ProgressSink: Send + Sync {
    fn report(&self, current: usize, total: usize);
}

const NATIVE_PREFIXES: &[&str] = &["darwin", "linux", "freebsd", "win"];

/// Extract all class files (and sibling resources) into `dest`. Returns the
/// number of entries actually written.
pub fn extract_server_jar(
    jar: &Path,
    dest: &Path,
    progress: &impl ProgressSink,
) -> Result<usize> {
    let file = File::open(jar).with_context(|| format!("opening {}", jar.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("parsing {} as zip", jar.display()))?;

    let total = archive.len();
    let mut written = 0usize;

    for i in 0..total {
        let mut entry = archive
            .by_index(i)
            .with_context(|| format!("reading zip entry {i}"))?;

        if entry.is_dir() {
            progress.report(i + 1, total);
            continue;
        }

        let raw_name = entry
            .enclosed_name()
            .ok_or_else(|| anyhow!("zip entry {} has an unsafe path", entry.name()))?
            .to_path_buf();

        if should_skip(&raw_name) {
            progress.report(i + 1, total);
            continue;
        }

        let out_rel = rewrite_path(&raw_name);
        let out_path = dest.join(&out_rel);

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let mut out = File::create(&out_path)
            .with_context(|| format!("creating {}", out_path.display()))?;
        io::copy(&mut entry, &mut out)
            .with_context(|| format!("writing {}", out_path.display()))?;
        out.flush()?;

        written += 1;
        progress.report(i + 1, total);
    }

    Ok(written)
}

fn should_skip(path: &Path) -> bool {
    // Only skip top-level native-lib directories (darwin/, linux/, freebsd/, win/).
    let Some(first) = path.components().next() else {
        return true;
    };
    let first_str = first.as_os_str().to_string_lossy().to_lowercase();
    NATIVE_PREFIXES.iter().any(|p| first_str == *p)
}

fn rewrite_path(path: &Path) -> PathBuf {
    // Case-insensitive match on `META-INF/LICENSE` as a file (not a directory).
    let as_str = path.to_string_lossy().replace('\\', "/");
    if as_str.eq_ignore_ascii_case("META-INF/LICENSE") {
        return PathBuf::from("META-INF/LICENSE.renamed");
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_native_lib_dirs() {
        assert!(should_skip(Path::new("win/zstd.dll")));
        assert!(should_skip(Path::new("linux/libzstd.so")));
        assert!(should_skip(Path::new("Darwin/libzstd.dylib")));
        assert!(!should_skip(Path::new("com/hypixel/Foo.class")));
    }

    #[test]
    fn renames_license_case_insensitively() {
        assert_eq!(
            rewrite_path(Path::new("META-INF/LICENSE")),
            PathBuf::from("META-INF/LICENSE.renamed")
        );
        assert_eq!(
            rewrite_path(Path::new("META-INF/license")),
            PathBuf::from("META-INF/LICENSE.renamed")
        );
        assert_eq!(
            rewrite_path(Path::new("META-INF/MANIFEST.MF")),
            PathBuf::from("META-INF/MANIFEST.MF")
        );
    }
}
