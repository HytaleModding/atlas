import { useEffect, useMemo } from "react";
import {
  AlertTriangle,
  ArrowLeft,
  Ban,
  ChevronRight,
  CircleCheck,
  Clock4,
  PencilLine,
  RefreshCw,
} from "lucide-react";

import {
  formatCandidate,
  severityLabel,
  type DiffEntry,
  type DiffSeverity,
  type Resolution,
} from "@/lib/diff";
import type { MountedIndexEntry } from "@/lib/fetcher";
import { useDiffStore } from "@/state/diffStore";
import { useFetchStore } from "@/state/fetchStore";
import { useProjectStore } from "@/state/projectStore";
import { useNavStore } from "@/state/navStore";

/**
 * "What would break in MY mod if Hytale shipped X?"
 *
 * The page has two states:
 *   1. Picker - choose project + baseline build + target build, hit run.
 *   2. Results - severity-grouped entries (removed / signature changed /
 *      deprecated / possibly renamed). Each entry can be expanded to show
 *      the full baseline-vs-target detail.
 *
 * Vocabulary policy: same as IndexCatalog. No "build", "mount", "manifest",
 * "artifact". Builds show as `{patchline} · {version}` (e.g. "release ·
 * 2026.04.22-89796e57b") which the backend already produces in
 * `lookup_mount`.
 */
