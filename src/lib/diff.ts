import { invoke } from "@tauri-apps/api/core";

/** Mirrors `diff::ApiKind` on the Rust side. */
export type ApiKind = "class" | "method" | "field";

/** Mirrors `diff::ApiRef`. */
export type ApiRef = {
  kind: ApiKind;
  class_fqn: string;
  member_name: string | null;
  source_path: string;
  line: number;
};

/** Mirrors `diff::Resolution`. The `outcome` discriminator matches the
 *  serde-tagged enum on the backend. */
export type Resolution =
  | {
      outcome: "found";
      modifiers: string[];
      signature: string | null;
      param_types: string[];
      return_type: string | null;
    }
  | {
      outcome: "name_only";
      candidates: MethodCandidate[];
    }
  | { outcome: "not_found" };

export type MethodCandidate = {
  modifiers: string[];
  return_type: string | null;
  param_types: string[];
};

/** Mirrors `diff::DiffSeverity`. `unchanged` never appears in the report
 *  body - it's only counted via `unchanged_count`. */
export type DiffSeverity =
  | "removed"
  | "signature_changed"
  | "deprecated"
  | "renamed_likely"
  | "unchanged";

export type DiffEntry = {
  severity: DiffSeverity;
  api_ref: ApiRef;
  baseline: Resolution;
  target: Resolution;
  note: string | null;
};

/** Mirrors `diff::DiffReport`. The four entry buckets are pre-grouped
 *  on the backend so the page just renders sections. */
export type DiffReport = {
  baseline_label: string;
  target_label: string;
  removed: DiffEntry[];
  signature_changed: DiffEntry[];
  deprecated: DiffEntry[];
  renamed_likely: DiffEntry[];
  unchanged_count: number;
  external_count: number;
};

/** Run the diff. Synchronous from the frontend's POV - the backend
 *  resolves project source + symbols.sqlite paths and returns the full
 *  report in one round-trip. */
export function diffRun(args: {
  projectId: string;
  baselineBuildId: string;
  targetBuildId: string;
}): Promise<DiffReport> {
  return invoke<DiffReport>("diff_run", {
    projectId: args.projectId,
    baselineBuildId: args.baselineBuildId,
    targetBuildId: args.targetBuildId,
  });
}

/** Friendly labels for severity buckets. Keeps DiffReport.tsx free of
 *  scattered string literals and centralises the user-facing wording. */
export function severityLabel(severity: DiffSeverity): string {
  switch (severity) {
    case "removed":
      return "Will break";
    case "signature_changed":
      return "Signature changed";
    case "deprecated":
      return "Deprecated";
    case "renamed_likely":
      return "Possibly renamed";
    case "unchanged":
      return "Unchanged";
  }
}

/** Render a method candidate as a one-line signature. Used by the
 *  details disclosure so the user can see exactly what changed. */
export function formatCandidate(c: MethodCandidate): string {
  const params = `(${c.param_types.join(", ")})`;
  return c.return_type ? `${params} -> ${c.return_type}` : params;
}
