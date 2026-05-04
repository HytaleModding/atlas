import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  Check,
  CloudDownload,
  FolderOpen,
  Info,
  Loader2,
  MoreVertical,
  Play,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  formatBytes,
  formatShortDate,
  openInIde,
  phaseLabel,
  slotLabel,
  type DetectedIde,
  type PatcherStatus,
  type Slot,
  type SlotOverview,
} from "@/lib/patcher";
import { useBranchStore } from "@/state/branchStore";
import { useOverviewStore } from "@/state/overviewStore";
import { usePatcherStore } from "@/state/patcherStore";
import { useConfigStore } from "@/state/configStore";
import { useIndexStore } from "@/state/indexStore";
import { useFetchStore } from "@/state/fetchStore";
import { useNavStore } from "@/state/navStore";
import { FETCH_URL_BY_SLOT, fetchPhaseLabel } from "@/lib/fetcher";
import { validateHytalePath } from "@/lib/config";
import { InstallPrereleaseGuide } from "./InstallPrereleaseGuide";
import type { SlotIndexSummary } from "@/lib/indexer";

/**
 * The single slot card that reflects whichever branch is active in the
 * LeftNav toggle. State-driven: "not configured" / "no decompile" /
 * "decompile in flight" / "up-to-date" / "stale" each render a tailored
 * layout so the primary action is always obvious.
 */
export function BranchCard() {
  const slot = useBranchStore((s) => s.active);
  const fullOverview = useOverviewStore((s) => s.overview);
  const overview: SlotOverview | null = fullOverview
    ? slot === "release"
      ? fullOverview.release
      : fullOverview.pre_release
    : null;
  const ides = fullOverview?.ides ?? [];
  const refreshOverview = useOverviewStore((s) => s.refresh);

  const patcherStatus = usePatcherStore((s) => s.status);
  const patcherActiveSlot = usePatcherStore((s) => s.activeSlot);
  const patcherProgress = usePatcherStore((s) => s.progress);
  const patcherStarting = usePatcherStore((s) => s.starting);
  const startDecompile = usePatcherStore((s) => s.start);
  const clearSlot = usePatcherStore((s) => s.clear);
  const patcherError = usePatcherStore((s) => s.errorMessage);

  const indexOverview = useIndexStore((s) => s.overview);
  const indexActiveSlot = useIndexStore((s) => s.activeSlot);
  const indexStatus = useIndexStore((s) => s.status);
  const indexProgress = useIndexStore((s) => s.progress);
  const startIndex = useIndexStore((s) => s.start);
  const indexSummary: SlotIndexSummary | null =
    slot === "release"
      ? indexOverview?.release ?? null
      : indexOverview?.pre_release ?? null;
  const indexingThisSlot = indexActiveSlot === slot;
  const indexBusyElsewhere =
    indexActiveSlot !== null && indexActiveSlot !== slot;

  const fetchStatus = useFetchStore((s) => s.status);
  const fetchActiveBuildId = useFetchStore((s) => s.activeBuildId);
  const fetchProgress = useFetchStore((s) => s.progress);
  // In production builds the localhost dev URL is gated out; until the
  // central resolver lands, the build id is empty and the fetch button
  // simply has nothing to match against.
  const fetchExpectedBuildId = FETCH_URL_BY_SLOT[slot]?.buildId ?? "";
  const fetchingThisSlot =
    fetchActiveBuildId !== null && fetchActiveBuildId === fetchExpectedBuildId;

  const [guideOpen, setGuideOpen] = useState(false);
  const [configError, setConfigError] = useState<string | null>(null);

  const busyForThisSlot =
    patcherActiveSlot === slot &&
    (patcherStarting ||
      patcherStatus.kind === "phase" ||
      patcherStatus.kind === "downloading" ||
      patcherStatus.kind === "extracting");

  const busyForOtherSlot =
    patcherActiveSlot !== null &&
    patcherActiveSlot !== slot &&
    (patcherStatus.kind === "phase" ||
      patcherStatus.kind === "downloading" ||
      patcherStatus.kind === "extracting");

  async function pickInstall() {
    setConfigError(null);
    const snapshot = useConfigStore.getState().snapshot;
    const suggested =
      slot === "release"
        ? snapshot?.detected_release_path ??
          snapshot?.default_release_candidate ??
          undefined
        : snapshot?.detected_prerelease_path ??
          snapshot?.default_prerelease_candidate ??
          undefined;
    const picked = await open({
      directory: true,
      multiple: false,
      defaultPath: suggested,
      title: `Select Hytale ${slotLabel(slot).toLowerCase()} install folder`,
    });
    if (typeof picked !== "string") return;

    const check = await validateHytalePath(picked);
    if (!check.valid) {
      setConfigError(check.reason ?? "Invalid Hytale install path.");
      return;
    }
    const partial =
      slot === "release"
        ? { hytale_release_path: picked }
        : { hytale_prerelease_path: picked };
    await useConfigStore.getState().update(partial);
    await refreshOverview();
  }

  async function doDelete() {
    await clearSlot(slot);
    await refreshOverview();
  }

  return (
    <div className="flex w-full flex-col gap-2 rounded-md border border-border-subtle bg-bg-surface p-2.5">
      <Header
        slot={slot}
        overview={overview}
        busy={busyForThisSlot}
        busyElsewhere={busyForOtherSlot}
        onDecompile={() => void startDecompile(slot)}
        onDelete={() => void doDelete()}
        onPickInstall={() => void pickInstall()}
        hasDecompile={!!overview?.decompile}
        indexBusyElsewhere={indexBusyElsewhere}
        indexingThisSlot={indexingThisSlot}
        onReindex={() => void startIndex(slot)}
      />

      {/* Body: chooses one of the state-specific layouts. */}
      <Body
        slot={slot}
        overview={overview}
        ides={ides}
        busy={busyForThisSlot}
        busyElsewhere={busyForOtherSlot}
        status={patcherStatus}
        progress={patcherProgress}
        indexSummary={indexSummary}
        indexingThisSlot={indexingThisSlot}
        indexProgress={indexProgress}
        indexStatusKind={indexStatus.kind}
        fetchStatusKind={fetchStatus.kind}
        fetchingThisSlot={fetchingThisSlot}
        fetchProgress={fetchProgress}
        onPickInstall={() => void pickInstall()}
        onOpenGuide={() => setGuideOpen(true)}
        onDecompile={() => void startDecompile(slot)}
        onReindex={() => void startIndex(slot)}
      />

      {configError && (
        <ErrorBanner message={configError} onDismiss={() => setConfigError(null)} />
      )}
      {patcherError && (
        <ErrorBanner
          message={patcherError}
          onDismiss={() => usePatcherStore.getState().clearError()}
        />
      )}

      {guideOpen && (
        <InstallPrereleaseGuide onClose={() => setGuideOpen(false)} />
      )}
    </div>
  );
}

