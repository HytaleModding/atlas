import { invoke } from "@tauri-apps/api/core";

/** Mirrors `diff::compare::CompareCounts`. */
export type CompareCounts = {
  classes: number;
  methods: number;
  fields: number;
};

/** Mirrors `diff::compare::CompareReport`. The list fields are capped at
 *  `MAX_LIST_LEN` on the backend (currently 500); use the `_total` counts
 *  for the actual delta size. */
export type CompareReport = {
  baseline_label: string;
  target_label: string;
  baseline_counts: CompareCounts;
  target_counts: CompareCounts;
  classes_added: string[];
  classes_added_total: number;
  classes_removed: string[];
  classes_removed_total: number;
  classes_shared: number;
};

/** Run a corpus-wide compare between two mounted Hytale versions. The
 *  payload is symmetric difference at class level - fast even on full
 *  Hytale builds because all the work is one SQL query per side. */
export async function indexCompare(args: {
  baselineBuildId: string;
  targetBuildId: string;
}): Promise<CompareReport> {
  return invoke<CompareReport>("index_compare", {
    baselineBuildId: args.baselineBuildId,
    targetBuildId: args.targetBuildId,
  });
}
