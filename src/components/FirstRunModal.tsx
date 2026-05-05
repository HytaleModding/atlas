import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, Check, AlertTriangle } from "lucide-react";
import { cn } from "@/lib/utils";
import { validateHytalePath, type HytalePathCheck } from "@/lib/config";
import { useConfigStore } from "@/state/configStore";
import { useOverviewStore } from "@/state/overviewStore";

/** First-run modal per ui-spec.md § First-Run Modal.
 *  Blocks the app until the user either:
 *    - picks a valid Hytale install (Continue), or
 *    - explicitly skips (Skip for now); app opens in degraded state.
 */
export function FirstRunModal() {
  const snapshot = useConfigStore((s) => s.snapshot);
  const update = useConfigStore((s) => s.update);

  const initialPath =
    snapshot?.detected_release_path ??
    snapshot?.default_release_candidate ??
    "";

  const [path, setPath] = useState<string>(initialPath);
  const [check, setCheck] = useState<HytalePathCheck | null>(null);
  const [validating, setValidating] = useState(false);
  const [saving, setSaving] = useState(false);

  // Re-validate on every path change, debounced via effect.
  useEffect(() => {
    if (!path.trim()) {
      setCheck(null);
      return;
    }
    let cancelled = false;
    setValidating(true);
    const timer = setTimeout(async () => {
      try {
        const result = await validateHytalePath(path);
        if (!cancelled) setCheck(result);
      } finally {
        if (!cancelled) setValidating(false);
      }
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [path]);

  async function browse() {
    const picked = await open({
      directory: true,
      multiple: false,
      defaultPath: path || undefined,
      title: "Select Hytale install folder",
    });
    if (typeof picked === "string") setPath(picked);
  }

  async function onContinue() {
    if (!check?.valid) return;
    setSaving(true);
    try {
      await update({ hytale_release_path: path });
      // Saving the path flips `overview.configured` on the backend, but the
      // overview store caches the previous snapshot. Without an explicit
      // refresh, BranchCard keeps rendering "Choose install" until the next
      // window-focus poll. Match the pattern in BranchCard.pickInstall.
      await useOverviewStore.getState().refresh();
    } finally {
      setSaving(false);
    }
  }

  async function onSkip() {
    setSaving(true);
    try {
      await update({ first_run_skipped: true });
    } finally {
      setSaving(false);
    }
  }

  const canContinue = !!check?.valid && !saving;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div
        className="flex w-[480px] flex-col gap-4 rounded-lg border border-border-subtle bg-bg-surface p-6 shadow-2xl"
        role="dialog"
        aria-labelledby="firstrun-title"
      >
        <header className="flex flex-col gap-1">
          <h2
            id="firstrun-title"
            className="font-display text-xl text-fg-primary"
          >
            Welcome to Atlas
          </h2>
          <p className="text-sm text-fg-secondary">
            Atlas needs to know where Hytale is installed on your machine
            so it can search across the source code, modding guides, and
            API docs for you.
          </p>
        </header>

        <label className="flex flex-col gap-1">
          <span className="text-xs text-fg-muted">Hytale install path</span>
          <div className="flex gap-2">
            <input
              type="text"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="C:\Users\you\AppData\Roaming\Hytale\install\release\package\game\latest"
              className={cn(
                "flex-1 rounded-md border bg-bg-base px-3 py-2 font-mono text-sm text-fg-primary",
                "placeholder:text-fg-muted focus:outline-none",
                check?.valid === false
                  ? "border-destructive"
                  : check?.valid
                    ? "border-success"
                    : "border-border-subtle focus:border-accent-primary",
              )}
            />
            <button
              type="button"
              onClick={browse}
              className="flex items-center gap-1.5 rounded-md border border-border-subtle bg-bg-elevated px-3 py-2 text-sm text-fg-secondary hover:text-fg-primary"
            >
              <FolderOpen size={14} strokeWidth={1.75} />
              Browse
            </button>
          </div>
        </label>

        <ValidationLine check={check} validating={validating} />

        <footer className="flex items-center justify-end gap-2 pt-2">
          <button
            type="button"
            onClick={onSkip}
            disabled={saving}
            className="rounded-md px-3 py-2 text-sm text-fg-secondary hover:text-fg-primary disabled:opacity-50"
          >
            Skip for now
          </button>
          <button
            type="button"
            onClick={onContinue}
            disabled={!canContinue}
            className={cn(
              "rounded-md px-4 py-2 text-sm font-medium transition-colors",
              canContinue
                ? "bg-accent-primary text-accent-primary-fg hover:brightness-110"
                : "cursor-not-allowed bg-bg-elevated text-fg-muted",
            )}
          >
            Continue
          </button>
        </footer>
      </div>
    </div>
  );
}

function ValidationLine({
  check,
  validating,
}: {
  check: HytalePathCheck | null;
  validating: boolean;
}) {
  if (validating) {
    return (
      <span className="text-xs text-fg-muted">Checking path…</span>
    );
  }
  if (!check) {
    return (
      <span className="text-xs text-fg-muted">
        Pick the folder where Hytale is installed.
      </span>
    );
  }
  if (check.valid) {
    return (
      <span className="flex items-center gap-1.5 text-xs text-success">
        <Check size={12} strokeWidth={2.25} />
        Hytale install detected.
      </span>
    );
  }
  return (
    <span className="flex items-start gap-1.5 text-xs text-destructive">
      <AlertTriangle size={12} strokeWidth={2.25} className="mt-0.5 shrink-0" />
      <span>{check.reason ?? "Invalid path."}</span>
    </span>
  );
}
