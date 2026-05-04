//! Compare a single [`super::ApiRef`]'s baseline + target [`Resolution`]s
//! and classify the delta.
//!
//! Returns `Option<DiffEntry>`:
//! - `None`: ref is "external" - missing from both snapshots. Usually means
//!   the user imported a class from outside the indexed corpus (third-party
//!   dep, JDK type, etc.). The orchestrator counts these so the user can
//!   see we ignored them, but they're not actionable.
//! - `Some(entry)`: classified delta. `severity == Unchanged` is still
//!   surfaced here so the orchestrator can bump `unchanged_count` without
//!   needing to know the classification rules.
//!
//! The "renamed_likely" heuristic is a Levenshtein-distance scan over the
//! *target* class's member names. We only suggest a rename if the target
//! class still exists - otherwise the symbol moved or got nuked, which
//! "Removed" already conveys.
//!
//! Levenshtein is implemented inline (no `strsim` dep) because it's a
//! handful of lines and we don't need anything fancier.

use anyhow::Result;

use crate::indexer::symbols::SymbolsDb;

use super::{
    ApiKind, ApiRef, DiffEntry, DiffSeverity, MethodCandidate, Resolution,
};

/// Maximum edit distance for a name to count as "probably a rename".
/// Three is generous enough to catch `getName` → `getFullName` (4 - rejects)
/// and `setQty` → `setQuantity` (5 - rejects), but tight enough that
/// `add` and `get` don't collapse into each other.
const RENAME_MAX_DISTANCE: usize = 3;

/// Returns `Some(entry)` for a classifiable delta, or `None` if the ref
/// resolves nowhere (treated as "external" by the orchestrator).
pub fn compare_pair(
    api_ref: ApiRef,
    baseline: Resolution,
    target: Resolution,
    target_db: &SymbolsDb,
) -> Result<Option<DiffEntry>> {
    use Resolution::*;

    match (&baseline, &target) {
        // External: not in either snapshot. Caller bumps external_count.
        (NotFound, NotFound) => Ok(None),

        // Newly-appearing symbol. User's code works against target, which
        // is what they care about for the breakage report. Bucket as
        // Unchanged so it goes into the count and doesn't pollute the
        // actionable lists.
        (NotFound, _) => Ok(Some(unchanged(api_ref, baseline, target))),

        // Symbol existed in baseline, now gone from target. Either truly
        // removed or renamed - check the target class's other members.
        (_, NotFound) => {
            let suggestion = rename_suggestion(&api_ref, target_db)?;
            let (severity, note) = match suggestion {
                Some(name) => (
                    DiffSeverity::RenamedLikely,
                    Some(format!(
                        "{} not found in target; closest match on the class is `{}`",
                        member_label(&api_ref),
                        name
                    )),
                ),
                None => (
                    DiffSeverity::Removed,
                    Some(format!(
                        "{} was present in baseline but is gone from target",
                        member_label(&api_ref)
                    )),
                ),
            };
            Ok(Some(DiffEntry {
                severity,
                api_ref,
                baseline,
                target,
                note,
            }))
        }

        // Both sides have something. Walk the kind-specific paths.
        _ => Ok(Some(classify_present(api_ref, baseline, target))),
    }
}

/// Both baseline and target produced a Resolution other than NotFound.
/// Decide between SignatureChanged / Deprecated / Unchanged.
fn classify_present(
    api_ref: ApiRef,
    baseline: Resolution,
    target: Resolution,
) -> DiffEntry {
    match api_ref.kind {
        ApiKind::Class => classify_class(api_ref, baseline, target),
        ApiKind::Method => classify_method(api_ref, baseline, target),
        ApiKind::Field => classify_field(api_ref, baseline, target),
    }
}

