//! Hytale Modding docs ingestion.
//!
//! Walks a clone of <https://github.com/HytaleModding/site>, picking up
//! every `.md` and `.mdx` file as one searchable doc chunk. HM uses
//! `.mdx` (markdown + JSX) for almost all guide content - only the
//! repo-level READMEs and one corner of the type-docs are plain `.md`.
//! Treating both as the same flat-text input is fine for search: the
//! embedder + BM25 just see the body, and the right-panel viewer
//! renders MDX through a markdown shim (see `MarkdownView.tsx`).
//!
//! HM also ships translations (`af-ZA/`, `de/`, etc.) under
//! `content/docs/<locale>/`. Indexing every locale would 24x-duplicate
//! every guide and poison English search results, so we filter to the
//! English subtree by default.
//!
//! Chunks land in the same Tantivy + Lance index as the Java source,
//! tagged with `source_type = "hm_doc"` so the search UI can filter on
//! them. Fetching the repo (git clone) is the caller's responsibility;
//! this module only walks an already-on-disk tree.

use std::path::Path;

use anyhow::{Context, Result};

/// One markdown file ready to be turned into a search chunk.
pub struct DocFile {
    /// Path relative to the docs repo root, forward-slash form.
    pub rel_path: String,
    /// First H1 heading if present, otherwise the filename stem.
    pub title: String,
    /// Full markdown body, including the heading line.
    pub body: String,
    /// Total line count, used for UI readout.
    pub line_count: u64,
    /// Comma-joined author names from the frontmatter `authors[].name`
    /// (or singular `author` field). `None` if the doc has no
    /// frontmatter / no author entries.
    pub authors: Option<String>,
}

/// Walk a docs repo and emit one [`DocFile`] per `.md` file. Hidden
/// directories (`.git/`, `.github/`) and non-`.md` files are skipped.
/// Returns an empty vec if the directory is missing - the caller can
/// decide whether absent docs is an error or a warning.
pub fn walk_docs(repo_dir: &Path) -> Result<Vec<DocFile>> {
    if !repo_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(repo_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden dirs (`.git`, `.github`, `.vscode`, etc).
            // Files starting with `.` (e.g. `.editorconfig`) are kept;
            // they'll fall out of the `.md`-only filter below. The root
            // entry (depth 0) is always kept - on Windows tempfile
            // creates dirs with a `.tmp` prefix that would otherwise
            // trip the hidden-dir filter.
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            !e.file_name()
                .to_str()
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
        });
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", repo_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str());
        if !matches!(ext, Some("md") | Some("mdx")) {
            continue;
        }
        // Filter out non-English translations under `content/docs/<locale>/`.
        // Repo-level docs (README.md, CONTRIBUTING.md, etc.) live outside
        // `content/docs/` and are kept regardless. Files under
        // `content/docs/en/...` are kept; everything else under
        // `content/docs/` is treated as a translation duplicate and skipped.
        if let Ok(rel) = path.strip_prefix(repo_dir) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if let Some(after) = rel_str.strip_prefix("content/docs/") {
                let first_seg = after.split('/').next().unwrap_or("");
                if first_seg != "en" {
                    continue;
                }
            }
        }
        let body =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let title = extract_title(&body)
            .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(String::from))
            .unwrap_or_else(|| "untitled".into());
        let rel_path = path
            .strip_prefix(repo_dir)
            .with_context(|| format!("relativizing {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let line_count = body.lines().count() as u64;
        let authors = extract_authors(&body);
        out.push(DocFile {
            rel_path,
            title,
            body,
            line_count,
            authors,
        });
    }
    Ok(out)
}

/// Pull the YAML frontmatter block out of a markdown body, returning
/// the inner text (without the leading/trailing `---` fences). `None`
/// if the body doesn't open with `---`.
fn extract_frontmatter(body: &str) -> Option<&str> {
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))?;
    // Find the closing fence at the start of a line.
    let mut search_start = 0usize;
    while search_start < rest.len() {
        let slice = &rest[search_start..];
        if slice.starts_with("---\n") || slice.starts_with("---\r\n") || slice == "---" {
            return Some(&rest[..search_start]);
        }
        let next_nl = match slice.find('\n') {
            Some(i) => i + 1,
            None => return None,
        };
        search_start += next_nl;
    }
    None
}

