import { useEffect } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  Database,
  RefreshCw,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  fetchPhaseLabel,
  formatBytes,
  type FetchPhase,
  type MountedIndexEntry,
} from "@/lib/fetcher";
import { useFetchStore } from "@/state/fetchStore";

/**
 * Hytale Data: the user-facing read-only status view.
 *
 * Shows which Hytale versions Atlas has reference data for, when it
 * was last updated, and whether it's current. Wholly read-only from
 * the user's POV: there's no "Mount from file" button here, no
 * "build_id" hex, no signing fingerprints. The escape hatch for dev
 * iteration lives in Settings → Developer.
 *
 * Vocabulary policy: nothing on this page should mention "artifact",
 * "mount", "manifest", "schema version", or other infra terms. See
 * memory/feedback_no_ui_jargon.md for the full ban list.
 */
export function IndexCatalog() {
  const mounted = useFetchStore((s) => s.mounted);
  const status = useFetchStore((s) => s.status);
  const activeBuildId = useFetchStore((s) => s.activeBuildId);
  const progress = useFetchStore((s) => s.progress);
  const errorMessage = useFetchStore((s) => s.errorMessage);
  const refreshing = useFetchStore((s) => s.refreshing);
  const refreshCatalog = useFetchStore((s) => s.refreshCatalog);
  const clearError = useFetchStore((s) => s.clearError);

  useEffect(() => {
    void refreshCatalog();
  }, [refreshCatalog]);

  const fetchBusy =
    status.kind === "phase" ||
    status.kind === "downloading" ||
    status.kind === "extracting";

  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between border-b border-border-subtle px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold text-fg-primary">
            Hytale Data
          </h1>
          <p className="text-xs text-fg-muted">
            The Hytale source code, modding guides, and API docs Atlas
            uses to answer your searches.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void refreshCatalog()}
          disabled={refreshing}
          className="flex items-center gap-2 rounded border border-border-subtle px-3 py-1.5 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
        >
          <RefreshCw
            size={14}
            className={cn(refreshing && "animate-spin")}
          />
          Refresh
        </button>
      </header>

      <div className="flex-1 overflow-auto px-6 py-4">
        {errorMessage && (
          <div className="mb-4 flex items-start gap-2 rounded border border-status-error/40 bg-status-error/10 p-2 text-xs text-status-error">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" />
            <span className="flex-1">{errorMessage}</span>
            <button
              type="button"
              onClick={clearError}
              className="text-fg-muted hover:text-fg-primary"
            >
              Dismiss
            </button>
          </div>
        )}

        {fetchBusy && activeBuildId && (
          <div className="mb-4">
            <DataProgressBanner
              phase={progress.phase}
              current={progress.current}
              total={progress.total}
            />
          </div>
        )}

        {status.kind === "done" && (
          <div className="mb-4 flex items-center gap-2 rounded border border-status-ok/40 bg-status-ok/10 p-2 text-xs text-status-ok">
            <CheckCircle2 size={14} />
            Updated successfully.
          </div>
        )}

        {mounted.length === 0 ? (
          <EmptyState />
        ) : (
          <ul className="flex flex-col gap-2">
            {mounted.map((entry) => (
              <DataRow key={entry.build_id} entry={entry} />
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex flex-col items-center justify-center gap-3 rounded-md border border-dashed border-border-subtle bg-bg-surface p-10 text-center">
      <Database size={28} strokeWidth={1.5} className="text-fg-muted" />
      <div>
        <p className="text-sm font-medium text-fg-primary">
          No Hytale data loaded yet.
        </p>
        <p className="mt-1 max-w-sm text-xs text-fg-muted">
          Atlas downloads Hytale&apos;s source code, modding guides, and
          API docs the first time you set it up. We&apos;ll guide you
          through it on next launch.
        </p>
      </div>
    </div>
  );
}

/**
 * One row in the data list. Shows only what a user cares about:
 * which Hytale version, which channel (Release vs Pre-release), when
 * Atlas got it, and how big it is. The technical details
 * (fingerprints, schema versions, embedder IDs, file paths) are
 * hidden; they're available via developer tooling if needed.
 */
function DataRow({ entry }: { entry: MountedIndexEntry }) {
  const { manifest } = entry;
  const channel =
    manifest.hytale_patchline === "pre-release" ? "Pre-release" : "Release";
  const version = manifest.hytale_impl_version || "Unknown version";

  return (
    <li className="rounded-md border border-border-subtle bg-bg-surface p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="text-sm font-medium text-fg-primary">
              Hytale {shortenVersion(version)}
            </span>
            <span className="rounded bg-bg-elevated px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-fg-secondary">
              {channel}
            </span>
          </div>
          <div className="mt-1 text-xs text-fg-muted">
            Added {shortDate(manifest.created_at)}
          </div>
        </div>
        <div className="shrink-0 text-right text-xs text-fg-muted">
          {formatBytes(entry.size_bytes)}
        </div>
      </div>
    </li>
  );
}

function DataProgressBanner({
  phase,
  current,
  total,
}: {
  phase: FetchPhase | null;
  current: number;
  total: number | null;
}) {
  const label = phase ? friendlyPhaseLabel(phase) : "Getting ready…";
  const pct =
    total && total > 0
      ? Math.min(100, Math.round((current / total) * 100))
      : null;
  const bytesLabel =
    phase === "downloading" && total
      ? `${formatBytes(current)} / ${formatBytes(total)}`
      : null;

  return (
    <div className="rounded border border-border-subtle bg-bg-surface p-3 text-xs">
      <div className="mb-1.5 flex items-center justify-between">
        <span className="text-fg-primary">{label}</span>
        {bytesLabel && <span className="text-fg-muted">{bytesLabel}</span>}
      </div>
      <div className="h-1 w-full overflow-hidden rounded bg-bg-elevated">
        <div
          className={cn(
            "h-full bg-accent-primary transition-all",
            pct === null && "animate-pulse opacity-60",
          )}
          style={{ width: pct !== null ? `${pct}%` : "100%" }}
        />
      </div>
    </div>
  );
}

/** User-friendly progress copy. The infra-flavoured strings live in
 *  fetchPhaseLabel; we route around them here so a normal user
 *  doesn't read "Verifying signature…" mid-update. */
function friendlyPhaseLabel(phase: FetchPhase): string {
  switch (phase) {
    case "resolving":
      return "Looking up Hytale data…";
    case "downloading":
      return "Downloading…";
    case "verifying":
      return "Checking integrity…";
    case "extracting":
      return "Setting up…";
    case "mounting":
      return "Finishing up…";
    default:
      // Defensive fallback if FetchPhase grows; never user-facing in
      // current code paths.
      return fetchPhaseLabel(phase);
  }
}

function shortDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toISOString().slice(0, 10);
}

/** "2026.03.26-89796e57b" → "2026.03.26"; full mode for fallback. */
function shortenVersion(v: string): string {
  const dash = v.indexOf("-");
  return dash > 0 ? v.slice(0, dash) : v;
}