function Header({
  slot,
  overview,
  busy,
  busyElsewhere,
  onDecompile,
  onDelete,
  onPickInstall,
  hasDecompile,
  indexBusyElsewhere,
  indexingThisSlot,
  onReindex,
}: {
  slot: Slot;
  overview: SlotOverview | null;
  busy: boolean;
  busyElsewhere: boolean;
  onDecompile: () => void;
  onDelete: () => void;
  onPickInstall: () => void;
  hasDecompile: boolean;
  indexBusyElsewhere: boolean;
  indexingThisSlot: boolean;
  onReindex: () => void;
}) {
  const showMenu = !!overview?.configured;
  const versionText = overview?.hytale_version
    ? `v${overview.hytale_version}`
    : overview?.configured
      ? "Version unknown"
      : "Not configured";

  return (
    <header className="flex items-start gap-2">
      <StatusDot slot={slot} overview={overview} busy={busy} />
      <p
        className="min-w-0 flex-1 truncate font-mono text-[11px] text-fg-muted"
        title={versionText}
      >
        {versionText}
      </p>
      {showMenu && (
        <OverflowMenu
          busyElsewhere={busyElsewhere}
          onDecompile={onDecompile}
          onDelete={onDelete}
          onPickInstall={onPickInstall}
          hasDecompile={hasDecompile}
          indexBusyElsewhere={indexBusyElsewhere}
          indexingThisSlot={indexingThisSlot}
          onReindex={onReindex}
        />
      )}
    </header>
  );
}