fn classify_class(
    api_ref: ApiRef,
    baseline: Resolution,
    target: Resolution,
) -> DiffEntry {
    let base_mods = modifiers_of(&baseline);
    let target_mods = modifiers_of(&target);
    if newly_deprecated(base_mods, target_mods) {
        return DiffEntry {
            severity: DiffSeverity::Deprecated,
            api_ref,
            baseline,
            target,
            note: Some("class is now @Deprecated in target".to_string()),
        };
    }
    unchanged(api_ref, baseline, target)
}

fn classify_method(
    api_ref: ApiRef,
    baseline: Resolution,
    target: Resolution,
) -> DiffEntry {
    let base_overloads = method_overloads(&baseline);
    let target_overloads = method_overloads(&target);

    if !overload_lists_match(&base_overloads, &target_overloads) {
        let note = format!(
            "method overloads changed:\n  baseline: {}\n  target:   {}",
            render_overloads(&base_overloads),
            render_overloads(&target_overloads),
        );
        return DiffEntry {
            severity: DiffSeverity::SignatureChanged,
            api_ref,
            baseline,
            target,
            note: Some(note),
        };
    }

    // Same overload set; check if any picked up @Deprecated.
    let base_dep = base_overloads.iter().any(|c| has_deprecated(&c.modifiers));
    let target_dep = target_overloads
        .iter()
        .any(|c| has_deprecated(&c.modifiers));
    if !base_dep && target_dep {
        return DiffEntry {
            severity: DiffSeverity::Deprecated,
            api_ref,
            baseline,
            target,
            note: Some("method is now @Deprecated in target".to_string()),
        };
    }

    unchanged(api_ref, baseline, target)
}

fn classify_field(
    api_ref: ApiRef,
    baseline: Resolution,
    target: Resolution,
) -> DiffEntry {
    let base_sig = signature_of(&baseline);
    let target_sig = signature_of(&target);
    if base_sig != target_sig {
        let note = format!(
            "field type changed:\n  baseline: {}\n  target:   {}",
            base_sig.unwrap_or_else(|| "<unknown>".to_string()),
            target_sig.unwrap_or_else(|| "<unknown>".to_string()),
        );
        return DiffEntry {
            severity: DiffSeverity::SignatureChanged,
            api_ref,
            baseline,
            target,
            note: Some(note),
        };
    }
    if newly_deprecated(modifiers_of(&baseline), modifiers_of(&target)) {
        return DiffEntry {
            severity: DiffSeverity::Deprecated,
            api_ref,
            baseline,
            target,
            note: Some("field is now @Deprecated in target".to_string()),
        };
    }
    unchanged(api_ref, baseline, target)
}

fn unchanged(api_ref: ApiRef, baseline: Resolution, target: Resolution) -> DiffEntry {
    DiffEntry {
        severity: DiffSeverity::Unchanged,
        api_ref,
        baseline,
        target,
        note: None,
    }
}

fn modifiers_of(res: &Resolution) -> Option<&[String]> {
    match res {
        Resolution::Found { modifiers, .. } => Some(modifiers.as_slice()),
        Resolution::NameOnly { candidates } => candidates.first().map(|c| c.modifiers.as_slice()),
        Resolution::NotFound => None,
    }
}

fn signature_of(res: &Resolution) -> Option<String> {
    match res {
        Resolution::Found { signature, .. } => signature.clone(),
        _ => None,
    }
}

/// Pull the full overload list out of a method resolution. `Found` is
/// flattened into a single-candidate vec so the comparison logic doesn't
/// need to special-case it.
fn method_overloads(res: &Resolution) -> Vec<MethodCandidate> {
    match res {
        Resolution::Found {
            modifiers,
            param_types,
            return_type,
            ..
        } => vec![MethodCandidate {
            modifiers: modifiers.clone(),
            param_types: param_types.clone(),
            return_type: return_type.clone(),
        }],
        Resolution::NameOnly { candidates } => candidates.clone(),
        Resolution::NotFound => Vec::new(),
    }
}

