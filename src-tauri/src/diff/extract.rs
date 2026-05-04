//! Extract API references from a user's mod source folder.
//!
//! Walks every `.java` file under `project_dir`, parses with
//! tree-sitter-java, and emits one [`super::ApiRef`] per:
//!
//! - **Import declaration** - one `Class` ref per `import com.foo.Bar;`.
//!   Wildcard (`import com.foo.*`) and `import static` are skipped in V1.
//! - **Method invocation qualified by an imported simple class name** -
//!   e.g. `ItemStack.create(...)` if `com.hytale.foo.ItemStack` was
//!   imported. Emits a `Method` ref keyed on the imported FQN.
//! - **Field access qualified by an imported simple class name** - e.g.
//!   `Items.STONE` if `Items` was imported. Emits a `Field` ref.
//!
//! What we *don't* do (V1 cuts):
//!
//! - Type inference. `instance.method(...)` where `instance` is a method
//!   parameter doesn't resolve back to its declaring class without a
//!   real type checker, so we skip - the diff would be guessing.
//! - Cross-package same-class references. If user code lives in
//!   `com.hytale.foo` and uses `Bar` from the same package without an
//!   import, `Bar` won't be in our import map and we can't tag it.
//! - Static import bodies. `import static Foo.bar;` is rare in Hytale
//!   plugins and adds bookkeeping; defer.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tree_sitter::{Node, Parser, TreeCursor};
use walkdir::WalkDir;

use super::{ApiKind, ApiRef};

/// Walk `project_dir`, extract `ApiRef`s from every `.java` file. Errors
/// during a single file's parse are logged and skipped - one malformed
/// file shouldn't kill the whole diff.
pub fn extract_refs(project_dir: &Path) -> Result<Vec<ApiRef>> {
    if !project_dir.is_dir() {
        anyhow::bail!(
            "project source path is not a directory: {}",
            project_dir.display()
        );
    }

    let mut out = Vec::new();
    for entry in WalkDir::new(project_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_java = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("java"))
            .unwrap_or(false);
        if !is_java {
            continue;
        }
        let rel = path
            .strip_prefix(project_dir)
            .unwrap_or(path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(?err, file = %path.display(), "skipping unreadable .java");
                continue;
            }
        };
        if let Err(err) = extract_from_source(&source, &rel, &mut out) {
            tracing::warn!(?err, file = %path.display(), "skipping unparseable .java");
        }
    }
    Ok(out)
}

/// Public for tests: parse one file's source and append refs to `out`.
pub fn extract_from_source(source: &str, rel_path: &str, out: &mut Vec<ApiRef>) -> Result<()> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::language())
        .context("loading tree-sitter-java grammar")?;
    let Some(tree) = parser.parse(source, None) else {
        anyhow::bail!("tree-sitter failed to parse {rel_path}");
    };
    let bytes = source.as_bytes();
    let root = tree.root_node();

    // First pass: gather imports → simple-name → FQN map. Also emits
    // one Class ref per non-wildcard import.
    let mut imports: HashMap<String, String> = HashMap::new();
    collect_imports(root, bytes, rel_path, &mut imports, out);

    // Second pass: walk for method invocations + field accesses whose
    // qualifier matches an imported simple name.
    let mut cursor = root.walk();
    walk_for_uses(&mut cursor, bytes, rel_path, &imports, out);

    Ok(())
}

/// Walk top-level children for `import_declaration` nodes; record one
/// `Class` ApiRef per concrete import and stash `simple_name -> fqn` so
/// the use-site walk knows which qualifiers point at imported types.
fn collect_imports(
    root: Node<'_>,
    bytes: &[u8],
    rel_path: &str,
    imports: &mut HashMap<String, String>,
    out: &mut Vec<ApiRef>,
) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "import_declaration" {
            continue;
        }
        // Static imports look like `import static com.foo.Bar.MEMBER;`
        // - we deliberately skip these in V1 (see module docs).
        if has_static_keyword(child, bytes) {
            continue;
        }
        // Wildcard? `import com.foo.*;` has an `asterisk` child token.
        // Skip - tracking every type a wildcard could pull in is too
        // noisy for V1.
        if has_asterisk(child) {
            continue;
        }
        // The dotted FQN is the first (and usually only) named child of
        // kind `scoped_identifier` or `identifier`.
        let mut inner = child.walk();
        for n in child.named_children(&mut inner) {
            if matches!(n.kind(), "scoped_identifier" | "identifier") {
                if let Some(fqn) = node_text(n, bytes) {
                    let fqn = fqn.trim().to_string();
                    if fqn.is_empty() {
                        break;
                    }
                    let simple = simple_name(&fqn);
                    imports.insert(simple.to_string(), fqn.clone());
                    out.push(ApiRef {
                        kind: ApiKind::Class,
                        class_fqn: fqn,
                        member_name: None,
                        source_path: rel_path.to_string(),
                        line: (n.start_position().row + 1) as u32,
                    });
                }
                break;
            }
        }
    }
}

fn has_static_keyword(node: Node<'_>, bytes: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Anonymous tokens inside import_declaration: `import`, `static`, `;`.
        if child.kind() == "static" {
            return true;
        }
        // Defensive: some grammars expose the keyword via byte text on
        // an unnamed node. Cheap fallback.
        if !child.is_named() {
            if let Some(t) = node_text(child, bytes) {
                if t.trim() == "static" {
                    return true;
                }
            }
        }
    }
    false
}

