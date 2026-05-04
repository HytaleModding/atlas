import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";
import { toast } from "sonner";

import type { MountedIndexEntry } from "@/lib/fetcher";
import type { Slot } from "@/lib/patcher";
import { useFetchStore } from "@/state/fetchStore";
import { useConfigStore } from "@/state/configStore";

/**
 * Two compact chips - one per patchline - showing which mounted Hytale
 * version search is currently using. Clicking a chip opens a small list
 * of other mounted versions for that patchline; picking one writes the
 * choice through `setActive` so search re-targets without a restart.
 *
 * Lives in the StatusBar so the answer to "which version am I searching"
 * is always one glance away.
 */
export function ActiveBuildChips() {
  const mounted = useFetchStore((s) => s.mounted);
  const setActive = useFetchStore((s) => s.setActive);
  const config = useConfigStore((s) => s.snapshot?.config ?? null);

  if (!config) return null;

  const releaseBuilds = mounted.filter(
    (m) => (m.manifest.hytale_patchline ?? "release") === "release",
  );
  const preReleaseBuilds = mounted.filter(
    (m) => m.manifest.hytale_patchline === "pre-release",
  );

  // Don't crowd the bar with empty placeholders - if no release builds are
  // mounted yet, the user is still on first-run and the chips would just
  // show "—" twice.
  if (releaseBuilds.length === 0 && preReleaseBuilds.length === 0) {
    return null;
  }

  return (
    <div className="flex items-center gap-2">
      {releaseBuilds.length > 0 && (
        <ActiveBuildChip
          label="Release"
          patchline="release"
          builds={releaseBuilds}
          activeId={config.active_release_build ?? null}
          onPick={async (id) => {
            await setActive("release", id);
            toast.success("Switched release version");
          }}
        />
      )}
      {preReleaseBuilds.length > 0 && (
        <ActiveBuildChip
          label="Pre-release"
          patchline="pre-release"
          builds={preReleaseBuilds}
          activeId={config.active_pre_release_build ?? null}
          onPick={async (id) => {
            await setActive("pre-release", id);
            toast.success("Switched pre-release version");
          }}
        />
      )}
    </div>
  );
}

function ActiveBuildChip({
  label,
  builds,
  activeId,
  onPick,
}: {
  label: string;
  patchline: Slot;
  builds: MountedIndexEntry[];
  activeId: string | null;
  onPick: (buildId: string) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close the popover when the user clicks anywhere else. Standard
  // mousedown-outside handler so we don't fight the chip's own click.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [open]);

  const active = activeId
    ? builds.find((b) => b.build_id === activeId) ?? null
    : null;
  const display = active
    ? active.manifest.hytale_impl_version
    : builds.length > 0
      ? "Pick…"
      : "—";

  // If only one build is mounted, skip the dropdown - clicking does nothing
  // because there's nowhere to switch to. Render as a plain span so it
  // visually de-emphasises.
  if (builds.length === 1) {
    return (
      <span
        className="flex items-center gap-1 rounded border border-border-subtle px-1.5 py-0.5 text-fg-secondary"
        title={`${label} version`}
      >
        <span className="text-fg-muted">{label}:</span>
        <span className="font-mono">{display}</span>
      </span>
    );
  }

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 rounded border border-border-subtle px-1.5 py-0.5 text-fg-secondary hover:bg-bg-elevated"
        title={`Switch ${label} version`}
      >
        <span className="text-fg-muted">{label}:</span>
        <span className="font-mono">{display}</span>
        <ChevronDown size={10} />
      </button>
      {open && (
        <div className="absolute bottom-full right-0 mb-1 min-w-[200px] rounded-md border border-border-subtle bg-bg-elevated p-1 shadow-lg">
          {builds.map((b) => {
            const isActive = b.build_id === activeId;
            return (
              <button
                key={b.build_id}
                type="button"
                onClick={async () => {
                  setOpen(false);
                  if (!isActive) {
                    try {
                      await onPick(b.build_id);
                    } catch (err) {
                      toast.error("Couldn't switch", {
                        description: String(err),
                      });
                    }
                  }
                }}
                className="flex w-full items-center justify-between gap-2 rounded px-2 py-1 text-left text-xs text-fg-primary hover:bg-bg-surface"
              >
                <span className="truncate font-mono">
                  {b.manifest.hytale_impl_version}
                </span>
                {isActive && (
                  <Check size={11} className="shrink-0 text-accent-primary" />
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
