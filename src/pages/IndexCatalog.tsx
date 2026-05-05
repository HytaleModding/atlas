import { useEffect, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  CloudDownload,
  Database,
  FolderOpen,
  FolderPlus,
  Hammer,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { openPath } from "@tauri-apps/plugin-opener";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { cn } from "@/lib/utils";
import {
  fetchPhaseLabel,
  type FetchPhase,
  type MountedIndexEntry,
  type RemoteBuildResolution,
} from "@/lib/fetcher";
import { formatBytes } from "@/lib/format";
import { indexCompare, type CompareReport } from "@/lib/compare";
import type { Slot } from "@/lib/patcher";
import { useFetchStore } from "@/state/fetchStore";
import { useConfigStore } from "@/state/configStore";
import { useProjectStore } from "@/state/projectStore";
import { useDiffStore } from "@/state/diffStore";
import { useNavStore } from "@/state/navStore";
import type { ProjectListEntry } from "@/lib/project";

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
  const remoteResolutions = useFetchStore((s) => s.remoteResolutions);
  const checkingUpdates = useFetchStore((s) => s.checkingUpdates);
  const checkForUpdates = useFetchStore((s) => s.checkForUpdates);
  const startFetch = useFetchStore((s) => s.start);

  const config = useConfigStore((s) => s.snapshot?.config ?? null);

  useEffect(() => {
    void refreshCatalog();
  }, [refreshCatalog]);

  const fetchBusy =
    status.kind === "phase" ||
    status.kind === "downloading" ||
    status.kind === "extracting";

  const activeIds: Record<Slot, string | null> = {
    release: config?.active_release_build ?? null,
    "pre-release": config?.active_pre_release_build ?? null,
  };

  // Decide which resolver results are worth surfacing as "newer build
  // available" cards: only if the remote build_id differs from anything
  // currently mounted for that patchline.
  const updateCards = (["release", "pre-release"] as Slot[])
    .map((slot) => {
      const resolution = remoteResolutions[slot];
      if (!resolution) return null;
      const alreadyHave = mounted.some(
        (m) => m.build_id === resolution.build_id,
      );
      if (alreadyHave) return null;
      return { slot, resolution };
    })
    .filter(
      (
        x,
      ): x is { slot: Slot; resolution: RemoteBuildResolution } => x !== null,
    );

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
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void checkForUpdates()}
            disabled={checkingUpdates || fetchBusy}
            className="flex items-center gap-2 rounded border border-border-subtle px-3 py-1.5 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
          >
            <CloudDownload
              size={14}
              className={cn(checkingUpdates && "animate-pulse")}
            />
            Check for updates
          </button>
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
        </div>
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

        {updateCards.map(({ slot, resolution }) => (
          <UpdateCard
            key={slot}
            slot={slot}
            resolution={resolution}
            disabled={fetchBusy}
            onUpdate={async () => {
              try {
                await startFetch({
                  buildId: resolution.build_id,
                  url: resolution.url,
                });
              } catch (err) {
                toast.error("Update failed", { description: String(err) });
              }
            }}
          />
        ))}

        {mounted.length === 0 ? (
          <EmptyState />
        ) : (
          <ul className="flex flex-col gap-2">
            {mounted.map((entry) => {
              const slot: Slot =
                entry.manifest.hytale_patchline === "pre-release"
                  ? "pre-release"
                  : "release";
              const sameSlotCount = mounted.filter(
                (m) =>
                  (m.manifest.hytale_patchline === "pre-release"
                    ? "pre-release"
                    : "release") === slot,
              ).length;
              return (
                <DataRow
                  key={entry.build_id}
                  entry={entry}
                  slot={slot}
                  isActive={activeIds[slot] === entry.build_id}
                  canDelete={sameSlotCount > 1}
                />
              );
            })}
          </ul>
        )}

        <ProjectsSection />

        <CompareSection />
      </div>
    </div>
  );
}

/**
 * Your Projects: registered mod-source folders. Each row shows the
 * project's friendly name, source path, and either an "Index now" CTA
 * or an "In search" badge once the index has been built. Indexing
 * progress is rendered inline on the row using the project store's
 * per-id progress map.
 */