function StatusDot({
  slot: _,
  overview,
  busy,
}: {
  slot: Slot;
  overview: SlotOverview | null;
  busy: boolean;
}) {
  let color = "bg-fg-muted"; // grey: not configured / data not prepared
  let title = "Data not prepared";

  if (busy) {
    color = "bg-accent-primary animate-pulse";
    title = "Working…";
  } else if (overview?.decompile) {
    if (overview.decompile.fresh) {
      color = "bg-success";
      title = "Up to date";
    } else {
      color = "bg-warning";
      title = "Hytale has updated since data was last prepared";
    }
  }

  return (
    <span
      aria-label={title}
      title={title}
      className={cn("mt-1.5 h-2 w-2 shrink-0 rounded-full", color)}
    />
  );
}

function Body({
  slot,
  overview,
  ides,
  busy,
  busyElsewhere,
  status,
  progress,
  indexSummary,
  indexingThisSlot,
  indexProgress,
  indexStatusKind,
  fetchStatusKind,
  fetchingThisSlot,
  fetchProgress,
  onPickInstall,
  onOpenGuide,
  onDecompile,
  onReindex,
}: {
  slot: Slot;
  overview: SlotOverview | null;
  ides: DetectedIde[];
  busy: boolean;
  busyElsewhere: boolean;
  status: PatcherStatus;
  progress: ReturnType<typeof usePatcherStore.getState>["progress"];
  indexSummary: SlotIndexSummary | null;
  indexingThisSlot: boolean;
  indexProgress: ReturnType<typeof useIndexStore.getState>["progress"];
  indexStatusKind: ReturnType<typeof useIndexStore.getState>["status"]["kind"];
  fetchStatusKind: ReturnType<typeof useFetchStore.getState>["status"]["kind"];
  fetchingThisSlot: boolean;
  fetchProgress: ReturnType<typeof useFetchStore.getState>["progress"];
  onPickInstall: () => void;
  onOpenGuide: () => void;
  onDecompile: () => void;
  onReindex: () => void;
}) {
  if (busy) {
    return (
      <ProgressBlock slot={slot} status={status} progress={progress} />
    );
  }

  if (!overview) {
    return (
      <p className="text-xs text-fg-muted">Loading overview…</p>
    );
  }

  if (!overview.configured) {
    return (
      <NotConfigured
        slot={slot}
        overview={overview}
        onPickInstall={onPickInstall}
        onOpenGuide={onOpenGuide}
      />
    );
  }

  if (!overview.jar_exists) {
    return (
      <p className="flex items-start gap-1.5 rounded-md border border-warning/40 bg-warning/10 px-2 py-1.5 text-[11px] text-warning">
        <AlertTriangle size={11} strokeWidth={2.25} className="mt-0.5 shrink-0" />
        <span>HytaleServer.jar missing.</span>
      </p>
    );
  }

  if (!overview.decompile) {
    return (
      <NoDecompile
        overview={overview}
        busyElsewhere={busyElsewhere}
        onDecompile={onDecompile}
      />
    );
  }

  return (
    <Configured
      overview={overview}
      ides={ides}
      indexSummary={indexSummary}
      indexingThisSlot={indexingThisSlot}
      indexProgress={indexProgress}
      indexStatusKind={indexStatusKind}
      fetchStatusKind={fetchStatusKind}
      fetchingThisSlot={fetchingThisSlot}
      fetchProgress={fetchProgress}
      onReindex={onReindex}
    />
  );
}

function NotConfigured({
  slot,
  overview: _,
  onPickInstall,
  onOpenGuide,
}: {
  slot: Slot;
  overview: SlotOverview;
  onPickInstall: () => void;
  onOpenGuide: () => void;
}) {
  if (slot === "pre-release") {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-[11px] text-fg-muted">
          Enable the Pre-release patchline in the Hytale launcher first.
        </p>
        <PrimaryButton icon={Info} onClick={onOpenGuide}>
          How to install
        </PrimaryButton>
        <SecondaryButton icon={FolderOpen} onClick={onPickInstall}>
          Pick folder
        </SecondaryButton>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="text-[11px] text-fg-muted">
        Pick your Hytale release install folder.
      </p>
      <PrimaryButton icon={FolderOpen} onClick={onPickInstall}>
        Choose install
      </PrimaryButton>
    </div>
  );
}

function NoDecompile({
  overview,
  busyElsewhere,
  onDecompile,
}: {
  overview: SlotOverview;
  busyElsewhere: boolean;
  onDecompile: () => void;
}) {
  return (
    <div className="flex flex-col gap-2">
      <p className="text-[11px] text-fg-muted">
        Ready to prepare data
        {overview.jar_size ? ` (${formatBytes(overview.jar_size)})` : ""}.
      </p>
      <PrimaryButton
        icon={Play}
        onClick={onDecompile}
        disabled={busyElsewhere}
        title={busyElsewhere ? "Already preparing data" : undefined}
      >
        Prepare data
      </PrimaryButton>
    </div>
  );
}

