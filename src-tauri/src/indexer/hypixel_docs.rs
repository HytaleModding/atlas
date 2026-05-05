//! Hypixel Javadoc ingestion.
//!
//! Walks a local cache of mirrored Javadoc HTML - produced by
//! `release.server.docs.hytale.com` and `prerelease.server.docs.hytale.com`
//! (or any other Javadoc site rendered by the Java 11+ standard doclet) -
//! and emits one [`JavadocEntry`] per class / interface / enum / record /
//! annotation page. Each entry's `body` concatenates the type's prose
//! description with every method's prose description so a single search
//! chunk covers the whole class's documented surface.
//!
//! Architecture mirrors [`hm_docs`](super::hm_docs):
//! - The caller is responsible for putting Javadoc HTML on disk. CI uses
//! `wget --mirror --no-parent --no-host-directories` (or equivalent)
//! against each Javadoc host. Atlas itself never speaks HTTP from the
//! desktop indexer path.
//! - For local dev convenience, [`fetch_to_cache`] performs the same
//! mirror via `reqwest`, walking the `type-search-index.js` to
//! enumerate class pages.
//! - [`walk_cache`] is the consumer side: walks the cache and emits
//! `JavadocEntry` records for `indexer::run` to push as
//! `source_type = "hypixel_doc"` chunks.
//!
//! Parsing strategy - **defensive, not strict**. Javadoc HTML structure
//! is stable across releases but minor template drift is real (Java 17
//! tweaked some `<section>` classes). Match a primary selector first;
//! if it returns nothing, fall back to extracting all visible text
//! from the page's `<main>` element. The chunk text quality matters less
//! than recall - once a class is in the index, the user will find it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use scraper::{ElementRef, Html, Selector};

/// One Javadoc class page rendered as a single search chunk.
pub struct JavadocEntry {
    /// Path relative to the cache root, forward-slash form.
    /// E.g. `com/hytale/server/api/PlayerService.html`.
    pub rel_path: String,
    /// Fully-qualified type name, e.g. `com.hytale.server.api.PlayerService`.
    pub fqn: String,
    /// Simple type name, e.g. `PlayerService`.
    pub simple_name: String,
    /// One of `"class"`, `"interface"`, `"enum"`, `"record"`,
    /// `"annotation"`, or `"unknown"` if the header couldn't be parsed.
    pub kind: String,
    /// Full prose body - class description + every method description,
    /// separated by blank lines. Plain text, HTML stripped.
    pub body: String,
    /// Just the class-level prose description (the `<section class="description">`
    /// block), without method details concatenated in. Used for aux-text
    /// injection on matching source chunks: prepending the full `body`
    /// would bloat every method chunk in a 30-method class with the
    /// same hundreds of lines of doc text. Empty when the page has no
    /// class-level description.
    pub type_description: String,
    /// Total line count of `body`, used for UI readout.
    pub line_count: u64,
    /// Per-method docs preserved with structure. lets the
    /// source viewer render an inline Javadoc card above each method's
    /// declaration, not just one card at the class level.
    pub methods: Vec<MethodDoc>,
}

/// One method's doc block extracted from a Javadoc class page. Used by
/// the inline-Javadoc resolver to render a per-method card above the
/// matching source line. Keeping the simple-type list lets the resolver
/// disambiguate overloaded methods against the source's chunker output
/// without having to round-trip through full type resolution.
#[derive(Clone, Debug)]
pub struct MethodDoc {
    /// Method name as it appears in the Javadoc heading. Constructors
    /// keep their class name; don't translate them to `<init>`.
    pub name: String,
    /// Parameter types in order, reduced to their simple (last-segment)
    /// names with generics stripped. `java.util.List<String>` → `List`.
    pub param_simple_types: Vec<String>,
    /// Return type's simple name, when extractable. `Optional<T>` → `Optional`.
    /// Empty for constructors and when the signature can't be parsed.
    pub return_type_simple_name: Option<String>,
    /// Description prose (HTML stripped, whitespace collapsed). May be
    /// empty when the doclet emitted only the signature.
    pub prose: String,
    /// True when the doclet rendered a `<span class="deprecated-label">`
    /// or equivalent marker inside the method block.
    pub deprecated: bool,
}

/// Walk a Javadoc cache directory and emit one [`JavadocEntry`] per
/// class page. Non-class pages (`overview-summary.html`,
/// `package-summary.html`, `module-summary.html`, indexes, frames) and
/// non-`.html` files are skipped. Returns an empty vec if the directory
/// is missing - the caller decides whether absent docs is an error.
pub fn walk_cache(cache_dir: &Path) -> Result<Vec<JavadocEntry>> {
    if !cache_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(cache_dir).follow_links(false);
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", cache_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("html") {
            continue;
        }
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if is_non_class_page(file_name) {
            continue;
        }
        let html =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let rel_path = path
            .strip_prefix(cache_dir)
            .with_context(|| format!("relativizing {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(parsed) = parse_class_page(&rel_path, &html) {
            out.push(parsed);
        }
    }
    Ok(out)
}