/// Pull author names out of HM docs frontmatter. Supports both the
/// plural array form (`authors:\n  - name: "X"\n    link: "..."`) and
/// the rare singular string form (`author: "X"`). Returns the joined
/// list as `"Alice, Bob"`, or `None` if no authors are listed.
///
/// We don't pull a real YAML parser into the build for this - the HM
/// frontmatter format is narrow and stable, and a hand-rolled scan
/// keeps the indexer lean.
fn extract_authors(body: &str) -> Option<String> {
    let fm = extract_frontmatter(body)?;
    let mut names: Vec<String> = Vec::new();
    let mut in_authors_block = false;
    let mut authors_indent: usize = 0;

    for raw in fm.lines() {
        // Collapse Windows line endings if any slipped past.
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // Singular `author: "X"` or `author: X`.
        if !in_authors_block {
            if let Some(rest) = trimmed.strip_prefix("author:") {
                let v = strip_quotes(rest.trim());
                if !v.is_empty() {
                    names.push(v.to_string());
                }
                continue;
            }
            if trimmed.starts_with("authors:") {
                in_authors_block = true;
                authors_indent = indent;
                continue;
            }
            continue;
        }

        // Inside the `authors:` block. Stop when we de-dent back to or
        // past the `authors:` key column on a non-blank, non-list line.
        if trimmed.is_empty() {
            continue;
        }
        let is_list_entry = trimmed.starts_with("- ");
        if !is_list_entry && indent <= authors_indent {
            in_authors_block = false;
            // Re-process this line as a top-level key.
            if let Some(rest) = trimmed.strip_prefix("author:") {
                let v = strip_quotes(rest.trim());
                if !v.is_empty() {
                    names.push(v.to_string());
                }
            }
            continue;
        }

        // Look for `name: "..."` on either the list-entry start line
        // (`- name: "X"`) or a continuation line.
        let after_dash = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if let Some(rest) = after_dash.strip_prefix("name:") {
            let v = strip_quotes(rest.trim());
            if !v.is_empty() {
                names.push(v.to_string());
            }
        }
    }

    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Pull the first ATX-style H1 heading out of a markdown body. Returns
/// the heading text (without the leading `#` and whitespace), or `None`
/// if no H1 is found. Setext headings (`=====` underlines) are not
/// supported - the HM docs use ATX consistently.
fn extract_title(body: &str) -> Option<String> {
    for raw in body.lines() {
        let line = raw.trim_start();
        if let Some(rest) = line.strip_prefix("# ") {
            let trimmed = rest.trim().trim_end_matches('#').trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extracts_first_h1_as_title() {
        let body = "Some intro\n# Real Title\n## Sub";
        assert_eq!(extract_title(body).as_deref(), Some("Real Title"));
    }

    #[test]
    fn ignores_h2_and_deeper() {
        let body = "## Just an h2\n### Sub";
        assert!(extract_title(body).is_none());
    }

    #[test]
    fn strips_trailing_hashes() {
        let body = "# Title with closer ##";
        assert_eq!(extract_title(body).as_deref(), Some("Title with closer"));
    }

    #[test]
    fn extracts_authors_array_with_links() {
        let body = "---\ntitle: \"Block Components\"\nauthors:\n    - name: \"Bird\"\n      link: \"https://example.com\"\n    - name: \"oskarscot\"\n      link: \"https://oskar.scot\"\n---\n\n# Overview\n";
        assert_eq!(extract_authors(body).as_deref(), Some("Bird, oskarscot"),);
    }

    #[test]
    fn extracts_singular_author() {
        let body = "---\ntitle: \"X\"\nauthor: \"Alice\"\n---\n\n# X\n";
        assert_eq!(extract_authors(body).as_deref(), Some("Alice"));
    }

    #[test]
    fn no_frontmatter_no_authors() {
        let body = "# Just a heading\n\nbody text";
        assert!(extract_authors(body).is_none());
    }

    #[test]
    fn frontmatter_without_authors_returns_none() {
        let body = "---\ntitle: \"X\"\ndescription: \"Y\"\n---\n\n# X\n";
        assert!(extract_authors(body).is_none());
    }

    #[test]
    fn missing_dir_returns_empty() {
        let docs = walk_docs(Path::new("/this/path/does/not/exist")).unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn walks_only_markdown_skipping_hidden_dirs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.md"), "# A\n\nbody").unwrap();
        fs::write(dir.path().join("b.txt"), "ignored").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git").join("c.md"), "skip me").unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested").join("d.md"), "no h1 here").unwrap();

        let mut docs = walk_docs(dir.path()).unwrap();
        docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title, "A");
        assert!(docs[0].rel_path.ends_with("a.md"));
        // Falls back to filename stem when no H1 is present.
        assert_eq!(docs[1].title, "d");
    }

    #[test]
    fn picks_up_mdx_alongside_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("plain.md"), "# Plain\n\nbody").unwrap();
        fs::write(dir.path().join("guide.mdx"), "# Guide\n\nmdx body").unwrap();
        let mut docs = walk_docs(dir.path()).unwrap();
        docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().any(|d| d.rel_path.ends_with("guide.mdx")));
    }

    #[test]
    fn skips_non_english_locales_under_content_docs() {
        let dir = tempdir().unwrap();
        // Repo-level files outside content/docs are always kept.
        fs::write(dir.path().join("README.md"), "# Repo\n").unwrap();
        // English content kept.
        let en_dir = dir
            .path()
            .join("content")
            .join("docs")
            .join("en")
            .join("guides");
        fs::create_dir_all(&en_dir).unwrap();
        fs::write(en_dir.join("camera.mdx"), "# Camera\n").unwrap();
        // Translation skipped.
        let de_dir = dir
            .path()
            .join("content")
            .join("docs")
            .join("de")
            .join("guides");
        fs::create_dir_all(&de_dir).unwrap();
        fs::write(de_dir.join("camera.mdx"), "# Kamera\n").unwrap();

        let mut docs = walk_docs(dir.path()).unwrap();
        docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        let titles: Vec<_> = docs.iter().map(|d| d.title.as_str()).collect();
        assert!(titles.contains(&"Repo"));
        assert!(titles.contains(&"Camera"));
        assert!(!titles.contains(&"Kamera"));
    }
}