function Configured({
  overview,
  ides,
  indexSummary,
  indexingThisSlot,
  indexProgress,
  indexStatusKind,
  fetchStatusKind,
  fetchingThisSlot,
  fetchProgress,
  onReindex,
}: {
  overview: SlotOverview;
  ides: DetectedIde[];
  indexSummary: SlotIndexSummary | null;
  indexingThisSlot: boolean;
  indexProgress: ReturnType<typeof useIndexStore.getState>["progress"];
  indexStatusKind: ReturnType<typeof useIndexStore.getState>["status"]["kind"];
  fetchStatusKind: ReturnType<typeof useFetchStore.getState>["status"]["kind"];
  fetchingThisSlot: boolean;
  fetchProgress: ReturnType<typeof useFetchStore.getState>["progress"];
  onReindex: () => void;
}) {
  const decompile = overview.decompile!;
  const decompiledAt = formatShortDate(decompile.decompiled_at);

  return (
    <div className="flex flex-col gap-2">
      <p className="text-[11px] text-fg-muted">
        {decompile.fresh
          ? `Updated ${decompiledAt}`
          : `Updated ${decompiledAt} · Hytale changed`}
      </p>

      {!decompile.fresh && (
        <p className="flex items-start gap-1.5 rounded-md border border-warning/40 bg-warning/10 px-2 py-1.5 text-[11px] text-warning">
          <AlertTriangle size={11} strokeWidth={2.25} className="mt-0.5 shrink-0" />
          <span>Hytale updated. Refresh data to match.</span>
        </p>
      )}

      <IndexStatusLine
        summary={indexSummary}
        indexingThisSlot={indexingThisSlot}
        progress={indexProgress}
        statusKind={indexStatusKind}
        fetchStatusKind={fetchStatusKind}
        fetchingThisSlot={fetchingThisSlot}
        fetchProgress={fetchProgress}
        onReindex={onReindex}
      />

      {ides.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {ides.map((ide) => (
            <IconButton
              key={ide.id}
              title={`Open in ${ide.display_name}`}
              onClick={() => void openInIde(ide.id, decompile.output_dir)}
            >
              {ideGlyph(ide.id)}
            </IconButton>
          ))}
        </div>
      )}
    </div>
  );
}

