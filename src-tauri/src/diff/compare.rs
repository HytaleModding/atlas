//! Corpus-wide compare between two `symbols.sqlite` snapshots.
//!
//! Distinct from `super::report` (which compares one user-project ref pair):
//! this scans the entire class table on both sides and computes the
//! symmetric difference at the class level. The output powers the
//! "what changed between Hytale X and Hytale Y" panel.
//!
//! V1 stays at class granularity. Per-method drilldown lives behind a
//! follow-up - the typical user reads "200 classes added, 5 removed,
//! these 5: ..." and that's enough signal to decide whether to dig in.

use anyhow::Result;
use serde::Serialize;

use crate::indexer::symbols::SymbolsDb;

/// Cap on the per-bucket lists so a wholesale rename doesn't ship a
/// 50k-entry payload to the frontend. The full counts still come back
/// in the summary - we just truncate the example list.
pub const MAX_LIST_LEN: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct CompareCounts {
    pub classes: u64,
    pub methods: u64,
    pub fields: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareReport {
    pub baseline_label: String,
    pub target_label: String,
    pub baseline_counts: CompareCounts,
    pub target_counts: CompareCounts,
    /// Class FQNs present in target but not baseline. Capped at
    /// [`MAX_LIST_LEN`]; total is in `classes_added_total`.
    pub classes_added: Vec<String>,
    pub classes_added_total: usize,
    /// Class FQNs present in baseline but not target.
    pub classes_removed: Vec<String>,
    pub classes_removed_total: usize,
    /// Number of class FQNs present in both.
    pub classes_shared: usize,
}

/// Read both DBs, compute counts + symmetric difference at class level.
pub fn compare(
    baseline: &SymbolsDb,
    baseline_label: String,
    target: &SymbolsDb,
    target_label: String,
) -> Result<CompareReport> {
    let baseline_counts = counts(baseline)?;
    let target_counts = counts(target)?;

    // Both lists already come back sorted from `all_class_fqns`.
    let base_fqns = baseline.all_class_fqns()?;
    let target_fqns = target.all_class_fqns()?;

    let base_set: std::collections::BTreeSet<&str> =
        base_fqns.iter().map(|s| s.as_str()).collect();
    let target_set: std::collections::BTreeSet<&str> =
        target_fqns.iter().map(|s| s.as_str()).collect();

    let added_iter: Vec<String> = target_set
        .difference(&base_set)
        .map(|s| (*s).to_string())
        .collect();
    let removed_iter: Vec<String> = base_set
        .difference(&target_set)
        .map(|s| (*s).to_string())
        .collect();
    let shared = base_set.intersection(&target_set).count();

    let classes_added_total = added_iter.len();
    let classes_removed_total = removed_iter.len();

    let classes_added: Vec<String> =
        added_iter.into_iter().take(MAX_LIST_LEN).collect();
    let classes_removed: Vec<String> =
        removed_iter.into_iter().take(MAX_LIST_LEN).collect();

    Ok(CompareReport {
        baseline_label,
        target_label,
        baseline_counts,
        target_counts,
        classes_added,
        classes_added_total,
        classes_removed,
        classes_removed_total,
        classes_shared: shared,
    })
}

fn counts(db: &SymbolsDb) -> Result<CompareCounts> {
    let r = db.row_counts()?;
    Ok(CompareCounts {
        classes: r.classes,
        methods: r.methods,
        fields: r.fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::chunker::{ClassSymbol, FileSymbols, TypeKind};
    use tempfile::tempdir;

    fn build_db(class_fqns: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("symbols.sqlite");
        let mut db = SymbolsDb::create(&path).unwrap();
        let symbols = FileSymbols {
            classes: class_fqns
                .iter()
                .map(|f| ClassSymbol {
                    fqn: f.to_string(),
                    simple_name: f.rsplit('.').next().unwrap().to_string(),
                    kind: TypeKind::Class,
                    modifiers: vec!["public".to_string()],
                    superclass: None,
                    interfaces: vec![],
                    start_line: 1,
                    end_line: 10,
                })
                .collect(),
            methods: vec![],
            fields: vec![],
        };
        let tx = db.begin_write().unwrap();
        // Bundle all classes under one synthetic file - the path doesn't
        // matter for the compare path, only the class table contents do.
        tx.insert_file("synthetic.java", &symbols).unwrap();
        tx.commit().unwrap();
        (dir, path)
    }

    #[test]
    fn computes_added_removed_shared() {
        let (_d1, p1) = build_db(&["com.example.A", "com.example.B"]);
        let (_d2, p2) = build_db(&["com.example.B", "com.example.C"]);
        let baseline = SymbolsDb::open_read_only(&p1).unwrap();
        let target = SymbolsDb::open_read_only(&p2).unwrap();
        let r = compare(
            &baseline,
            "release · v1".into(),
            &target,
            "pre-release · v2".into(),
        )
        .unwrap();
        assert_eq!(r.classes_added, vec!["com.example.C".to_string()]);
        assert_eq!(r.classes_removed, vec!["com.example.A".to_string()]);
        assert_eq!(r.classes_shared, 1);
        assert_eq!(r.classes_added_total, 1);
        assert_eq!(r.classes_removed_total, 1);
    }
}