fn has_asterisk(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "asterisk" || child.kind() == "*" {
            return true;
        }
    }
    false
}

/// Recursive walk emitting `Method` / `Field` refs whose qualifier
/// matches an imported simple class name. Iterative would be lower
/// stack pressure but the shape mirrors `chunker::walk` for consistency.
fn walk_for_uses(
    cursor: &mut TreeCursor<'_>,
    bytes: &[u8],
    rel_path: &str,
    imports: &HashMap<String, String>,
    out: &mut Vec<ApiRef>,
) {
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        match node.kind() {
            "method_invocation" => visit_method_invocation(node, bytes, rel_path, imports, out),
            "field_access" => visit_field_access(node, bytes, rel_path, imports, out),
            _ => {}
        }
        // Recurse regardless: a method_invocation can contain nested
        // calls in its argument list.
        walk_for_uses(cursor, bytes, rel_path, imports, out);
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    cursor.goto_parent();
}

fn visit_method_invocation(
    node: Node<'_>,
    bytes: &[u8],
    rel_path: &str,
    imports: &HashMap<String, String>,
    out: &mut Vec<ApiRef>,
) {
    // tree-sitter-java fields:
    //   object: receiver expression (optional)
    //   name:   method identifier
    //   arguments: argument_list
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(method_name) = node_text(name_node, bytes) else {
        return;
    };
    let Some(object) = node.child_by_field_name("object") else {
        // Bare call (`foo()` / `this.foo()` is a different shape) -
        // can't resolve without type inference. Skip.
        return;
    };
    // Only handle the simple case: object is a single identifier whose
    // text matches an imported class simple name.
    if object.kind() != "identifier" {
        return;
    }
    let Some(qualifier) = node_text(object, bytes) else {
        return;
    };
    let Some(class_fqn) = imports.get(qualifier.as_str()) else {
        return;
    };
    out.push(ApiRef {
        kind: ApiKind::Method,
        class_fqn: class_fqn.clone(),
        member_name: Some(method_name),
        source_path: rel_path.to_string(),
        line: (node.start_position().row + 1) as u32,
    });
}

fn visit_field_access(
    node: Node<'_>,
    bytes: &[u8],
    rel_path: &str,
    imports: &HashMap<String, String>,
    out: &mut Vec<ApiRef>,
) {
    let Some(field_node) = node.child_by_field_name("field") else {
        return;
    };
    let Some(field_name) = node_text(field_node, bytes) else {
        return;
    };
    let Some(object) = node.child_by_field_name("object") else {
        return;
    };
    if object.kind() != "identifier" {
        return;
    }
    let Some(qualifier) = node_text(object, bytes) else {
        return;
    };
    let Some(class_fqn) = imports.get(qualifier.as_str()) else {
        return;
    };
    out.push(ApiRef {
        kind: ApiKind::Field,
        class_fqn: class_fqn.clone(),
        member_name: Some(field_name),
        source_path: rel_path.to_string(),
        line: (node.start_position().row + 1) as u32,
    });
}

fn simple_name(fqn: &str) -> &str {
    fqn.rsplit_once('.').map(|(_, s)| s).unwrap_or(fqn)
}

fn node_text(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    node.utf8_text(bytes).ok().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> Vec<ApiRef> {
        let mut out = Vec::new();
        extract_from_source(source, "Test.java", &mut out).unwrap();
        out
    }

    #[test]
    fn extracts_class_ref_from_import() {
        let src = "package x;\nimport com.hytale.foo.Bar;\nclass T {}\n";
        let refs = run(src);
        assert!(refs.iter().any(|r| r.kind == ApiKind::Class
            && r.class_fqn == "com.hytale.foo.Bar"));
    }

    #[test]
    fn skips_wildcard_and_static_imports() {
        let src = "import com.hytale.foo.*;\nimport static com.hytale.foo.Bar.MEMBER;\nclass T {}\n";
        let refs = run(src);
        assert!(refs.is_empty(), "expected no refs, got {refs:?}");
    }

    #[test]
    fn extracts_method_ref_when_qualifier_is_imported() {
        let src = "import com.hytale.foo.ItemStack;\n\
                   class T { void use(){ ItemStack.create(\"foo\"); }}\n";
        let refs = run(src);
        assert!(refs.iter().any(|r| r.kind == ApiKind::Method
            && r.class_fqn == "com.hytale.foo.ItemStack"
            && r.member_name.as_deref() == Some("create")));
    }

    #[test]
    fn skips_method_call_on_unimported_qualifier() {
        // `Math` not imported (and not in a Hytale-style FQN); we
        // shouldn't emit a Method ref for it.
        let src = "class T { int x = Math.max(1,2); }\n";
        let refs = run(src);
        assert!(
            refs.iter()
                .all(|r| r.kind != ApiKind::Method || r.class_fqn != "Math"),
            "got Method ref for unimported qualifier: {refs:?}"
        );
    }

    #[test]
    fn extracts_field_ref_when_qualifier_is_imported() {
        let src = "import com.hytale.foo.Items;\n\
                   class T { Object x = Items.STONE; }\n";
        let refs = run(src);
        assert!(refs.iter().any(|r| r.kind == ApiKind::Field
            && r.class_fqn == "com.hytale.foo.Items"
            && r.member_name.as_deref() == Some("STONE")));
    }
}