function IndexStatusLine({
  summary,
  indexingThisSlot,
  progress,
  statusKind,
  fetchStatusKind,
  fetchingThisSlot,
  fetchProgress,
  onReindex: _onReindex,
}: {
  summary: SlotIndexSummary | null;
  indexingThisSlot: boolean;
  progress: ReturnType<typeof useIndexStore.getState>["progress"];
  statusKind: ReturnType<typeof useIndexStore.getState>["status"]["kind"];
  fetchStatusKind: ReturnType<typeof useFetchStore.getState>["status"]["kind"];
  fetchingThisSlot: boolean;
  fetchProgress: ReturnType<typeof useFetchStore.getState>["progress"];
  onReindex: () => void;
}) {
  // Live fetch progress for THIS slot takes precedence; that's the
  // central-pivot path the Body now defaults to. Render it before
  // the legacy local-index progress so a user who hits Fetch sees
  // the download bar even if a stale local-index status is around.
  if (
    fetchingThisSlot &&
    fetchStatusKind !== "idle" &&
    fetchStatusKind !== "done"
  ) {
    const total = fetchProgress.total ?? 0;
    const pct =
      total > 0
        ? Math.min(100, (fetchProgress.current / total) * 100)
        : 0;
    const label = fetchProgress.phase
      ? fetchPhaseLabel(fetchProgress.phase)
      : "Downloading…";
    return (
      <div className="flex flex-col gap-1">
        <div className="flex items-center justify-between text-[11px] text-fg-secondary">
          <span className="flex items-center gap-1.5">
            <Loader2 size={11} strokeWidth={2.25} className="animate-spin" />
            {label}
          </span>
          <span className="font-mono text-fg-muted">
            {total > 0
              ? `${formatBytes(fetchProgress.current)} / ${formatBytes(total)}`
              : formatBytes(fetchProgress.current)}
          </span>
        </div>
        <div className="h-1 w-full overflow-hidden rounded-full bg-bg-elevated">
          <div
            className="h-full bg-accent-primary transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  // Live local-indexer progress (kept for the user-mods path that
  // still calls index_start; the source slots no longer expose it
  // as a primary action).
  if (indexingThisSlot && statusKind !== "idle" && statusKind !== "done") {
    const pct =
      progress.total > 0
        ? Math.min(100, (progress.current / progress.total) * 100)
        : 0;
    return (
      <div className="flex flex-col gap-1">
        <div className="flex items-center justify-between text-[11px] text-fg-secondary">
          <span className="flex items-center gap-1.5">
            <Loader2 size={11} strokeWidth={2.25} className="animate-spin" />
            Preparing search data
          </span>
          <span className="font-mono text-fg-muted">
            {progress.total > 0
              ? `${progress.current.toLocaleString()} / ${progress.total.toLocaleString()}`
              : progress.phase ?? ""}
          </span>
        </div>
        <div className="h-1 w-full overflow-hidden rounded-full bg-bg-elevated">
          <div
            className="h-full bg-accent-primary transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  // Either no index yet or a newer one is available. The actual pull
  // action lives on the Index Catalog page now; this card just nudges
  // the user there.
  if (!summary || !summary.ready || summary.stale) {
    const updateAvailable = !!summary?.stale;
    return (
      <div className="flex flex-col gap-1.5">
        {updateAvailable ? (
          <p className="flex items-start gap-1.5 rounded-md border border-accent-primary/40 bg-accent-primary/10 px-2 py-1.5 text-[11px] text-accent-primary">
            <CloudDownload
              size={11}
              strokeWidth={2.25}
              className="mt-0.5 shrink-0"
            />
            <span>Hytale data update available.</span>
          </p>
        ) : (
          <p className="text-[11px] text-fg-muted">
            No Hytale data for this version yet.
          </p>
        )}
        <button
          type="button"
          onClick={() => useNavStore.getState().setPage("catalog")}
          className="flex w-full items-center justify-center gap-1.5 rounded-md border border-border-subtle px-2 py-1.5 text-[11px] text-fg-secondary transition-colors hover:bg-bg-elevated hover:text-fg-primary"
        >
          <CloudDownload size={11} strokeWidth={1.75} />
          Open Hytale Data
        </button>
      </div>
    );
  }

  const docs = summary.docs ?? 0;
  return (
    <p className="font-mono text-[11px] text-fg-muted">
      Ready · {docs.toLocaleString()} file{docs === 1 ? "" : "s"}
    </p>
  );
}

/** One-letter badge for the IDE buttons so they fit a narrow sidebar.
 *  Keeps Explorer's folder icon but otherwise shows a single glyph. */
function ideGlyph(id: DetectedIde["id"]): React.ReactNode {
  if (id === "explorer") return <FolderOpen size={12} strokeWidth={1.75} />;
  const letter =
    id === "vs-code" || id === "vs-code-insiders"
      ? "VS"
      : id === "intellij-ultimate"
        ? "IU"
        : "IC";
  return <span className="font-mono text-[10px] font-semibold">{letter}</span>;
}

function IconButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className="flex h-7 w-7 items-center justify-center rounded-md border border-border-subtle text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary"
    >
      {children}
    </button>
  );
}

function OverflowMenu({
  busyElsewhere,
  onDecompile,
  onDelete,
  onPickInstall,
  hasDecompile,
  indexBusyElsewhere,
  indexingThisSlot,
  onReindex,
}: {
  busyElsewhere: boolean;
  onDecompile: () => void;
  onDelete: () => void;
  onPickInstall: () => void;
  hasDecompile: boolean;
  indexBusyElsewhere: boolean;
  indexingThisSlot: boolean;
  onReindex: () => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-fg-secondary hover:bg-bg-elevated"
        title="More actions"
      >
        <MoreVertical size={14} strokeWidth={1.75} />
      </button>
      {open && (
        <div className="absolute right-0 top-full z-20 mt-1 w-48 overflow-hidden rounded-md border border-border-subtle bg-bg-surface shadow-lg">
          <MenuItem
            icon={RefreshCw}
            disabled={busyElsewhere}
            onClick={() => {
              setOpen(false);
              onDecompile();
            }}
          >
            Refresh data
          </MenuItem>
          {hasDecompile && (
            <MenuItem
              icon={RefreshCw}
              disabled={indexBusyElsewhere || indexingThisSlot}
              onClick={() => {
                setOpen(false);
                onReindex();
              }}
            >
              {indexingThisSlot ? "Preparing search…" : "Refresh search data"}
            </MenuItem>
          )}
          <MenuItem
            icon={Trash2}
            onClick={() => {
              setOpen(false);
              onDelete();
            }}
          >
            Delete prepared data
          </MenuItem>
          <MenuItem
            icon={FolderOpen}
            onClick={() => {
              setOpen(false);
              onPickInstall();
            }}
          >
            Change install path
          </MenuItem>
        </div>
      )}
    </div>
  );
}

function MenuItem({
  icon: Icon,
  children,
  onClick,
  disabled,
}: {
  icon: React.ComponentType<{ size?: number; strokeWidth?: number }>;
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex w-full items-center gap-2 px-3 py-2 text-left text-xs",
        disabled
          ? "cursor-not-allowed text-fg-muted"
          : "text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary",
      )}
    >
      <Icon size={12} strokeWidth={1.75} />
      {children}
    </button>
  );
}

