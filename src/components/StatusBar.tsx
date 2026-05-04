import { useConfigStore } from "@/state/configStore";
import { useIndexStore } from "@/state/indexStore";
import { indexPhaseLabel } from "@/lib/indexer";
import { slotLabel } from "@/lib/patcher";
import { ActiveBuildChips } from "./ActiveBuildChips";

/** Status bar per ui-spec.md § Application Shell. Reads live config +
 *  indexer state so long-running work is visible even when the user has
 *  navigated away from the BranchCard. */
export function StatusBar() {
  const snapshot = useConfigStore((s) => s.snapshot);
  const hytalePath = snapshot?.config.hytale_release_path ?? null;
  const hytaleLabel = hytalePath
    ? shortenPath(hytalePath)
    : snapshot?.config.first_run_skipped
      ? "Hytale: not configured"
      : "Hytale: detecting…";

  const indexStatus = useIndexStore((s) => s.status);
  const indexActiveSlot = useIndexStore((s) => s.activeSlot);
  const indexProgress = useIndexStore((s) => s.progress);

  const leftLabel = (() => {
    if (indexStatus.kind === "phase" || indexStatus.kind === "progress") {
      const slot = indexActiveSlot
        ? slotLabel(indexActiveSlot).toLowerCase()
        : "";
      const phase = indexProgress.phase
        ? indexPhaseLabel(indexProgress.phase)
        : "Preparing search data…";
      if (indexProgress.total > 0) {
        const pct = Math.min(
          100,
          Math.round((indexProgress.current / indexProgress.total) * 100),
        );
        return `Preparing ${slot} · ${phase} ${pct}%`;
      }
      return `Preparing ${slot} · ${phase}`;
    }
    return "Idle";
  })();

  return (
    <div
      className="flex shrink-0 items-center justify-between border-t border-border-subtle bg-bg-surface px-3 text-xs text-fg-muted"
      style={{ height: "var(--status-bar-height)" }}
    >
      <span>{leftLabel}</span>
      <div className="flex items-center gap-3">
        <ActiveBuildChips />
        <span title={hytalePath ?? undefined}>{hytaleLabel}</span>
        {import.meta.env.DEV && <span>Atlas 0.1.0-dev</span>}
      </div>
    </div>
  );
}

/** Compact a long path for status bar display: keeps drive + last 2 segments. */
function shortenPath(path: string): string {
  const segs = path.split(/[\\/]/).filter(Boolean);
  if (segs.length <= 3) return path;
  return `${segs[0]}\\…\\${segs[segs.length - 2]}\\${segs[segs.length - 1]}`;
}