/// Two overload lists "match" if each candidate in `a` has a structurally
/// equal partner in `b` (same param list and return type), and the lists
/// are the same length. Modifiers are intentionally excluded - those are
/// handled by the @Deprecated check.
fn overload_lists_match(a: &[MethodCandidate], b: &[MethodCandidate]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut used = vec![false; b.len()];
    for ca in a {
        let mut matched = false;
        for (i, cb) in b.iter().enumerate() {
            if used[i] {
                continue;
            }
            if ca.param_types == cb.param_types && ca.return_type == cb.return_type {
                used[i] = true;
                matched = true;
                break;
            }
        }
        if !matched {
            return false;
        }
    }
    true
}

fn render_overloads(list: &[MethodCandidate]) -> String {
    if list.is_empty() {
        return "<none>".to_string();
    }
    let mut parts: Vec<String> = list
        .iter()
        .map(|c| {
            let params = format!("({})", c.param_types.join(", "));
            match &c.return_type {
                Some(rt) => format!("{params} -> {rt}"),
                None => params,
            }
        })
        .collect();
    parts.sort();
    parts.join(" | ")
}

fn has_deprecated(mods: &[String]) -> bool {
    mods.iter().any(|m| {
        let t = m.trim();
        t == "@Deprecated" || t.ends_with(".Deprecated") || t == "Deprecated"
    })
}

fn newly_deprecated(base: Option<&[String]>, target: Option<&[String]>) -> bool {
    let b = base.map(has_deprecated).unwrap_or(false);
    let t = target.map(has_deprecated).unwrap_or(false);
    !b && t
}

fn member_label(api_ref: &ApiRef) -> String {
    match (&api_ref.kind, &api_ref.member_name) {
        (ApiKind::Class, _) => format!("class `{}`", api_ref.class_fqn),
        (ApiKind::Method, Some(name)) => {
            format!("method `{}.{}`", api_ref.class_fqn, name)
        }
        (ApiKind::Field, Some(name)) => {
            format!("field `{}.{}`", api_ref.class_fqn, name)
        }
        _ => format!("`{}`", api_ref.class_fqn),
    }
}

/// Look for a same-class member whose name is within `RENAME_MAX_DISTANCE`
/// edits of the user's referenced name. Only meaningful for Method / Field
/// refs (Class refs have no "other names on the class" to compare against).
fn rename_suggestion(api_ref: &ApiRef, target_db: &SymbolsDb) -> Result<Option<String>> {
    let Some(name) = api_ref.member_name.as_deref() else {
        return Ok(None);
    };
    let candidates = match api_ref.kind {
        ApiKind::Method => target_db.method_names_on_class(&api_ref.class_fqn)?,
        ApiKind::Field => target_db.field_names_on_class(&api_ref.class_fqn)?,
        ApiKind::Class => return Ok(None),
    };
    let mut best: Option<(String, usize)> = None;
    for cand in candidates {
        if cand == name {
            continue;
        }
        let d = levenshtein(name, &cand);
        if d > RENAME_MAX_DISTANCE {
            continue;
        }
        match &best {
            Some((_, bd)) if *bd <= d => {}
            _ => best = Some((cand, d)),
        }
    }
    Ok(best.map(|(name, _)| name))
}