function ProgressBlock({
  slot: _,
  status,
  progress,
}: {
  slot: Slot;
  status: PatcherStatus;
  progress: ReturnType<typeof usePatcherStore.getState>["progress"];
}) {
  const phase =
    status.kind === "phase" ? status.phase : progress.phase ?? "extracting";
  const label = phaseLabel(phase);

  if (status.kind === "extracting") {
    return (
      <ProgressBar
        label={label}
        current={status.current}
        total={status.total}
        format={(c, t) => `${c.toLocaleString()} / ${t.toLocaleString()} files`}
      />
    );
  }
  if (status.kind === "downloading") {
    const total = status.total ?? 0;
    return (
      <ProgressBar
        label={label}
        current={status.received}
        total={total}
        format={(c, t) => (t > 0 ? `${formatBytes(c)} / ${formatBytes(t)}` : formatBytes(c))}
      />
    );
  }
  return (
    <p className="flex items-center gap-1.5 text-xs text-fg-secondary">
      <Loader2 size={12} strokeWidth={2.25} className="animate-spin" />
      {label}
    </p>
  );
}

function ProgressBar({
  label,
  current,
  total,
  format,
}: {
  label: string;
  current: number;
  total: number;
  format: (current: number, total: number) => string;
}) {
  const pct = total > 0 ? Math.min(100, (current / total) * 100) : 0;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex justify-between text-xs text-fg-secondary">
        <span>{label}</span>
        <span className="font-mono text-fg-muted">{format(current, total)}</span>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-bg-elevated">
        <div
          className="h-full bg-accent-primary transition-[width] duration-150"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

function PrimaryButton({
  icon: Icon,
  onClick,
  children,
  disabled,
  title,
}: {
  icon: React.ComponentType<{ size?: number; strokeWidth?: number }>;
  onClick: () => void;
  children: React.ReactNode;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className={cn(
        "flex w-full items-center justify-center gap-1.5 rounded-md px-2 py-1.5 text-xs font-medium transition-colors",
        disabled
          ? "cursor-not-allowed bg-bg-elevated text-fg-muted"
          : "bg-accent-primary text-accent-primary-fg hover:brightness-110",
      )}
    >
      <Icon size={12} strokeWidth={1.75} />
      {children}
    </button>
  );
}

function SecondaryButton({
  icon: Icon,
  onClick,
  children,
}: {
  icon: React.ComponentType<{ size?: number; strokeWidth?: number }>;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full items-center justify-center gap-1.5 rounded-md border border-border-subtle px-2 py-1.5 text-[11px] text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary"
    >
      <Icon size={11} strokeWidth={1.75} />
      {children}
    </button>
  );
}

function ErrorBanner({
  message,
  onDismiss,
}: {
  message: string;
  onDismiss: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onDismiss}
      className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-left text-xs text-destructive hover:bg-destructive/15"
      title="Click to dismiss"
    >
      <AlertTriangle size={12} strokeWidth={2.25} className="mt-0.5 shrink-0" />
      <span className="font-mono">{message}</span>
      <Check size={12} strokeWidth={2.25} className="ml-auto mt-0.5 shrink-0 opacity-50" />
    </button>
  );
}
