//! Walks a decompile output tree and yields `(rel_path, fqn, package, filename)`
//! tuples for every `.java` file.
//!
//! The content isn't loaded here - indexing reads it on demand so memory
//! stays bounded. A separate `count_files` pass happens up front so the UI
//! can show "X / Y files indexed" progress.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct JavaFile {
    /// Relative path inside the decompile root, forward-slashed for index
    /// stability across platforms (we store this in the Tantivy index as
    /// `path` and use it to round-trip to disk via `read_source`).
    pub rel_path: String,
    /// Absolute path on disk - only used for reading content during indexing.
    pub abs_path: PathBuf,
    /// Dotted package (e.g. `com.hypixel.hytale.foo`).
    pub package: String,
    /// Class/file name without `.java`.
    pub filename: String,
    /// `package.Filename`.
    pub fqn: String,
}

pub fn count_files(root: &Path) -> usize {
    if !root.is_dir() {
        return 0;
    }
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("java"))
                .unwrap_or(false)
        })
        .count()
}

pub fn walk(root: &Path) -> impl Iterator<Item = JavaFile> + '_ {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(move |entry| {
            let path = entry.into_path();
            if !path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("java"))
                .unwrap_or(false)
            {
                return None;
            }
            let rel = path.strip_prefix(root).ok()?.to_path_buf();
            Some(to_java_file(path, rel))
        })
}

fn to_java_file(abs: PathBuf, rel: PathBuf) -> JavaFile {
    // Normalize to forward slashes so the index entries are portable
    // (Windows produces `\`, but we persist `/`).
    let rel_str: String = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");

    let filename = rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    // Package = every directory above the file, joined with dots.
    let package: String = rel
        .parent()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(".")
        })
        .unwrap_or_default();

    let fqn = if package.is_empty() {
        filename.clone()
    } else {
        format!("{package}.{filename}")
    };

    JavaFile {
        rel_path: rel_str,
        abs_path: abs,
        package,
        filename,
        fqn,
    }
}