/// Two-row Levenshtein. `O(len(a) * len(b))` time, `O(min(a, b))` space.
/// Inputs are short identifiers - we don't bother with SIMD or early exits.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::chunker::{
        ClassSymbol, FieldSymbol, FileSymbols, MethodSymbol, TypeKind,
    };
    use tempfile::tempdir;

    fn ref_method(name: &str) -> ApiRef {
        ApiRef {
            kind: ApiKind::Method,
            class_fqn: "com.example.Foo".into(),
            member_name: Some(name.into()),
            source_path: "X.java".into(),
            line: 1,
        }
    }

    fn build_db(methods: Vec<MethodSymbol>, fields: Vec<FieldSymbol>) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();
        let symbols = FileSymbols {
            classes: vec![ClassSymbol {
                fqn: "com.example.Foo".to_string(),
                simple_name: "Foo".to_string(),
                kind: TypeKind::Class,
                modifiers: vec!["public".to_string()],
                superclass: None,
                interfaces: vec![],
                start_line: 1,
                end_line: 100,
            }],
            methods,
            fields,
        };
        let tx = db.begin_write().unwrap();
        tx.insert_file("com/example/Foo.java", &symbols).unwrap();
        tx.commit().unwrap();
        (dir, path)
    }

    fn method(name: &str, ret: Option<&str>, params: &[&str]) -> MethodSymbol {
        MethodSymbol {
            class_fqn: "com.example.Foo".to_string(),
            name: name.to_string(),
            is_constructor: false,
            modifiers: vec!["public".to_string()],
            return_type: ret.map(|s| s.to_string()),
            param_types: params.iter().map(|s| s.to_string()).collect(),
            thrown: vec![],
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn external_returns_none() {
        let (_d, p) = build_db(vec![], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let r = compare_pair(
            ref_method("doThing"),
            Resolution::NotFound,
            Resolution::NotFound,
            &db,
        )
        .unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn removed_when_baseline_present_target_absent() {
        // Target db has class but no methods - so no rename candidates.
        let (_d, p) = build_db(vec![], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let baseline = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("() -> void".into()),
            param_types: vec![],
            return_type: Some("void".into()),
        };
        let r = compare_pair(ref_method("doThing"), baseline, Resolution::NotFound, &db)
            .unwrap()
            .unwrap();
        assert_eq!(r.severity, DiffSeverity::Removed);
    }

    #[test]
    fn rename_suggested_when_close_match_exists_on_class() {
        // Target class has `doThings` (one edit away from `doThing`).
        let (_d, p) = build_db(vec![method("doThings", Some("void"), &[])], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let baseline = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("() -> void".into()),
            param_types: vec![],
            return_type: Some("void".into()),
        };
        let r = compare_pair(ref_method("doThing"), baseline, Resolution::NotFound, &db)
            .unwrap()
            .unwrap();
        assert_eq!(r.severity, DiffSeverity::RenamedLikely);
        assert!(r.note.unwrap().contains("doThings"));
    }

    #[test]
    fn signature_change_detected_for_methods() {
        let (_d, p) = build_db(vec![], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let baseline = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("(String) -> int".into()),
            param_types: vec!["String".into()],
            return_type: Some("int".into()),
        };
        let target = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("(String, int) -> int".into()),
            param_types: vec!["String".into(), "int".into()],
            return_type: Some("int".into()),
        };
        let r = compare_pair(ref_method("doThing"), baseline, target, &db)
            .unwrap()
            .unwrap();
        assert_eq!(r.severity, DiffSeverity::SignatureChanged);
    }

    #[test]
    fn deprecated_added_in_target() {
        let (_d, p) = build_db(vec![], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let baseline = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("() -> void".into()),
            param_types: vec![],
            return_type: Some("void".into()),
        };
        let target = Resolution::Found {
            modifiers: vec!["public".into(), "@Deprecated".into()],
            signature: Some("() -> void".into()),
            param_types: vec![],
            return_type: Some("void".into()),
        };
        let r = compare_pair(ref_method("doThing"), baseline, target, &db)
            .unwrap()
            .unwrap();
        assert_eq!(r.severity, DiffSeverity::Deprecated);
    }

    #[test]
    fn unchanged_when_signatures_and_modifiers_match() {
        let (_d, p) = build_db(vec![], vec![]);
        let db = SymbolsDb::open_read_only(&p).unwrap();
        let r = Resolution::Found {
            modifiers: vec!["public".into()],
            signature: Some("() -> void".into()),
            param_types: vec![],
            return_type: Some("void".into()),
        };
        let entry = compare_pair(ref_method("doThing"), r.clone(), r, &db)
            .unwrap()
            .unwrap();
        assert_eq!(entry.severity, DiffSeverity::Unchanged);
    }
}
