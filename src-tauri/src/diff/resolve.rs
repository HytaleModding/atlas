//! Resolve a single [`super::ApiRef`] against one `symbols.sqlite`
//! snapshot.
//!
//! Output is a [`super::Resolution`]:
//!
//! - `Found`: the symbol exists on the snapshot at the expected
//!   `(class_fqn, kind, member_name)`. For methods we collapse to
//!   "found" only when there's a single overload - multi-overload
//!   classes still funnel through `Found` with the first row's
//!   modifiers/signature, which is enough for the deprecation /
//!   "still there" case. Signature-level diffing happens in
//!   [`crate::diff::report`] using the full overload list via
//!   `methods_by_name`.
//! - `NameOnly`: methods only - the name is on the class but we surface
//!   all overloads so the reporter can compare lists across snapshots.
//! - `NotFound`: nothing matches.

use anyhow::Result;

use crate::indexer::symbols::{DiffMethodRow, SymbolsDb};

use super::{ApiKind, ApiRef, MethodCandidate, Resolution};

pub fn resolve(db: &SymbolsDb, api_ref: &ApiRef) -> Result<Resolution> {
    match api_ref.kind {
        ApiKind::Class => resolve_class(db, &api_ref.class_fqn),
        ApiKind::Method => {
            let Some(name) = api_ref.member_name.as_deref() else {
                return Ok(Resolution::NotFound);
            };
            resolve_method(db, &api_ref.class_fqn, name)
        }
        ApiKind::Field => {
            let Some(name) = api_ref.member_name.as_deref() else {
                return Ok(Resolution::NotFound);
            };
            resolve_field(db, &api_ref.class_fqn, name)
        }
    }
}

fn resolve_class(db: &SymbolsDb, class_fqn: &str) -> Result<Resolution> {
    match db.class_modifiers(class_fqn)? {
        Some(modifiers) => Ok(Resolution::Found {
            modifiers,
            signature: None,
            param_types: Vec::new(),
            return_type: None,
        }),
        None => Ok(Resolution::NotFound),
    }
}

fn resolve_method(db: &SymbolsDb, class_fqn: &str, name: &str) -> Result<Resolution> {
    let rows = db.methods_by_name(class_fqn, name)?;
    if rows.is_empty() {
        return Ok(Resolution::NotFound);
    }
    // Always carry overloads so the reporter can compare signature
    // lists across snapshots without a second query.
    let candidates: Vec<MethodCandidate> = rows
        .iter()
        .map(|r: &DiffMethodRow| MethodCandidate {
            modifiers: r.modifiers.clone(),
            return_type: r.return_type.clone(),
            param_types: r.param_types.clone(),
        })
        .collect();
    if candidates.len() == 1 {
        // Single overload: emit `Found` with the canonical signature so
        // the report renders cleanly without needing to dive into the
        // candidate list.
        let only = &candidates[0];
        return Ok(Resolution::Found {
            modifiers: only.modifiers.clone(),
            signature: Some(format_signature(&only.return_type, &only.param_types)),
            param_types: only.param_types.clone(),
            return_type: only.return_type.clone(),
        });
    }
    Ok(Resolution::NameOnly { candidates })
}

fn resolve_field(db: &SymbolsDb, class_fqn: &str, name: &str) -> Result<Resolution> {
    match db.field_by_name(class_fqn, name)? {
        Some(row) => Ok(Resolution::Found {
            modifiers: row.modifiers,
            signature: Some(row.type_text),
            param_types: Vec::new(),
            return_type: None,
        }),
        None => Ok(Resolution::NotFound),
    }
}

fn format_signature(return_type: &Option<String>, params: &[String]) -> String {
    let params_part = format!("({})", params.join(", "));
    match return_type {
        Some(rt) => format!("{params_part} -> {rt}"),
        None => params_part,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::chunker::{
        ClassSymbol, FieldSymbol, FileSymbols, MethodSymbol, TypeKind,
    };
    use tempfile::tempdir;

    fn build_db_with_one_class(deprecated: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();
        let mut modifiers = vec!["public".to_string()];
        if deprecated {
            modifiers.push("@Deprecated".to_string());
        }
        let symbols = FileSymbols {
            classes: vec![ClassSymbol {
                fqn: "com.example.Foo".to_string(),
                simple_name: "Foo".to_string(),
                kind: TypeKind::Class,
                modifiers,
                superclass: None,
                interfaces: vec![],
                start_line: 1,
                end_line: 10,
            }],
            methods: vec![MethodSymbol {
                class_fqn: "com.example.Foo".to_string(),
                name: "doThing".to_string(),
                is_constructor: false,
                modifiers: vec!["public".to_string()],
                return_type: Some("int".to_string()),
                param_types: vec!["String".to_string()],
                thrown: vec![],
                start_line: 3,
                end_line: 5,
            }],
            fields: vec![FieldSymbol {
                class_fqn: "com.example.Foo".to_string(),
                name: "counter".to_string(),
                type_text: "int".to_string(),
                modifiers: vec!["public".to_string()],
                start_line: 7,
                end_line: 7,
            }],
        };
        let tx = db.begin_write().unwrap();
        tx.insert_file("com/example/Foo.java", &symbols).unwrap();
        tx.commit().unwrap();
        (dir, path)
    }

    #[test]
    fn class_ref_resolves_when_present() {
        let (_dir, path) = build_db_with_one_class(false);
        let db = SymbolsDb::open_read_only(&path).unwrap();
        let r = resolve(
            &db,
            &ApiRef {
                kind: ApiKind::Class,
                class_fqn: "com.example.Foo".into(),
                member_name: None,
                source_path: "X.java".into(),
                line: 1,
            },
        )
        .unwrap();
        assert!(matches!(r, Resolution::Found { .. }));
    }

    #[test]
    fn method_ref_returns_found_for_single_overload() {
        let (_dir, path) = build_db_with_one_class(false);
        let db = SymbolsDb::open_read_only(&path).unwrap();
        let r = resolve(
            &db,
            &ApiRef {
                kind: ApiKind::Method,
                class_fqn: "com.example.Foo".into(),
                member_name: Some("doThing".into()),
                source_path: "X.java".into(),
                line: 1,
            },
        )
        .unwrap();
        match r {
            Resolution::Found { signature, .. } => {
                assert_eq!(signature.as_deref(), Some("(String) -> int"));
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn missing_member_returns_not_found() {
        let (_dir, path) = build_db_with_one_class(false);
        let db = SymbolsDb::open_read_only(&path).unwrap();
        let r = resolve(
            &db,
            &ApiRef {
                kind: ApiKind::Method,
                class_fqn: "com.example.Foo".into(),
                member_name: Some("nope".into()),
                source_path: "X.java".into(),
                line: 1,
            },
        )
        .unwrap();
        assert!(matches!(r, Resolution::NotFound));
    }
}