export function DiffReportPage() {
  const projects = useProjectStore((s) => s.projects);
  const refreshProjects = useProjectStore((s) => s.refresh);
  const mounted = useFetchStore((s) => s.mounted);
  const refreshCatalog = useFetchStore((s) => s.refreshCatalog);
  const setPage = useNavStore((s) => s.setPage);

  const projectId = useDiffStore((s) => s.projectId);
  const baselineBuildId = useDiffStore((s) => s.baselineBuildId);
  const targetBuildId = useDiffStore((s) => s.targetBuildId);
  const loading = useDiffStore((s) => s.loading);
  const error = useDiffStore((s) => s.error);
  const report = useDiffStore((s) => s.report);
  const setTarget = useDiffStore((s) => s.setTarget);
  const setBaseline = useDiffStore((s) => s.setBaseline);
  const setTargetBuild = useDiffStore((s) => s.setTargetBuild);
  const run = useDiffStore((s) => s.run);
  const reset = useDiffStore((s) => s.reset);

  // Pull fresh data each time the page mounts. Both queries are cheap
  // (in-memory + small SQL respectively) and the page can otherwise
  // show stale "no projects yet" if the user just registered one.
  useEffect(() => {
    void refreshProjects();
    void refreshCatalog();
  }, [refreshProjects, refreshCatalog]);

  const indexedProjects = useMemo(
    () => projects.filter((p) => p.index_ready),
    [projects],
  );
  const releaseBuilds = useMemo(
    () =>
      mounted.filter(
        (m) => (m.manifest.hytale_patchline ?? "release") === "release",
      ),
    [mounted],
  );
  const preReleaseBuilds = useMemo(
    () => mounted.filter((m) => m.manifest.hytale_patchline === "pre-release"),
    [mounted],
  );

  const canRun =
    !!projectId && !!baselineBuildId && !!targetBuildId && !loading;

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between border-b border-border-subtle px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold text-fg-primary">
            Check what would break
          </h1>
          <p className="mt-0.5 text-xs text-fg-muted">
            Compare your project against an upcoming Hytale version to see
            what your code uses that&apos;s changed.
          </p>
        </div>
        <button
          type="button"
          onClick={() => {
            reset();
            setPage("catalog");
          }}
          className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated"
        >
          <ArrowLeft size={12} />
          Back
        </button>
      </header>

      <div className="flex flex-1 flex-col gap-6 overflow-y-auto px-6 py-5">
        <PickerCard
          projects={indexedProjects.map((p) => ({ id: p.id, name: p.name }))}
          baselines={releaseBuilds}
          targets={preReleaseBuilds.length > 0 ? preReleaseBuilds : releaseBuilds}
          projectId={projectId}
          baselineBuildId={baselineBuildId}
          targetBuildId={targetBuildId}
          onProjectChange={(id) => setTarget({ projectId: id })}
          onBaselineChange={setBaseline}
          onTargetChange={setTargetBuild}
          onRun={() => void run()}
          canRun={canRun}
          loading={loading}
        />

        {error && (
          <div className="flex items-start gap-2 rounded border border-status-error/40 bg-status-error/10 p-3 text-sm text-status-error">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        {report && <ResultsView report={report} />}

        {!report && !error && !loading && (
          <EmptyHint
            indexedProjects={indexedProjects.length}
            preReleaseBuilds={preReleaseBuilds.length}
          />
        )}
      </div>
    </div>
  );
}

function PickerCard({
  projects,
  baselines,
  targets,
  projectId,
  baselineBuildId,
  targetBuildId,
  onProjectChange,
  onBaselineChange,
  onTargetChange,
  onRun,
  canRun,
  loading,
}: {
  projects: { id: string; name: string }[];
  baselines: MountedIndexEntry[];
  targets: MountedIndexEntry[];
  projectId: string | null;
  baselineBuildId: string | null;
  targetBuildId: string | null;
  onProjectChange: (id: string) => void;
  onBaselineChange: (id: string | null) => void;
  onTargetChange: (id: string | null) => void;
  onRun: () => void;
  canRun: boolean;
  loading: boolean;
}) {
  return (
    <div className="rounded-md border border-border-subtle bg-bg-surface p-4">
      <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
        <PickerField label="Project">
          <select
            value={projectId ?? ""}
            onChange={(e) => onProjectChange(e.target.value)}
            className="w-full rounded border border-border-subtle bg-bg-base px-2 py-1.5 text-xs text-fg-primary"
            disabled={loading}
          >
            <option value="">Pick a project…</option>
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </PickerField>

        <PickerField label="What you build against today">
          <select
            value={baselineBuildId ?? ""}
            onChange={(e) =>
              onBaselineChange(e.target.value === "" ? null : e.target.value)
            }
            className="w-full rounded border border-border-subtle bg-bg-base px-2 py-1.5 text-xs text-fg-primary"
            disabled={loading}
          >
            <option value="">Pick a Hytale version…</option>
            {baselines.map((b) => (
              <option key={b.build_id} value={b.build_id}>
                {buildLabel(b)}
              </option>
            ))}
          </select>
        </PickerField>

        <PickerField label="What you&apos;re checking against">
          <select
            value={targetBuildId ?? ""}
            onChange={(e) =>
              onTargetChange(e.target.value === "" ? null : e.target.value)
            }
            className="w-full rounded border border-border-subtle bg-bg-base px-2 py-1.5 text-xs text-fg-primary"
            disabled={loading}
          >
            <option value="">Pick a Hytale version…</option>
            {targets.map((b) => (
              <option key={b.build_id} value={b.build_id}>
                {buildLabel(b)}
              </option>
            ))}
          </select>
        </PickerField>
      </div>

      <div className="mt-4 flex items-center justify-end gap-2">
        <button
          type="button"
          disabled={!canRun}
          onClick={onRun}
          className="flex items-center gap-1.5 rounded bg-accent-primary px-3 py-1.5 text-xs font-medium text-bg-base hover:opacity-90 disabled:opacity-50"
        >
          {loading ? (
            <>
              <RefreshCw size={12} className="animate-spin" />
              Checking…
            </>
          ) : (
            <>Run check</>
          )}
        </button>
      </div>
    </div>
  );
}

function PickerField({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className="text-[11px] font-medium uppercase tracking-wide text-fg-muted">
        {label}
      </span>
      {children}
    </label>
  );
}

function buildLabel(b: MountedIndexEntry): string {
  const patchline = b.manifest.hytale_patchline ?? "release";
  return `${patchline} · ${b.manifest.hytale_impl_version}`;
}

function EmptyHint({
  indexedProjects,
  preReleaseBuilds,
}: {
  indexedProjects: number;
  preReleaseBuilds: number;
}) {
  if (indexedProjects === 0) {
    return (
      <div className="rounded-md border border-dashed border-border-subtle bg-bg-surface p-6 text-center text-xs text-fg-muted">
        Add and index a project under <strong>Hytale Data → Your
        Projects</strong> first - the check needs your code to compare.
      </div>
    );
  }
  if (preReleaseBuilds === 0) {
    return (
      <div className="rounded-md border border-dashed border-border-subtle bg-bg-surface p-6 text-center text-xs text-fg-muted">
        No pre-release version installed yet. Atlas can still check between
        two release versions, but the typical use is &quot;will my code
        survive the next pre-release&quot;.
      </div>
    );
  }
  return (
    <div className="rounded-md border border-dashed border-border-subtle bg-bg-surface p-6 text-center text-xs text-fg-muted">
      Pick a project and two Hytale versions above, then hit{" "}
      <strong>Run check</strong>.
    </div>
  );
}

function ResultsView({ report }: { report: ReturnType<typeof useDiffStore.getState>["report"] }) {
  if (!report) return null;
  const total =
    report.removed.length +
    report.signature_changed.length +
    report.deprecated.length +
    report.renamed_likely.length;

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-baseline justify-between gap-3 rounded-md border border-border-subtle bg-bg-surface p-4">
        <div>
          <p className="text-sm text-fg-primary">
            <span className="font-mono text-xs text-fg-muted">
              {report.baseline_label}
            </span>
            {" → "}
            <span className="font-mono text-xs text-fg-muted">
              {report.target_label}
            </span>
          </p>
          <p className="mt-1 text-xs text-fg-muted">
            {total === 0
              ? "Nothing in your code looks like it would break."
              : `${total} reference${total === 1 ? "" : "s"} flagged.`}{" "}
            {report.unchanged_count} unchanged, {report.external_count} from
            outside Atlas (third-party, JDK, etc.) skipped.
          </p>
        </div>
        {total === 0 && (
          <span className="flex items-center gap-1.5 rounded bg-status-success/15 px-2 py-1 text-xs text-status-success">
            <CircleCheck size={12} />
            Looks safe
          </span>
        )}
      </div>

      <SeveritySection severity="removed" entries={report.removed} />
      <SeveritySection
        severity="signature_changed"
        entries={report.signature_changed}
      />
      <SeveritySection
        severity="renamed_likely"
        entries={report.renamed_likely}
      />
      <SeveritySection severity="deprecated" entries={report.deprecated} />
    </div>
  );
}