/// Cheap directory walk that returns `(fqn, path)` pairs without
/// reading or parsing the HTML. Used by the inline-Javadoc resolver
/// to build an FQN→path lookup map at viewer-open time
/// without paying the full `walk_cache` cost. FQN is derived from the
/// relative path (`com/foo/Bar.html` → `com.foo.Bar`); pages that don't
/// have a `.` in their derived FQN are top-level classes and survive.
pub fn walk_cache_paths(cache_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    if !cache_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(cache_dir).follow_links(false);
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", cache_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("html") {
            continue;
        }
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if is_non_class_page(file_name) {
            continue;
        }
        let rel = match path.strip_prefix(cache_dir) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let stem = match Path::new(&rel).file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let pkg = derive_package(&rel);
        let fqn = if pkg.is_empty() {
            stem
        } else {
            format!("{pkg}.{stem}")
        };
        out.push((fqn, path.to_path_buf()));
    }
    Ok(out)
}

/// Skip-list for filenames that are part of every Javadoc tree but
/// aren't class pages. Anything not matching is treated as a candidate
/// class page; `parse_class_page` does the final filtering by checking
/// for a recognisable type header.
fn is_non_class_page(name: &str) -> bool {
    matches!(
        name,
        "index.html"
            | "overview-summary.html"
            | "overview-tree.html"
            | "package-summary.html"
            | "package-tree.html"
            | "package-frame.html"
            | "module-summary.html"
            | "module-tree.html"
            | "module-frame.html"
            | "allclasses.html"
            | "allclasses-index.html"
            | "allpackages-index.html"
            | "constant-values.html"
            | "deprecated-list.html"
            | "help-doc.html"
            | "index-all.html"
            | "serialized-form.html"
            | "system-properties.html"
    ) || name.starts_with("index-")
}

/// Parse one Javadoc class page. Returns `None` if the page doesn't look
/// like a type page (no detectable header) - this filters out stray HTML
/// the skip-list missed without bailing the whole walk.
pub fn parse_class_page(rel_path: &str, html: &str) -> Option<JavadocEntry> {
    let doc = Html::parse_document(html);

    // The type header lives in `<h1 class="title">` for the standard
    // doclet. Format: "Class Foo", "Interface Bar", "Enum Class Baz",
    // "Record Class Qux", "Annotation Interface MyAnno".
    let h1_sel = Selector::parse("h1.title, h1[title]").ok()?;
    let header_raw = doc
        .select(&h1_sel)
        .next()
        .map(|el| collapse_whitespace(&el.text().collect::<String>()));
    let (kind, simple_name) = header_raw
        .as_deref()
        .and_then(parse_type_header)
        .unwrap_or_else(|| {
            // Fallback: derive simple name from filename stem; mark kind
            // unknown so callers can tell parsing was lossy.
            let stem = std::path::Path::new(rel_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string();
            ("unknown".to_string(), stem)
        });

    let package = derive_package(rel_path);
    let fqn = if package.is_empty() {
        simple_name.clone()
    } else {
        format!("{package}.{simple_name}")
    };

    // Type-level prose: the standard doclet renders this as `<div class="block">`
    // immediately after the header. Older templates use
    // `<section class="description"> ... <div class="block">`.
    // Tracked separately from method details so aux-text injection
    // (prepending Javadoc prose onto matching source chunks) can use
    // just the class-level summary without bloating every chunk with
    // every method's prose.
    let mut sections: Vec<String> = Vec::new();
    let mut type_description = String::new();
    let type_block_sel = Selector::parse(
        "section.description div.block, .description > .block, #class-description div.block",
    )
    .ok()?;
    if let Some(el) = doc.select(&type_block_sel).next() {
        let text = collapse_whitespace(&el.text().collect::<String>());
        if !text.is_empty() {
            type_description = text.clone();
            sections.push(text);
        }
    }

    // Method-level prose. Each method's detail block lives under
    // `section.detail` (Java 11+) or `<a id="...">` blocks (older).
    // Walk every `section.detail`, pull (heading, signature, block) once
    // per section, push the prose into the BM25/embedding `body` and
    // also build a structured `MethodDoc` for the inline-Javadoc
    // resolver. Field-detail sections also flow through
    // here; keep their prose in the body for retrieval but skip them
    // in `methods` because the inline anchor only targets methods.
    let detail_section_sel = Selector::parse("section.detail").ok()?;
    let h_sel = Selector::parse("h3, h4").ok()?;
    let sig_sel = Selector::parse("div.member-signature, .member-signature").ok()?;
    let block_sel = Selector::parse("div.block").ok()?;
    let notes_sel = Selector::parse("dl.notes").ok()?;
    let deprecated_sel = Selector::parse(
        "span.deprecated-label, .deprecated-label, span.deprecation-comment, .deprecation-comment",
    )
    .ok()?;
    let mut methods: Vec<MethodDoc> = Vec::new();
    for section in doc.select(&detail_section_sel) {
        let description = section
            .select(&block_sel)
            .next()
            .map(|el| collapse_whitespace(&el.text().collect::<String>()))
            .unwrap_or_default();
        let notes = section
            .select(&notes_sel)
            .next()
            .map(format_notes_dl)
            .unwrap_or_default();
        let prose = match (description.is_empty(), notes.is_empty()) {
            (true, true) => String::new(),
            (false, true) => description,
            (true, false) => notes,
            (false, false) => format!("{description}\n\n{notes}"),
        };
        if !prose.is_empty() {
            sections.push(prose.clone());
        }
        let heading = section
            .select(&h_sel)
            .next()
            .map(|el| collapse_whitespace(&el.text().collect::<String>()))
            .unwrap_or_default();
        if heading.is_empty() {
            continue;
        }
        let sig = section
            .select(&sig_sel)
            .next()
            .map(|el| collapse_whitespace(&el.text().collect::<String>()))
            .unwrap_or_default();
        // Only emit a `MethodDoc` when the signature contains a parameter
        // list, since that's how distinguish methods/constructors from
        // fields and enum constants without baking in a tag list.
        if !sig.contains('(') {
            continue;
        }
        // Skip methods Hypixel left fully undocumented. Without a
        // description block and without any notes, the inline anchor
        // would render as a header with nothing under it.
        if prose.is_empty() {
            continue;
        }
        let (param_simple_types, return_type_simple_name) = parse_method_signature(&sig);
        let deprecated = section.select(&deprecated_sel).next().is_some();
        methods.push(MethodDoc {
            name: heading,
            param_simple_types,
            return_type_simple_name,
            prose,
            deprecated,
        });
    }

    // Defensive fallback: if neither selector matched anything, scrape
    // visible text from `<main>` so the page still contributes some
    // signal to the index. Better stale match than no match.
    if sections.is_empty() {
        let main_sel = Selector::parse("main, body").ok()?;
        if let Some(el) = doc.select(&main_sel).next() {
            let text = collapse_whitespace(&el.text().collect::<String>());
            if !text.is_empty() {
                sections.push(text);
            }
        }
    }

    if sections.is_empty() {
        // Truly empty page - drop it.
        return None;
    }

    // Lead with the FQN so BM25 + the embedder both see the symbol name
    // up front. Mirrors what for source chunks.
    let mut body = String::with_capacity(64 + sections.iter().map(|s| s.len()).sum::<usize>());
    body.push_str(&fqn);
    body.push('\n');
    body.push_str(&kind);
    body.push(' ');
    body.push_str(&simple_name);
    body.push_str("\n\n");
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            body.push_str("\n\n");
        }
        body.push_str(section);
    }

    let line_count = body.lines().count() as u64;
    Some(JavadocEntry {
        rel_path: rel_path.to_string(),
        fqn,
        simple_name,
        kind,
        body,
        type_description,
        line_count,
        methods,
    })
}