function ProjectsSection() {
  const projects = useProjectStore((s) => s.projects);
  const progress = useProjectStore((s) => s.progress);
  const errors = useProjectStore((s) => s.errors);
  const refresh = useProjectStore((s) => s.refresh);
  const register = useProjectStore((s) => s.register);

  const [adding, setAdding] = useState(false);

  async function pickFolder() {
    setAdding(true);
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: "Pick your project folder",
      });
      if (typeof picked !== "string") return;
      await register(picked);
      toast.success("Project added", {
        description: "Click 'Index now' to make it searchable.",
      });
    } catch (err) {
      toast.error("Couldn't add project", { description: String(err) });
    } finally {
      setAdding(false);
    }
  }

  return (
    <section className="mt-8">
      <div className="mb-3 flex items-center justify-between border-b border-border-subtle pb-2">
        <div>
          <h2 className="text-sm font-semibold text-fg-primary">
            Your Projects
          </h2>
          <p className="text-xs text-fg-muted">
            Point Atlas at your own mod source so its code shows up in
            search alongside Hytale&apos;s.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => void refresh()}
            className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated"
          >
            <RefreshCw size={12} />
            Refresh
          </button>
          <button
            type="button"
            onClick={() => void pickFolder()}
            disabled={adding}
            className="flex items-center gap-1.5 rounded bg-accent-primary px-2.5 py-1 text-xs font-medium text-bg-base hover:opacity-90 disabled:opacity-50"
          >
            <FolderPlus size={12} />
            Add project
          </button>
        </div>
      </div>

      {projects.length === 0 ? (
        <ProjectsEmptyState />
      ) : (
        <ul className="flex flex-col gap-2">
          {projects.map((p) => (
            <ProjectRow
              key={p.id}
              project={p}
              progress={progress[p.id]}
              error={errors[p.id]}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

function ProjectsEmptyState() {
  return (
    <div className="flex flex-col items-center justify-center gap-2 rounded-md border border-dashed border-border-subtle bg-bg-surface p-6 text-center">
      <Hammer size={22} strokeWidth={1.5} className="text-fg-muted" />
      <p className="text-xs text-fg-muted">
        No projects yet. Add a folder to include your own code in search
        results.
      </p>
    </div>
  );
}

/** One row in the projects list. Mirrors `DataRow`'s look so the page
 *  reads as one consistent surface. */
function ProjectRow({
  project,
  progress,
  error,
}: {
  project: ProjectListEntry;
  progress: ReturnType<typeof useProjectStore.getState>["progress"][string] | undefined;
  error: string | undefined;
}) {
  const startIndex = useProjectStore((s) => s.startIndex);
  const removeIndex = useProjectStore((s) => s.removeIndex);
  const unregister = useProjectStore((s) => s.unregister);
  const clearError = useProjectStore((s) => s.clearError);
  const setDiffTarget = useDiffStore((s) => s.setTarget);
  const setPage = useNavStore((s) => s.setPage);
  const mounted = useFetchStore((s) => s.mounted);
  const activeRelease = useConfigStore(
    (s) => s.snapshot?.config.active_release_build ?? null,
  );
  const activePreRelease = useConfigStore(
    (s) => s.snapshot?.config.active_pre_release_build ?? null,
  );
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const [working, setWorking] = useState(false);

  const indexing = progress !== undefined;
  // We can pre-fill the diff picker with sensible defaults if we have an
  // active release and at least one pre-release version. Otherwise the
  // diff page falls back to an empty picker so the user picks manually.
  const hasPreRelease = mounted.some(
    (m) => m.manifest.hytale_patchline === "pre-release",
  );
  const diffEnabled = project.index_ready && !indexing;

  return (
    <li className="rounded-md border border-border-subtle bg-bg-surface p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="truncate text-sm font-medium text-fg-primary">
              {project.name}
            </span>
            {project.index_ready && !indexing && (
              <span className="rounded bg-accent-primary/15 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-accent-primary">
                In search
              </span>
            )}
          </div>
          <div className="mt-1 truncate font-mono text-[11px] text-fg-muted">
            {project.source_path}
          </div>
          {project.last_indexed_at && (
            <div className="mt-0.5 text-xs text-fg-muted">
              Last indexed {shortDate(project.last_indexed_at)}
            </div>
          )}
        </div>
      </div>

      {indexing && (
        <div className="mt-3">
          <ProjectProgressBanner
            phase={progress?.phase ?? null}
            current={progress?.current ?? 0}
            total={progress?.total ?? null}
          />
        </div>
      )}

      {error && !indexing && (
        <div className="mt-3 flex items-start gap-2 rounded border border-status-error/40 bg-status-error/10 p-2 text-xs text-status-error">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <span className="flex-1">{error}</span>
          <button
            type="button"
            onClick={() => clearError(project.id)}
            className="text-fg-muted hover:text-fg-primary"
          >
            Dismiss
          </button>
        </div>
      )}

      <div className="mt-3 flex items-center justify-end gap-2">
        {diffEnabled && (
          <button
            type="button"
            onClick={() => {
              const baseline = activeRelease ?? null;
              const preRelease = hasPreRelease
                ? activePreRelease ?? null
                : null;
              setDiffTarget({
                projectId: project.id,
                baselineBuildId: baseline,
                targetBuildId: preRelease,
              });
              setPage("diff");
            }}
            className="rounded border border-accent-primary/40 bg-accent-primary/10 px-2.5 py-1 text-xs text-accent-primary hover:bg-accent-primary/20"
          >
            Check what would break
          </button>
        )}
        <button
          type="button"
          disabled={indexing || working}
          onClick={async () => {
            setWorking(true);
            try {
              await startIndex(project.id);
            } catch (err) {
              toast.error("Couldn't start indexing", {
                description: String(err),
              });
            } finally {
              setWorking(false);
            }
          }}
          className="rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
        >
          {project.index_ready ? "Re-index" : "Index now"}
        </button>
        <button
          type="button"
          onClick={async () => {
            try {
              await openPath(project.source_path);
            } catch (err) {
              toast.error("Couldn't open folder", {
                description: String(err),
              });
            }
          }}
          className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated"
        >
          <FolderOpen size={12} />
          Show files
        </button>
        {project.index_ready && !indexing && (
          <button
            type="button"
            disabled={working}
            onClick={async () => {
              setWorking(true);
              try {
                await removeIndex(project.id);
                toast.success("Project search data cleared");
              } catch (err) {
                toast.error("Couldn't clear", { description: String(err) });
              } finally {
                setWorking(false);
              }
            }}
            className="rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
          >
            Clear search data
          </button>
        )}
        {confirmingRemove ? (
          <div className="flex items-center gap-1.5">
            <span className="text-xs text-fg-muted">Remove project?</span>
            <button
              type="button"
              disabled={working}
              onClick={async () => {
                setWorking(true);
                try {
                  await unregister(project.id);
                  toast.success("Project removed");
                } catch (err) {
                  toast.error("Couldn't remove", {
                    description: String(err),
                  });
                } finally {
                  setWorking(false);
                  setConfirmingRemove(false);
                }
              }}
              className="rounded bg-status-error/20 px-2.5 py-1 text-xs text-status-error hover:bg-status-error/30 disabled:opacity-50"
            >
              Remove
            </button>
            <button
              type="button"
              onClick={() => setConfirmingRemove(false)}
              className="rounded px-2.5 py-1 text-xs text-fg-muted hover:bg-bg-elevated"
            >
              Cancel
            </button>
          </div>
        ) : (
          <button
            type="button"
            disabled={indexing}
            onClick={() => setConfirmingRemove(true)}
            className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
          >
            <Trash2 size={12} />
            Remove
          </button>
        )}
      </div>
    </li>
  );
}

function ProjectProgressBanner({
  phase,
  current,
  total,
}: {
  phase: string | null;
  current: number;
  total: number | null;
}) {
  const label = phase ? friendlyProjectPhase(phase) : "Getting ready…";
  const pct =
    total && total > 0
      ? Math.min(100, Math.round((current / total) * 100))
      : null;

  return (
    <div className="rounded border border-border-subtle bg-bg-elevated p-2 text-xs">
      <div className="mb-1.5 flex items-center justify-between">
        <span className="text-fg-primary">{label}</span>
        {total !== null && total > 0 && (
          <span className="text-fg-muted">
            {current.toLocaleString()} / {total.toLocaleString()}
          </span>
        )}
      </div>
      <div className="h-1 w-full overflow-hidden rounded bg-bg-base">
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

/** Map indexer phase strings to user-friendly copy. The backend phase
 *  vocabulary mirrors `IndexerPhase::as_str()` in indexer/status.rs:
 *  `walking` | `indexing` | `committing`. */
function friendlyProjectPhase(phase: string): string {
  switch (phase) {
    case "walking":
      return "Scanning your code…";
    case "indexing":
      return "Reading your code…";
    case "committing":
      return "Saving search data…";
    default:
      return "Working…";
  }
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
 * One row in the data list. Shows only what a user cares about: which
 * Hytale version, which channel (Release vs Pre-release), when Atlas
 * got it, and how big it is. Row actions:
 *  - "Use for search" radio - flips which build the search engine targets
 *    for this channel. Hidden when only one build for that channel exists.
 *  - "Show files" - opens the on-disk folder, helpful for support.
 *  - "Remove" - delete the data; backend refuses if it would leave the
 *    channel empty.
 */
function DataRow({
  entry,
  slot,
  isActive,
  canDelete,
}: {
  entry: MountedIndexEntry;
  slot: Slot;
  isActive: boolean;
  canDelete: boolean;
}) {
  const { manifest } = entry;
  const channel = slot === "pre-release" ? "Pre-release" : "Release";
  const version = manifest.hytale_impl_version || "Unknown version";

  const setActive = useFetchStore((s) => s.setActive);
  const remove = useFetchStore((s) => s.remove);
  const [confirming, setConfirming] = useState(false);
  const [working, setWorking] = useState(false);

  return (
    <li
      className={cn(
        "rounded-md border bg-bg-surface p-4",
        isActive ? "border-accent-primary/50" : "border-border-subtle",
      )}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="text-sm font-medium text-fg-primary">
              Hytale {shortenVersion(version)}
            </span>
            <span className="rounded bg-bg-elevated px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-fg-secondary">
              {channel}
            </span>
            {isActive && (
              <span className="rounded bg-accent-primary/15 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-accent-primary">
                In use
              </span>
            )}
          </div>
          <div className="mt-1 text-xs text-fg-muted">
            Added {shortDate(manifest.created_at)}
          </div>
        </div>
        <div className="shrink-0 text-right text-xs text-fg-muted">
          {formatBytes(entry.size_bytes)}
        </div>
      </div>

      <div className="mt-3 flex items-center justify-end gap-2">
        {!isActive && canDelete && (
          <button
            type="button"
            disabled={working}
            onClick={async () => {
              setWorking(true);
              try {
                await setActive(slot, entry.build_id);
                toast.success(`Now using this for ${channel} search`);
              } catch (err) {
                toast.error("Couldn't switch", { description: String(err) });
              } finally {
                setWorking(false);
              }
            }}
            className="rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
          >
            Use for search
          </button>
        )}
        <button
          type="button"
          onClick={async () => {
            try {
              await openPath(entry.path);
            } catch (err) {
              toast.error("Couldn't open folder", {
                description: String(err),
              });
            }
          }}
          className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated"
        >
          <FolderOpen size={12} />
          Show files
        </button>
        {canDelete &&
          (confirming ? (
            <div className="flex items-center gap-1.5">
              <span className="text-xs text-fg-muted">Remove this data?</span>
              <button
                type="button"
                disabled={working}
                onClick={async () => {
                  setWorking(true);
                  try {
                    await remove(entry.build_id);
                    toast.success("Removed");
                  } catch (err) {
                    toast.error("Couldn't remove", {
                      description: String(err),
                    });
                  } finally {
                    setWorking(false);
                    setConfirming(false);
                  }
                }}
                className="rounded bg-status-error/20 px-2.5 py-1 text-xs text-status-error hover:bg-status-error/30 disabled:opacity-50"
              >
                Remove
              </button>
              <button
                type="button"
                onClick={() => setConfirming(false)}
                className="rounded px-2.5 py-1 text-xs text-fg-muted hover:bg-bg-elevated"
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setConfirming(true)}
              className="flex items-center gap-1.5 rounded border border-border-subtle px-2.5 py-1 text-xs text-fg-secondary hover:bg-bg-elevated"
            >
              <Trash2 size={12} />
              Remove
            </button>
          ))}
      </div>
    </li>
  );
}

/** Card shown above the data list when the central repo has a build the
 *  user doesn't have yet. Click → kicks off `index_fetch`; the existing
 *  progress banner takes over from there. */
function UpdateCard({
  slot,
  resolution,
  disabled,
  onUpdate,
}: {
  slot: Slot;
  resolution: RemoteBuildResolution;
  disabled: boolean;
  onUpdate: () => Promise<void>;
}) {
  const [working, setWorking] = useState(false);
  const channel = slot === "pre-release" ? "Pre-release" : "Release";
  const versionLabel =
    resolution.hytale_impl_version &&
    resolution.hytale_impl_version.length > 0
      ? `Hytale ${shortenVersion(resolution.hytale_impl_version)}`
      : `Build ${resolution.release_tag}`;

  return (
    <div className="mb-3 flex items-center justify-between gap-3 rounded-md border border-accent-primary/40 bg-accent-primary/5 p-3 text-xs">
      <div>
        <div className="font-medium text-fg-primary">
          New {channel} data available
        </div>
        <div className="mt-0.5 text-fg-muted">{versionLabel}</div>
      </div>
      <button
        type="button"
        disabled={disabled || working}
        onClick={async () => {
          setWorking(true);
          try {
            await onUpdate();
          } finally {
            setWorking(false);
          }
        }}
        className="rounded bg-accent-primary px-3 py-1.5 text-xs font-medium text-bg-base hover:opacity-90 disabled:opacity-50"
      >
        {working ? "Starting…" : "Update"}
      </button>
    </div>
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

/**
 * Compare two installed Hytale versions corpus-wide. Renders as a
 * collapsed disclosure - it's a "see how much changed" diagnostic for
 * curious users, not part of the primary flow. Once expanded, picking
 * two versions and clicking Compare runs `index_compare` and shows a
 * count summary + the first chunk of class FQNs that were added or
 * removed.
 */
function CompareSection() {
  const mounted = useFetchStore((s) => s.mounted);
  const [open, setOpen] = useState(false);

  if (mounted.length < 2) {
    // Compare needs two builds - hide the entire section until the user
    // has installed at least two so we don't dangle a useless disclosure.
    return null;
  }

  return (
    <section className="mt-8">
      <details
        open={open}
        onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
        className="rounded-md border border-border-subtle bg-bg-surface"
      >
        <summary className="cursor-pointer list-none p-3 text-sm font-semibold text-fg-primary marker:hidden">
          <div className="flex items-center justify-between">
            <span>Compare two Hytale versions</span>
            <span className="text-xs text-fg-muted">
              {open ? "Hide" : "Show"}
            </span>
          </div>
          <p className="mt-0.5 text-xs font-normal text-fg-muted">
            See what classes were added or removed between any two installed
            versions.
          </p>
        </summary>
        {open && <CompareBody mounted={mounted} />}
      </details>
    </section>
  );
}

function CompareBody({ mounted }: { mounted: MountedIndexEntry[] }) {
  const [baselineId, setBaselineId] = useState<string | null>(null);
  const [targetId, setTargetId] = useState<string | null>(null);
  const [report, setReport] = useState<CompareReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canRun =
    !!baselineId && !!targetId && baselineId !== targetId && !loading;

  async function run() {
    if (!baselineId || !targetId) return;
    setLoading(true);
    setError(null);
    try {
      const r = await indexCompare({
        baselineBuildId: baselineId,
        targetBuildId: targetId,
      });
      setReport(r);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="border-t border-border-subtle p-4">
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <CompareSelect
          label="Baseline"
          value={baselineId}
          options={mounted}
          onChange={setBaselineId}
          disabled={loading}
        />
        <CompareSelect
          label="Target"
          value={targetId}
          options={mounted}
          onChange={setTargetId}
          disabled={loading}
        />
      </div>
      <div className="mt-3 flex justify-end">
        <button
          type="button"
          disabled={!canRun}
          onClick={() => void run()}
          className="rounded bg-accent-primary px-3 py-1.5 text-xs font-medium text-bg-base hover:opacity-90 disabled:opacity-50"
        >
          {loading ? "Comparing…" : "Compare"}
        </button>
      </div>
      {error && (
        <div className="mt-3 rounded border border-status-error/40 bg-status-error/10 p-2 text-xs text-status-error">
          {error}
        </div>
      )}
      {report && <CompareResults report={report} />}
    </div>
  );
}

function CompareSelect({
  label,
  value,
  options,
  onChange,
  disabled,
}: {
  label: string;
  value: string | null;
  options: MountedIndexEntry[];
  onChange: (id: string | null) => void;
  disabled: boolean;
}) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className="text-[11px] font-medium uppercase tracking-wide text-fg-muted">
        {label}
      </span>
      <select
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value === "" ? null : e.target.value)}
        disabled={disabled}
        className="rounded border border-border-subtle bg-bg-base px-2 py-1.5 text-xs text-fg-primary"
      >
        <option value="">Pick a version…</option>
        {options.map((b) => {
          const patchline = b.manifest.hytale_patchline ?? "release";
          return (
            <option key={b.build_id} value={b.build_id}>
              {patchline} · {b.manifest.hytale_impl_version}
            </option>
          );
        })}
      </select>
    </label>
  );
}

function CompareResults({ report }: { report: CompareReport }) {
  const baseClasses = report.baseline_counts.classes;
  const targetClasses = report.target_counts.classes;
  const classDelta = Number(targetClasses) - Number(baseClasses);

  return (
    <div className="mt-4 flex flex-col gap-3">
      <div className="flex flex-wrap gap-3 rounded border border-border-subtle bg-bg-base p-3 text-xs">
        <Stat
          label="Classes"
          baseline={Number(baseClasses)}
          target={Number(targetClasses)}
          delta={classDelta}
        />
        <Stat
          label="Methods"
          baseline={Number(report.baseline_counts.methods)}
          target={Number(report.target_counts.methods)}
          delta={
            Number(report.target_counts.methods) -
            Number(report.baseline_counts.methods)
          }
        />
        <Stat
          label="Fields"
          baseline={Number(report.baseline_counts.fields)}
          target={Number(report.target_counts.fields)}
          delta={
            Number(report.target_counts.fields) -
            Number(report.baseline_counts.fields)
          }
        />
        <span className="ml-auto text-fg-muted">
          {report.classes_shared.toLocaleString()} classes in both
        </span>
      </div>

      <CompareList
        title={`Added in target (${report.classes_added_total})`}
        items={report.classes_added}
        truncated={report.classes_added_total > report.classes_added.length}
        emptyHint="No classes added."
      />
      <CompareList
        title={`Removed from baseline (${report.classes_removed_total})`}
        items={report.classes_removed}
        truncated={
          report.classes_removed_total > report.classes_removed.length
        }
        emptyHint="No classes removed."
      />
    </div>
  );
}

function Stat({
  label,
  baseline,
  target,
  delta,
}: {
  label: string;
  baseline: number;
  target: number;
  delta: number;
}) {
  const sign = delta > 0 ? "+" : "";
  const tone =
    delta === 0
      ? "text-fg-muted"
      : delta > 0
        ? "text-status-success"
        : "text-status-error";
  return (
    <span className="flex items-baseline gap-1.5">
      <span className="text-fg-muted">{label}:</span>
      <span className="font-mono text-fg-primary">
        {baseline.toLocaleString()} → {target.toLocaleString()}
      </span>
      <span className={cn("font-mono", tone)}>
        ({sign}
        {delta.toLocaleString()})
      </span>
    </span>
  );
}

function CompareList({
  title,
  items,
  truncated,
  emptyHint,
}: {
  title: string;
  items: string[];
  truncated: boolean;
  emptyHint: string;
}) {
  return (
    <div>
      <h4 className="mb-1 text-xs font-semibold text-fg-secondary">{title}</h4>
      {items.length === 0 ? (
        <p className="text-xs text-fg-muted">{emptyHint}</p>
      ) : (
        <>
          <ul className="max-h-48 overflow-y-auto rounded border border-border-subtle bg-bg-base p-2 font-mono text-[11px] text-fg-secondary">
            {items.map((c) => (
              <li key={c} className="truncate">
                {c}
              </li>
            ))}
          </ul>
          {truncated && (
            <p className="mt-1 text-[11px] text-fg-muted">
              List truncated; full count above.
            </p>
          )}
        </>
      )}
    </div>
  );
}