function SeveritySection({
  severity,
  entries,
}: {
  severity: DiffSeverity;
  entries: DiffEntry[];
}) {
  if (entries.length === 0) {
    return null; // Hide empty sections - keeps the page focused on the
                 // actionable stuff. The summary line at the top already
                 // tells the user nothing was found.
  }
  return (
    <section>
      <header className="mb-2 flex items-center gap-2 border-b border-border-subtle pb-1.5">
        <SeverityIcon severity={severity} />
        <h2 className="text-sm font-semibold text-fg-primary">
          {severityLabel(severity)}
        </h2>
        <span className="text-xs text-fg-muted">({entries.length})</span>
      </header>
      <ul className="flex flex-col gap-2">
        {entries.map((entry, i) => (
          <EntryRow key={`${entry.api_ref.source_path}:${entry.api_ref.line}:${i}`} entry={entry} />
        ))}
      </ul>
    </section>
  );
}

function SeverityIcon({ severity }: { severity: DiffSeverity }) {
  switch (severity) {
    case "removed":
      return <Ban size={14} className="text-status-error" />;
    case "signature_changed":
      return <PencilLine size={14} className="text-status-warning" />;
    case "deprecated":
      return <Clock4 size={14} className="text-status-warning" />;
    case "renamed_likely":
      return <ChevronRight size={14} className="text-fg-muted" />;
    case "unchanged":
      return <CircleCheck size={14} className="text-status-success" />;
  }
}

function EntryRow({ entry }: { entry: DiffEntry }) {
  const ref = entry.api_ref;
  const subject = ref.member_name
    ? `${ref.class_fqn}.${ref.member_name}`
    : ref.class_fqn;
  return (
    <li className="rounded-md border border-border-subtle bg-bg-surface p-3">
      <div className="flex items-baseline justify-between gap-3">
        <span className="truncate font-mono text-xs text-fg-primary">
          {subject}
        </span>
        <span className="shrink-0 font-mono text-[11px] text-fg-muted">
          {ref.source_path}:{ref.line}
        </span>
      </div>
      {entry.note && (
        <pre className="mt-2 whitespace-pre-wrap break-words rounded bg-bg-base px-2 py-1.5 font-mono text-[11px] text-fg-secondary">
          {entry.note}
        </pre>
      )}
      {(entry.baseline.outcome === "name_only" ||
        entry.target.outcome === "name_only") && (
        <details className="mt-2 text-xs text-fg-muted">
          <summary className="cursor-pointer">All overloads</summary>
          <ResolutionDetail label="Baseline" resolution={entry.baseline} />
          <ResolutionDetail label="Target" resolution={entry.target} />
        </details>
      )}
    </li>
  );
}

function ResolutionDetail({
  label,
  resolution,
}: {
  label: string;
  resolution: Resolution;
}) {
  let body: React.ReactNode;
  if (resolution.outcome === "found") {
    body = resolution.signature ?? "(no signature)";
  } else if (resolution.outcome === "name_only") {
    body = (
      <ul className="list-disc pl-4">
        {resolution.candidates.map((c, i) => (
          <li key={i}>{formatCandidate(c)}</li>
        ))}
      </ul>
    );
  } else {
    body = "not found";
  }
  return (
    <div className="mt-1.5 font-mono text-[11px]">
      <span className="text-fg-muted">{label}: </span>
      <span className="text-fg-secondary">{body}</span>
    </div>
  );
}