/// Pull `(param_simple_types, return_type_simple_name)` out of a Javadoc
/// member-signature string like `public void kick(Player player, String reason)`
/// or `public static <T> Optional<T> wrap(T item)`.
///
/// Returns empty vec + `None` when the signature can't be parsed; callers
/// fall back to name-only matching against the source. Never throw on
/// signatures don't recognise - better a miss than a panic.
fn parse_method_signature(sig: &str) -> (Vec<String>, Option<String>) {
    // Find the first top-level `(`. Anything before it is modifiers +
    // type params + return type + method name; anything between it and
    // its matching `)` is the parameter list.
    let bytes = sig.as_bytes();
    let mut depth_angle: i32 = 0;
    let mut paren_open: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth_angle += 1,
            b'>' => depth_angle = (depth_angle - 1).max(0),
            b'(' if depth_angle == 0 => {
                paren_open = Some(i);
                break;
            }
            _ => {}
        }
    }
    let Some(open) = paren_open else {
        return (Vec::new(), None);
    };
    // Find matching `)` allowing nested generics inside the parameter list.
    let mut depth_paren: i32 = 1;
    depth_angle = 0;
    let mut close: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().skip(open + 1) {
        match b {
            b'<' => depth_angle += 1,
            b'>' => depth_angle = (depth_angle - 1).max(0),
            b'(' if depth_angle == 0 => depth_paren += 1,
            b')' if depth_angle == 0 => {
                depth_paren -= 1;
                if depth_paren == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = match close {
        Some(c) => c,
        None => return (Vec::new(), None),
    };

    // Parameter list - split at top-level commas.
    let params_str = &sig[open + 1..close];
    let mut params: Vec<String> = Vec::new();
    if !params_str.trim().is_empty() {
        let mut depth_a: i32 = 0;
        let mut depth_p: i32 = 0;
        let mut start = 0usize;
        let pb = params_str.as_bytes();
        for (i, &b) in pb.iter().enumerate() {
            match b {
                b'<' => depth_a += 1,
                b'>' => depth_a = (depth_a - 1).max(0),
                b'(' => depth_p += 1,
                b')' => depth_p = (depth_p - 1).max(0),
                b',' if depth_a == 0 && depth_p == 0 => {
                    let part = &params_str[start..i];
                    if let Some(t) = simple_type_of_param(part) {
                        params.push(t);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        let tail = &params_str[start..];
        if !tail.trim().is_empty() {
            if let Some(t) = simple_type_of_param(tail) {
                params.push(t);
            }
        }
    }

    // Return type: scan backwards from `(` skipping the method name.
    // Layout is "<modifiers...> <return-type> <method-name>(", so the
    // last whitespace-separated token before `(` is the method name and
    // the token before that is the return type's tail.
    let head = sig[..open].trim();
    let return_type = head
        .rsplit_once(char::is_whitespace)
        .and_then(|(before, _name)| {
            let before = before.trim_end();
            if before.is_empty() {
                None
            } else {
                Some(simple_type_name(before))
            }
        });

    (params, return_type)
}

/// Reduce one parameter declaration like `final java.util.List<String> xs`
/// to its simple type name `List`. Handles annotations, varargs, and
/// trailing variable names. Returns `None` if the declaration is empty.
fn simple_type_of_param(part: &str) -> Option<String> {
    let trimmed = part.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip leading annotations like `@NotNull` (no-op when absent).
    let mut tokens: Vec<&str> = trimmed.split_whitespace().collect();
    while tokens.first().is_some_and(|t| t.starts_with('@')) {
        tokens.remove(0);
    }
    // Strip leading `final` modifier on params (Javadoc rarely emits it
    // but be defensive).
    if tokens.first() == Some(&"final") {
        tokens.remove(0);
    }
    if tokens.is_empty() {
        return None;
    }
    // The last token is usually the variable name; the type can span
    // multiple tokens when generics or arrays got split by whitespace
    // (rare, but `Map<K, V>` after split_whitespace becomes `Map<K,` `V>`).
    // Take everything except the last token, then if the type still has
    // unbalanced angle brackets it means whitespace inside a generic
    // separated us - recombine until balanced.
    if tokens.len() == 1 {
        return Some(simple_type_name(tokens[0]));
    }
    let mut type_part = tokens[..tokens.len() - 1].join(" ");
    let opens = type_part.matches('<').count();
    let closes = type_part.matches('>').count();
    if opens != closes {
        // Variable name actually belonged to the type - rejoin all.
        type_part = tokens.join(" ");
    }
    Some(simple_type_name(&type_part))
}

/// Reduce a fully-qualified, generic-laden type to its simple name.
/// `java.util.List<String>` → `List`; `Map<K, V>` → `Map`;
/// `String[]` → `String`; `int...` → `int`; `T extends Foo` → `T`.
///
/// Public so the source-side method-list resolver in `symbols.rs` can
/// reduce its raw param strings the same way and match overloads on
/// the Javadoc side.
pub fn simple_type_name(t: &str) -> String {
    let s = t.trim();
    // Drop generic params, array brackets, varargs ellipsis, and any
    // type-bound suffix (e.g. `T extends Foo`).
    let s = s.split('<').next().unwrap_or(s);
    let s = s.split('[').next().unwrap_or(s);
    let s = s.split("...").next().unwrap_or(s);
    let s = s.split_whitespace().next().unwrap_or(s);
    // Take the last `.`-separated segment for FQNs.
    s.rsplit('.').next().unwrap_or(s).to_string()
}

/// Render a Javadoc class page as structured plain text for the
/// right-panel viewer. Unlike the indexer's [`parse_class_page`] (which
/// flattens everything into a single bag-of-text optimized for BM25 +
/// embeddings), this preserves visible structure: a header block with
/// the FQN and type signature, the class-level description, then each
/// method as `── methodSignature` heading + its description paragraph.
/// Output is plain text - the panel applies CSS wrap, so callers don't
/// need to insert hard line breaks within paragraphs.
///
/// Returns `None` if the page doesn't parse as a class doc (skip-list
/// miss); callers fall back to raw HTML.
pub fn render_class_page(rel_path: &str, html: &str) -> Option<String> {
    let doc = Html::parse_document(html);

    // Header - same selector as parse_class_page.
    let h1_sel = Selector::parse("h1.title, h1[title]").ok()?;
    let header_raw = doc
        .select(&h1_sel)
        .next()
        .map(|el| collapse_whitespace(&el.text().collect::<String>()))?;
    let (kind, simple_name) =
        parse_type_header(&header_raw).unwrap_or_else(|| ("type".to_string(), header_raw.clone()));
    let package = derive_package(rel_path);
    let fqn = if package.is_empty() {
        simple_name.clone()
    } else {
        format!("{package}.{simple_name}")
    };

    let mut out = String::new();
    out.push_str(&fqn);
    out.push('\n');
    out.push_str(&kind);
    out.push(' ');
    out.push_str(&simple_name);
    out.push_str("\n\n");

    // Class signature line if present (e.g. "public class Foo extends Bar").
    if let Ok(sig_sel) = Selector::parse("section.description div.type-signature, .type-signature")
    {
        if let Some(el) = doc.select(&sig_sel).next() {
            let sig = collapse_whitespace(&el.text().collect::<String>());
            if !sig.is_empty() {
                out.push_str(&sig);
                out.push_str("\n\n");
            }
        }
    }

    // Type-level description.
    if let Ok(desc_sel) = Selector::parse(
        "section.description div.block, .description > .block, #class-description div.block",
    ) {
        if let Some(el) = doc.select(&desc_sel).next() {
            let text = collapse_whitespace(&el.text().collect::<String>());
            if !text.is_empty() {
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
    }

    // Method details - render each `<section class="detail">` block as
    // its own headed paragraph. Standard Java 11+ doclet wraps each
    // member in `<section class="detail">` containing an `<h3>` (member
    // name), an optional `<div class="member-signature">`, and a
    // `<div class="block">` (description). Emit name + signature as a
    // visual separator so the viewer reads like a doc, not a blob.
    if let Ok(detail_section_sel) = Selector::parse("section.detail") {
        let h_sel = Selector::parse("h3, h4").ok()?;
        let sig_sel = Selector::parse("div.member-signature, .member-signature").ok()?;
        let block_sel = Selector::parse("div.block").ok()?;
        let mut emitted_methods_header = false;
        for section in doc.select(&detail_section_sel) {
            let name = section
                .select(&h_sel)
                .next()
                .map(|el| collapse_whitespace(&el.text().collect::<String>()))
                .unwrap_or_default();
            let sig = section
                .select(&sig_sel)
                .next()
                .map(|el| collapse_whitespace(&el.text().collect::<String>()))
                .unwrap_or_default();
            let desc = section
                .select(&block_sel)
                .next()
                .map(|el| collapse_whitespace(&el.text().collect::<String>()))
                .unwrap_or_default();
            if name.is_empty() && sig.is_empty() && desc.is_empty() {
                continue;
            }
            if !emitted_methods_header {
                out.push_str("──────────────\n\n");
                emitted_methods_header = true;
            }
            if !name.is_empty() {
                out.push_str(&name);
                out.push('\n');
            }
            if !sig.is_empty() {
                out.push_str(&sig);
                out.push_str("\n\n");
            }
            if !desc.is_empty() {
                out.push_str(&desc);
                out.push_str("\n\n");
            }
        }
    }

    if out.trim().is_empty() {
        return None;
    }
    Some(out)
}

/// Squash runs of whitespace (including newlines) down to single spaces
/// and trim. Javadoc HTML has lots of indentation in `text()` output
/// that is meaningless for embedding.
/// Flatten a Javadoc `<dl class="notes">` block into readable plain text.
///
/// The standard doclet renders `@param`, `@return`, `@throws`, `@since`,
/// `@see`, `@specified-by`, `@overrides` etc. as alternating `<dt>` / `<dd>`
/// pairs inside `dl.notes`. Preserve that label/value structure so the
/// inline-Javadoc card shows e.g.
///
/// ```text
/// Parameters:
/// itemId - The block type key of the associated item.
/// Returns:
/// the asset store for CameraShake assets.
/// ```
///
/// Each `<dt>` starts a new label group; each following `<dd>` is indented
/// under the most recent label. Document order is preserved.
fn format_notes_dl(dl: ElementRef) -> String {
    let dt_dd_sel = match Selector::parse("dt, dd") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    let mut have_label = false;
    for el in dl.select(&dt_dd_sel) {
        let tag = el.value().name();
        let text = collapse_whitespace(&el.text().collect::<String>());
        if text.is_empty() {
            continue;
        }
        match tag {
            "dt" => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text);
                have_label = true;
            }
            "dd" => {
                if !out.is_empty() {
                    out.push('\n');
                }
                if have_label {
                    out.push_str("  ");
                }
                out.push_str(&text);
            }
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // leading-trim
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Pull `(kind, simple_name)` out of a Javadoc type-header string.
/// Handles the seven standard headings emitted by the Java doclet:
/// "Class Foo"
/// "Interface Foo"
/// "Enum Class Foo" (Java 17+)
/// "Enum Foo" (Java 11)
/// "Record Class Foo" (Java 17+)
/// "Annotation Interface Foo" (Java 17+)
/// "Annotation Type Foo" (Java 11)
fn parse_type_header(header: &str) -> Option<(String, String)> {
    let h = header.trim();
    let prefixes: &[(&str, &str)] = &[
        ("Annotation Interface ", "annotation"),
        ("Annotation Type ", "annotation"),
        ("Enum Class ", "enum"),
        ("Record Class ", "record"),
        ("Interface ", "interface"),
        ("Enum ", "enum"),
        ("Record ", "record"),
        ("Class ", "class"),
    ];
    for (prefix, kind) in prefixes {
        if let Some(rest) = h.strip_prefix(prefix) {
            // Strip generic params and trailing whitespace / annotations:
            // "Foo<T>" → "Foo"
            // "Foo<T extends Bar>" → "Foo"
            let name = rest.split('<').next().unwrap_or(rest).trim();
            if !name.is_empty() {
                return Some(((*kind).to_string(), name.to_string()));
            }
        }
    }
    None
}

/// Derive the dotted Java package from a forward-slash relative path.
/// `com/hytale/server/api/PlayerService.html` → `com.hytale.server.api`.
/// Returns an empty string for top-level classes.
fn derive_package(rel_path: &str) -> String {
    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() <= 1 {
        return String::new();
    }
    // Skip leading host-slug components. `fetch_many_to_cache` nests
    // pages under one or more per-host subdirectories (e.g.
    // `release.server.docs.hytale.com/com/.../X.html`), but the FQN of
    // a Java class never includes those. Heuristic: Java package
    // components never contain a `.`; host slugs always do.
    let mut start = 0;
    while start < parts.len() - 1 && parts[start].contains('.') {
        start += 1;
    }
    parts[start..parts.len() - 1].join(".")
}

/// Build a lookup map of `fqn → type_description` from a slice of
/// entries. Empty descriptions are dropped so callers can treat an
/// absent key as "no aux text to inject" without checking emptiness.
/// O(n) build; O(1) per source-chunk lookup downstream.
pub fn build_aux_text_index(entries: &[JavadocEntry]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::with_capacity(entries.len());
    for entry in entries {
        if entry.type_description.is_empty() {
            continue;
        }
        map.insert(entry.fqn.clone(), entry.type_description.clone());
    }
    map
}

/// Prepend a Javadoc class description onto a chunk's text in the same
/// canonical format used by [`crate::indexer::summarizer::inject_summary`],
/// so the wire-format stays consistent and the search side doesn't need
/// to know which path produced the marker. The prefix differs (`JAVADOC`
/// vs `SUMMARY`) so log analysis can tell the two apart.
pub fn inject_aux_text(chunk: &mut crate::indexer::chunker::Chunk, javadoc: &str) {
    chunk.text = format!("// JAVADOC: {javadoc}\n\n{}", chunk.text);
}

// -- Optional in-process fetcher (local dev only) ----------------------
//
// CI pipelines should mirror Javadocs with `wget` or similar before
// invoking `atlas-build`. The function below exists so a developer can
// `cargo run --bin atlas-build -- index --hypixel-docs <fresh-cache>`
// without setting up an external mirror first. It's deliberately
// minimal: walks `type-search-index.js`, GETs each class page, writes
// to disk. No retries, no backoff, no concurrency - meant for a one-off
// local pull, not production.

/// Pull every class page referenced by `type-search-index.js` from
/// `host` and cache them under `cache_dir`. `host` should be a base URL
/// like `https://release.server.docs.hytale.com`. Returns the number of
/// new pages downloaded (already-cached pages are skipped). The optional
/// `module` argument scopes the fetch to one Javadoc module if the host
/// uses module-prefixed URLs (most don't); pass `None` for the common
/// case.
pub async fn fetch_to_cache(host: &str, cache_dir: &Path, module: Option<&str>) -> Result<usize> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;

    let host = host.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .user_agent("atlas-build/0.1 (+https://github.com/HytaleModding)")
        .build()
        .context("building reqwest client")?;

    let index_url = format!("{host}/type-search-index.js");
    let index_text = client
        .get(&index_url)
        .send()
        .await
        .with_context(|| format!("GET {index_url}"))?
        .error_for_status()
        .with_context(|| format!("status check {index_url}"))?
        .text()
        .await
        .with_context(|| format!("body {index_url}"))?;

    let entries = parse_type_search_index(&index_text)?;
    let total = entries.len();
    let mut downloaded = 0usize;
    let mut cache_hits = 0usize;
    let mut processed = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    // Emit a progress line every PROGRESS_EVERY entries seen so the
    // terminal isn't a black hole during the cold-cache first-run case
    // (~6000 pages, ~10 minutes). The fetcher previously logged only on
    // 404s, so a healthy run looked indistinguishable from a hang.
    const PROGRESS_EVERY: usize = 100;
    tracing::info!(total, "javadoc fetch starting");
    for entry in entries {
        // Skip the synthetic "AllClasses" sentinel some doclet versions
        // add at index 0 (`{p:"",l:"All Classes and Interfaces"}`).
        if entry.class_name.is_empty() {
            continue;
        }
        let pkg_path = entry.package.replace('.', "/");
        let rel = if pkg_path.is_empty() {
            format!("{}.html", entry.class_name)
        } else {
            format!("{pkg_path}/{}.html", entry.class_name)
        };
        let rel = match module {
            Some(m) if !m.is_empty() => format!("{m}/{rel}"),
            _ => rel,
        };
        if !seen.insert(rel.clone()) {
            continue;
        }
        processed += 1;
        let dest = cache_dir.join(&rel);
        if dest.is_file() {
            cache_hits += 1;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let url = format!("{host}/{rel}");
            let resp = client
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            if !resp.status().is_success() {
                tracing::warn!(%url, status = %resp.status(), "javadoc page missing; skipping");
            } else {
                let body = resp.bytes().await.with_context(|| format!("body {url}"))?;
                std::fs::write(&dest, &body)
                    .with_context(|| format!("writing {}", dest.display()))?;
                downloaded += 1;
            }
        }
        if processed % PROGRESS_EVERY == 0 {
            tracing::info!(
                processed,
                total,
                downloaded,
                cache_hits,
                "javadoc fetch progress"
            );
        }
    }
    tracing::info!(
        processed,
        total,
        downloaded,
        cache_hits,
        "javadoc fetch complete"
    );
    Ok(downloaded)
}

/// Mirror of one record from `type-search-index.js`. The Java doclet
/// emits `{p:"<package>", c:"<class>", l:"<label>", u:"<url>"?}`.
struct TypeSearchEntry {
    package: String,
    class_name: String,
}

/// Parse the `typeSearchIndex = [...];` JS file. The body inside the
/// brackets is valid JSON, so strip the JS wrapper and hand the
/// payload to `serde_json`.
fn parse_type_search_index(text: &str) -> Result<Vec<TypeSearchEntry>> {
    let start = text
        .find('[')
        .ok_or_else(|| anyhow!("type-search-index.js missing '['"))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| anyhow!("type-search-index.js missing ']'"))?;
    if end <= start {
        return Err(anyhow!("type-search-index.js bracket order"));
    }
    let json = &text[start..=end];

    // The doclet emits unquoted keys (`p:"foo"`). serde_json needs them
    // quoted. Do the cheapest-possible normalization that's safe for
    // Javadoc output: quote the four known keys when they appear after
    // `{` or `,`. This is fragile against future doclet changes but
    // good enough for Java 11-21.
    let normalized = normalize_jsdoc_keys(json);

    let parsed: Vec<RawEntry> = serde_json::from_str(&normalized)
        .with_context(|| "parsing normalized type-search-index.js")?;

    Ok(parsed
        .into_iter()
        .map(|r| TypeSearchEntry {
            package: r.p.unwrap_or_default(),
            class_name: r.c.or(r.l).unwrap_or_default(),
        })
        .collect())
}

#[derive(serde::Deserialize)]
struct RawEntry {
    p: Option<String>,
    c: Option<String>,
    l: Option<String>,
}

/// Minimal JS-to-JSON: quote the keys `p`, `c`, `l`, `u` when they
/// appear at object boundaries. The doclet emits exactly these four
/// keys; anything else means the format changed and want to
/// re-test against the new doclet output anyway.
fn normalize_jsdoc_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 64);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        out.push(b as char);
        if b == b'{' || b == b',' {
            // Skip whitespace after the boundary, then check for an
            // unquoted key character followed by ':'.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                out.push(bytes[j] as char);
                j += 1;
            }
            if j + 1 < bytes.len()
                && matches!(bytes[j], b'p' | b'c' | b'l' | b'u')
                && bytes[j + 1] == b':'
            {
                out.push('"');
                out.push(bytes[j] as char);
                out.push('"');
                i = j;
            }
        }
        i += 1;
    }
    out
}

/// Convenience wrapper used by `atlas-build` when given multiple hosts:
/// fetches each into a sub-directory keyed by host (with the scheme
/// stripped) so release + prerelease coexist without clashing.
pub async fn fetch_many_to_cache(
    hosts: &[&str],
    cache_root: &Path,
) -> Result<Vec<(PathBuf, usize)>> {
    let mut out = Vec::new();
    for host in hosts {
        let key = host
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .replace(['/', ':'], "_");
        let sub = cache_root.join(key);
        let n = fetch_to_cache(host, &sub, None).await?;
        out.push((sub, n));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_type_header_for_each_kind() {
        assert_eq!(
            parse_type_header("Class PlayerService"),
            Some(("class".into(), "PlayerService".into()))
        );
        assert_eq!(
            parse_type_header("Interface Listener"),
            Some(("interface".into(), "Listener".into()))
        );
        assert_eq!(
            parse_type_header("Enum Class Direction"),
            Some(("enum".into(), "Direction".into()))
        );
        assert_eq!(
            parse_type_header("Enum Direction"),
            Some(("enum".into(), "Direction".into()))
        );
        assert_eq!(
            parse_type_header("Record Class Coord"),
            Some(("record".into(), "Coord".into()))
        );
        assert_eq!(
            parse_type_header("Annotation Interface Override"),
            Some(("annotation".into(), "Override".into()))
        );
        assert_eq!(
            parse_type_header("Annotation Type Override"),
            Some(("annotation".into(), "Override".into()))
        );
    }

    #[test]
    fn type_header_strips_generic_params() {
        assert_eq!(
            parse_type_header("Class Optional<T>"),
            Some(("class".into(), "Optional".into()))
        );
        assert_eq!(
            parse_type_header("Interface Map<K extends Object, V>"),
            Some(("interface".into(), "Map".into()))
        );
    }

    #[test]
    fn type_header_returns_none_on_unknown_prefix() {
        assert!(parse_type_header("Module com.foo").is_none());
        assert!(parse_type_header("Package com.foo").is_none());
    }

    #[test]
    fn package_derived_from_path() {
        assert_eq!(
            derive_package("com/hytale/server/api/PlayerService.html"),
            "com.hytale.server.api"
        );
        assert_eq!(derive_package("Toplevel.html"), "");
    }

    #[test]
    fn whitespace_collapse_is_idempotent_for_clean_input() {
        assert_eq!(collapse_whitespace("hello world"), "hello world");
        assert_eq!(collapse_whitespace("  hello\n\n  world  "), "hello world");
        assert_eq!(collapse_whitespace(""), "");
    }

    #[test]
    fn skips_known_non_class_pages() {
        for name in [
            "index.html",
            "package-summary.html",
            "module-tree.html",
            "allclasses-index.html",
            "index-1.html",
            "index-files.html",
            "constant-values.html",
        ] {
            assert!(is_non_class_page(name), "expected to skip {name}");
        }
        assert!(!is_non_class_page("PlayerService.html"));
    }

    #[test]
    fn parses_minimal_class_page() {
        // Hand-crafted minimal Javadoc-style HTML. Mirrors what the Java
        // 17+ standard doclet emits for a class with one documented
        // method; if the real doclet output drifts from this, the
        // selectors fall back to the `<main>` text dump and parsing
        // still succeeds.
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Class PlayerService</h1>
                <section class="description">
                  <div class="block">Manages player sessions on the server.</div>
                </section>
                <section class="detail">
                  <h3>kick</h3>
                  <div class="block">Removes the given player from the world.</div>
                </section>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/hytale/server/api/PlayerService.html", html).unwrap();
        assert_eq!(entry.kind, "class");
        assert_eq!(entry.simple_name, "PlayerService");
        assert_eq!(entry.fqn, "com.hytale.server.api.PlayerService");
        assert!(entry.body.contains("Manages player sessions"));
        assert!(entry.body.contains("Removes the given player"));
        assert!(entry.line_count > 0);
    }

    #[test]
    fn falls_back_to_main_when_selectors_miss() {
        // No `section.description` or `section.detail` - represents an
        // older or non-standard template. Walker should still pick up
        // text from `<main>`.
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Interface Listener</h1>
                <p>Receives game events.</p>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/foo/Listener.html", html).unwrap();
        assert_eq!(entry.kind, "interface");
        assert!(entry.body.contains("Receives game events"));
    }

    #[test]
    fn parses_normalized_type_search_index() {
        let js = r#"typeSearchIndex = [{p:"com.foo",l:"All Classes"},{p:"com.foo",c:"Bar"},{p:"com.foo.baz",c:"Qux",l:"Qux"}];updateSearchResults();"#;
        let parsed = parse_type_search_index(js).unwrap();
        // First record has empty `c`; the fetcher filters those, but
        // `parse_type_search_index` keeps them so callers can see what
        // the doclet actually emitted.
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].package, "com.foo");
        assert_eq!(parsed[1].class_name, "Bar");
        assert_eq!(parsed[2].package, "com.foo.baz");
        assert_eq!(parsed[2].class_name, "Qux");
    }

    #[test]
    fn extracts_per_method_docs_with_signatures() {
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Class ItemStack</h1>
                <section class="description">
                  <div class="block">A stack of items in a player inventory.</div>
                </section>
                <section class="detail">
                  <h3>setAmount</h3>
                  <div class="member-signature">public void setAmount(int amount)</div>
                  <div class="block">Sets the stack amount.</div>
                </section>
                <section class="detail">
                  <h3>setAmount</h3>
                  <div class="member-signature">public void setAmount(double amount)</div>
                  <div class="block">Sets a fractional amount.</div>
                </section>
                <section class="detail">
                  <h3>maxStackSize</h3>
                  <div class="member-signature">public int maxStackSize</div>
                  <div class="block">Field - should be skipped by methods extraction.</div>
                </section>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/hytale/server/api/ItemStack.html", html).unwrap();
        assert_eq!(entry.methods.len(), 2, "expected two methods, no field");
        assert_eq!(entry.methods[0].name, "setAmount");
        assert_eq!(entry.methods[0].param_simple_types, vec!["int"]);
        assert_eq!(
            entry.methods[0].return_type_simple_name.as_deref(),
            Some("void")
        );
        assert!(entry.methods[0].prose.contains("Sets the stack amount"));
        assert_eq!(entry.methods[1].param_simple_types, vec!["double"]);
    }

    #[test]
    fn captures_return_only_method_via_notes() {
        // Mirrors the CameraShake.getAssetStore() shape: no description
        // block, only a `@return` note. Before the fix this rendered as a
        // header-only inline card; now the prose carries the return text.
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Class CameraShake</h1>
                <section class="detail" id="getAssetStore()">
                  <h3>getAssetStore</h3>
                  <div class="member-signature">public static AssetStore getAssetStore()</div>
                  <dl class="notes">
                    <dt>Returns:</dt>
                    <dd>the asset store for CameraShake assets.</dd>
                  </dl>
                </section>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/hytale/CameraShake.html", html).unwrap();
        assert_eq!(entry.methods.len(), 1);
        let m = &entry.methods[0];
        assert_eq!(m.name, "getAssetStore");
        assert!(m.prose.contains("Returns:"));
        assert!(m.prose.contains("the asset store for CameraShake assets"));
    }

    #[test]
    fn captures_description_plus_parameter_notes() {
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Class ItemStack</h1>
                <section class="detail" id="ctor">
                  <h3>ItemStack</h3>
                  <div class="member-signature">public ItemStack(String itemId, int quantity)</div>
                  <div class="block">Constructor for an ItemStack instance.</div>
                  <dl class="notes">
                    <dt>Parameters:</dt>
                    <dd><code>itemId</code> - The block type key of the associated item.</dd>
                    <dd><code>quantity</code> - The quantity of this item stack.</dd>
                  </dl>
                </section>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/hytale/ItemStack.html", html).unwrap();
        assert_eq!(entry.methods.len(), 1);
        let prose = &entry.methods[0].prose;
        assert!(prose.starts_with("Constructor for an ItemStack instance."));
        assert!(prose.contains("Parameters:"));
        assert!(prose.contains("itemId - The block type key"));
        assert!(prose.contains("quantity - The quantity of this item stack."));
    }

    #[test]
    fn skips_methods_with_no_documentation_at_all() {
        let html = r#"
            <html><body>
              <main>
                <h1 class="title">Class Bare</h1>
                <section class="detail" id="documented">
                  <h3>documented</h3>
                  <div class="member-signature">public void documented()</div>
                  <div class="block">Has a description.</div>
                </section>
                <section class="detail" id="undocumented">
                  <h3>undocumented</h3>
                  <div class="member-signature">public void undocumented()</div>
                </section>
              </main>
            </body></html>
        "#;
        let entry = parse_class_page("com/hytale/Bare.html", html).unwrap();
        assert_eq!(entry.methods.len(), 1);
        assert_eq!(entry.methods[0].name, "documented");
    }

    #[test]
    fn signature_parser_handles_generics_and_qualified_names() {
        let (params, ret) = parse_method_signature(
            "public static <T> Optional<T> wrap(java.util.List<String> xs, int n)",
        );
        assert_eq!(params, vec!["List", "int"]);
        assert_eq!(ret.as_deref(), Some("Optional"));
    }

    #[test]
    fn signature_parser_handles_arrays_and_varargs() {
        let (params, _) = parse_method_signature("public void take(String[] arr, int... ns)");
        assert_eq!(params, vec!["String", "int"]);
    }

    #[test]
    fn signature_parser_handles_no_params() {
        let (params, ret) = parse_method_signature("public int hashCode()");
        assert!(params.is_empty());
        assert_eq!(ret.as_deref(), Some("int"));
    }

    #[test]
    fn missing_cache_dir_returns_empty() {
        let entries = walk_cache(Path::new("/nope/this/does/not/exist")).unwrap();
        assert!(entries.is_empty());
    }
}
