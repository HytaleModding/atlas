/**
 * Feedback report shaping + GitHub Issues handoff.
 *
 * Reports are submitted by opening a pre-filled "new issue" URL in the
 * user's OS browser. No auth, no tokens, no hosted form endpoint -
 * GitHub does the rest. The user reviews the body in the browser before
 * clicking Create, so they always have a final say on what's sent.
 *
 * To point this at a different repo, change FEEDBACK_REPO. The URL
 * shape `https://github.com/<owner>/<repo>/issues/new?...` is GitHub's
 * documented quick-issue surface.
 */

import type { SearchHit } from "@/lib/indexer";
import type { Slot } from "@/lib/patcher";

/** Owner/repo where feedback issues land. Edit if Atlas's feedback
 *  destination ever moves. */
export const FEEDBACK_REPO = "HytaleModding/atlas";

export type FeedbackKind = "bug" | "tuning";

/** Common search-tuning failure modes. The dropdown stays small and
 *  scannable; "Other" is the catch-all. Verbiage is unified around
 *  short, declarative phrases the user can match to what they're
 *  seeing without parsing different sentence shapes. */
export const TUNING_PROBLEMS = [
  "Incorrect file ranking",
  "Relevant file missing",
  "Irrelevant file present",
  "Duplicate results",
  "Javadocs missing or wrong",
  "HytaleModding documentation issue",
  "Other",
] as const;
export type TuningProblem = (typeof TUNING_PROBLEMS)[number];

/** Compact, privacy-safe snapshot of what the user is seeing. Path,
 *  score, ranking signals - no file contents. */
export type ResultsSnapshot = {
  query: string;
  slot: Slot;
  section: string;
  hit_count: number;
  hits: Array<{
    rank: number;
    path: string;
    fqn: string;
    source_type: string;
    score: number;
    chunk_kind: string;
    symbol_name: string;
    preview_line: number | null;
  }>;
};

export type BugReport = {
  kind: "bug";
  title: string;
  description: string;
  hytale_version: HytaleVersionInfo;
};

export type TuningReport = {
  kind: "tuning";
  query: string;
  problem: TuningProblem;
  description: string;
  /** Specific files/guides the user wants triagers to focus on - items
   *  that should be present but aren't, or that are ranked too high
   *  or too low for the query. */
  callout: string;
  /** Either an auto-snapshot of what the user just saw, or a manually
   *  selected slot when the user opted out of the snapshot. */
  results_snapshot: ResultsSnapshot | null;
  manual_slot: Slot | null;
  hytale_version: HytaleVersionInfo;
};

export type FeedbackReport = BugReport | TuningReport;

/** Identifies which mounted Hytale build the report was filed against.
 *  Always attached - the user can't opt out. */
export type HytaleVersionInfo = {
  active_branch: Slot;
  /** ISO-8601 timestamp the active slot's index was last (re)built. */
  release_indexed_at: string | null;
  pre_release_indexed_at: string | null;
  app_version: string;
};

/** Build the snapshot from the in-memory hits the search page is showing. */
export function buildSnapshot(args: {
  query: string;
  slot: Slot;
  section: string;
  hits: SearchHit[];
}): ResultsSnapshot {
  return {
    query: args.query,
    slot: args.slot,
    section: args.section,
    hit_count: args.hits.length,
    hits: args.hits.map((h, i) => ({
      rank: i + 1,
      path: h.path,
      fqn: h.fqn,
      source_type: h.source_type,
      score: round4(h.score),
      chunk_kind: h.chunk_kind,
      symbol_name: h.symbol_name,
      preview_line: h.preview_line,
    })),
  };
}

/** Render a report to a GitHub-issue-friendly title + Markdown body, then
 *  open it via the OS browser. */
export function feedbackIssueUrl(report: FeedbackReport): string {
  const { title, body, labels } = renderReport(report);
  const params = new URLSearchParams();
  params.set("title", title);
  params.set("body", body);
  params.set("labels", labels.join(","));
  return `https://github.com/${FEEDBACK_REPO}/issues/new?${params.toString()}`;
}

function renderReport(r: FeedbackReport): {
  title: string;
  body: string;
  labels: string[];
} {
  if (r.kind === "bug") {
    return {
      title: r.title || "Bug report",
      body: [
        "## Description",
        "",
        r.description || "_(no description)_",
        "",
        "## Environment",
        "",
        renderVersionBlock(r.hytale_version),
      ].join("\n"),
      labels: ["feedback", "bug"],
    };
  }
  // tuning
  const lines: string[] = [];
  lines.push("## Query");
  lines.push("");
  lines.push("```");
  lines.push(r.query || "_(empty)_");
  lines.push("```");
  lines.push("");
  lines.push(`**Problem:** ${r.problem}`);
  lines.push("");
  // Callout comes before the free-text description in the rendered
  // issue so triagers see the named files before the prose.
  if (r.callout) {
    lines.push("## Specific results to focus on");
    lines.push("");
    lines.push(r.callout);
    lines.push("");
  }
  if (r.description) {
    lines.push("## What's wrong");
    lines.push("");
    lines.push(r.description);
    lines.push("");
  }
  lines.push("## Environment");
  lines.push("");
  lines.push(renderVersionBlock(r.hytale_version));
  lines.push("");
  if (r.results_snapshot) {
    lines.push("## Results snapshot");
    lines.push("");
    lines.push("<details><summary>Top results at the time of report</summary>");
    lines.push("");
    lines.push("```json");
    lines.push(JSON.stringify(r.results_snapshot, null, 2));
    lines.push("```");
    lines.push("");
    lines.push("</details>");
  } else if (r.manual_slot) {
    lines.push(`_Snapshot disabled. User-selected mount: \`${r.manual_slot}\`._`);
  }
  return {
    title: r.query ? `Search tuning: ${truncate(r.query, 60)}` : "Search tuning",
    body: lines.join("\n"),
    labels: ["feedback", "search-tuning"],
  };
}

function renderVersionBlock(v: HytaleVersionInfo): string {
  return [
    `- Active branch: \`${v.active_branch}\``,
    `- Release index built: ${v.release_indexed_at ?? "_(none)_"}`,
    `- Pre-release index built: ${v.pre_release_indexed_at ?? "_(none)_"}`,
    `- Atlas version: \`${v.app_version}\``,
  ].join("\n");
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : `${s.slice(0, n - 1)}…`;
}

function round4(n: number): number {
  return Math.round(n * 10000) / 10000;
}
