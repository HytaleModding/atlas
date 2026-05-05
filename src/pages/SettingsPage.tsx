import { useEffect, useState } from "react";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  FolderOpen,
  Wrench,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { cn } from "@/lib/utils";
import { useConfigStore } from "@/state/configStore";
import { useFetchStore } from "@/state/fetchStore";
import { useUIPrefsStore, type EditorProtocol } from "@/state/uiPrefsStore";
import { validateHytalePath, type HytalePathCheck } from "@/lib/config";

/**
 * Settings page.
 *
 * The user-facing top half exposes the Hytale install path. The bottom
 * "Developer" section is collapsed by default and gates the in-app
 * testing escape hatches (loading a reference data file from disk so
 * we can iterate on indexing without touching the terminal).
 *
 * Vocabulary rule (memory/feedback_no_ui_jargon.md): nothing in the
 * top half mentions artifacts, mounting, manifests, etc. The Developer
 * section is allowed to use those words because anyone opening it has
 * already opted into developer territory.
 */
export function SettingsPage() {
  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 items-center justify-between border-b border-border-subtle px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold text-fg-primary">Settings</h1>
          <p className="text-xs text-fg-muted">
            Configure where Hytale lives on your machine.
          </p>
        </div>
      </header>

      <div className="flex-1 overflow-auto px-6 py-4">
        <HytalePathSection />
        <EditorSection />
        <DeveloperSection />
      </div>
    </div>
  );
}

function HytalePathSection() {
  const snapshot = useConfigStore((s) => s.snapshot);
  const update = useConfigStore((s) => s.update);

  const initialPath = snapshot?.config.hytale_release_path ?? "";
  const [path, setPath] = useState(initialPath);
  const [check, setCheck] = useState<HytalePathCheck | null>(null);
  const [validating, setValidating] = useState(false);
  const [saving, setSaving] = useState(false);
  const [savedJustNow, setSavedJustNow] = useState(false);

  // Re-sync local state when config loads after the page mounted.
  useEffect(() => {
    if (snapshot?.config.hytale_release_path && path === "") {
      setPath(snapshot.config.hytale_release_path);
    }
  }, [snapshot, path]);

  // Debounced validation.
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
    const picked = await openDialog({
      directory: true,
      multiple: false,
      defaultPath: path || undefined,
      title: "Select Hytale install folder",
    });
    if (typeof picked === "string") setPath(picked);
  }

  async function save() {
    if (!check?.valid) return;
    setSaving(true);
    try {
      await update({ hytale_release_path: path });
      setSavedJustNow(true);
      setTimeout(() => setSavedJustNow(false), 1800);
    } finally {
      setSaving(false);
    }
  }

  const dirty = path !== initialPath && check?.valid === true;

  return (
    <section className="mb-8">
      <h2 className="mb-3 text-sm font-medium text-fg-primary">
        Hytale install
      </h2>
      <div className="rounded-md border border-border-subtle bg-bg-surface p-4">
        <label className="flex flex-col gap-1.5">
          <span className="text-xs text-fg-muted">Install folder</span>
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

        <div className="mt-2 min-h-[18px] text-xs">
          {validating && (
            <span className="text-fg-muted">Checking path…</span>
          )}
          {!validating && check?.valid && (
            <span className="flex items-center gap-1.5 text-success">
              <Check size={12} strokeWidth={2.25} />
              Hytale install detected.
            </span>
          )}
          {!validating && check?.valid === false && (
            <span className="flex items-start gap-1.5 text-destructive">
              <AlertTriangle
                size={12}
                strokeWidth={2.25}
                className="mt-0.5 shrink-0"
              />
              <span>{check.reason ?? "Invalid path."}</span>
            </span>
          )}
        </div>

        <div className="mt-3 flex items-center justify-end gap-2">
          {savedJustNow && (
            <span className="text-xs text-success">Saved.</span>
          )}
          <button
            type="button"
            onClick={save}
            disabled={!dirty || saving}
            className={cn(
              "rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
              dirty && !saving
                ? "bg-accent-primary text-accent-primary-fg hover:brightness-110"
                : "cursor-not-allowed bg-bg-elevated text-fg-muted",
            )}
          >
            Save
          </button>
        </div>
      </div>
    </section>
  );
}

/**
 * Editor subsection. Picks the URL scheme used by the
 * "Open in editor" action in the source viewer header. `none` hides
 * the action entirely.
 */
function EditorSection() {
  const protocol = useUIPrefsStore((s) => s.editorProtocol);
  const setProtocol = useUIPrefsStore((s) => s.setEditorProtocol);

  const options: { value: EditorProtocol; label: string; hint: string }[] = [
    {
      value: "vscode",
      label: "Visual Studio Code",
      hint: "vscode://file/<path>:<line>",
    },
    {
      value: "idea",
      label: "JetBrains IDE (IntelliJ / Android Studio)",
      hint: "idea://open?file=<path>&line=<line>",
    },
    {
      value: "none",
      label: "Off (hide button)",
      hint: "Don't show the Open in editor action.",
    },
  ];

  return (
    <section className="mb-8">
      <h2 className="mb-3 text-sm font-medium text-fg-primary">Editor</h2>
      <div className="rounded-md border border-border-subtle bg-bg-surface p-4">
        <label className="flex flex-col gap-1.5">
          <span className="text-xs text-fg-muted">
            Open in editor uses this URL scheme.
          </span>
          <select
            value={protocol}
            onChange={(e) => setProtocol(e.target.value as EditorProtocol)}
            className="w-full rounded-md border border-border-subtle bg-bg-base px-3 py-2 font-mono text-sm text-fg-primary focus:border-accent-primary focus:outline-none"
          >
            {options.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
        </label>
        <p className="mt-2 font-mono text-[11px] text-fg-muted">
          {options.find((o) => o.value === protocol)?.hint}
        </p>
      </div>
    </section>
  );
}

/**
 * Collapsed-by-default Developer section. Once expanded, it exposes
 * the "Load reference data from file" escape hatch the rest of the UI
 * intentionally hides, for testing locally-built data files without
 * a real download server.
 */
function DeveloperSection() {
  const [open, setOpen] = useState(false);
  const startLocalMount = useFetchStore((s) => s.startLocalMount);
  const status = useFetchStore((s) => s.status);
  const showDebug = useUIPrefsStore((s) => s.showDebug);
  const setShowDebug = useUIPrefsStore((s) => s.setShowDebug);
  const snapshot = useConfigStore((s) => s.snapshot);
  const updateConfig = useConfigStore((s) => s.update);
  const [repoDraft, setRepoDraft] = useState(
    snapshot?.config.central_repo ?? "",
  );
  const [savingRepo, setSavingRepo] = useState(false);

  // Keep the draft in sync if config reloads while the section is open.
  useEffect(() => {
    if (snapshot?.config.central_repo !== undefined) {
      setRepoDraft(snapshot.config.central_repo);
    }
  }, [snapshot?.config.central_repo]);

  const busy =
    status.kind === "phase" ||
    status.kind === "downloading" ||
    status.kind === "extracting";

  async function pickFile() {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Atlas data file", extensions: ["zst"] }],
      title: "Pick an Atlas data file",
    });
    if (typeof picked === "string" && picked.length > 0) {
      void startLocalMount(picked);
    }
  }

  return (
    <section>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm font-medium text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary"
      >
        {open ? (
          <ChevronDown size={14} strokeWidth={2} />
        ) : (
          <ChevronRight size={14} strokeWidth={2} />
        )}
        <Wrench size={14} strokeWidth={1.75} />
        Developer
      </button>

      {open && (
        <div className="mt-2 rounded-md border border-border-subtle bg-bg-surface p-4">
          <p className="mb-3 text-xs text-fg-muted">
            These tools are for testing local builds of Atlas. Normal
            users should never need to open this section. See{" "}
            <code>docs/rebuild-runbook.md</code> for the full developer
            workflow.
          </p>

          <div className="flex items-center justify-between gap-3 rounded-md border border-border-subtle bg-bg-base p-3">
            <div className="min-w-0 flex-1">
              <div className="text-sm text-fg-primary">
                Load reference data from file
              </div>
              <div className="mt-0.5 text-xs text-fg-muted">
                Pick a signed data package produced by a local build.
                Atlas verifies the signature and adds it to the
                library.
              </div>
            </div>
            <button
              type="button"
              onClick={() => void pickFile()}
              disabled={busy}
              className="flex shrink-0 items-center gap-2 rounded-md border border-border-subtle bg-bg-elevated px-3 py-1.5 text-xs font-medium text-fg-primary hover:brightness-110 disabled:cursor-not-allowed disabled:text-fg-muted"
            >
              <FolderOpen size={14} strokeWidth={2.25} />
              Pick file…
            </button>
          </div>

          <div className="mt-3 rounded-md border border-border-subtle bg-bg-base p-3">
            <div className="mb-2">
              <div className="text-sm text-fg-primary">
                Reference data source
              </div>
              <div className="mt-0.5 text-xs text-fg-muted">
                GitHub repo (<code>owner/name</code>) Atlas pulls
                published reference data from.
              </div>
            </div>
            <div className="flex gap-2">
              <input
                type="text"
                value={repoDraft}
                onChange={(e) => setRepoDraft(e.target.value)}
                placeholder="HytaleModding/atlas"
                spellCheck={false}
                className="flex-1 rounded border border-border-subtle bg-bg-base px-2 py-1 font-mono text-xs text-fg-primary"
              />
              <button
                type="button"
                disabled={
                  savingRepo ||
                  repoDraft.trim() === (snapshot?.config.central_repo ?? "")
                }
                onClick={async () => {
                  setSavingRepo(true);
                  try {
                    await updateConfig({ central_repo: repoDraft.trim() });
                  } finally {
                    setSavingRepo(false);
                  }
                }}
                className="rounded border border-border-subtle px-3 py-1 text-xs text-fg-secondary hover:bg-bg-elevated disabled:opacity-50"
              >
                {savingRepo ? "Saving…" : "Save"}
              </button>
            </div>
          </div>

          <div className="mt-3 flex items-center justify-between gap-3 rounded-md border border-border-subtle bg-bg-base p-3">
            <div className="min-w-0 flex-1">
              <div className="text-sm text-fg-primary">
                Show ranking breakdown in search results
              </div>
              <div className="mt-0.5 text-xs text-fg-muted">
                Adds per-result search ranking debug under each hit so
                you can see why something ranked where it did.
              </div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={showDebug}
              onClick={() => setShowDebug(!showDebug)}
              className={cn(
                "relative h-6 w-11 shrink-0 rounded-full transition-colors",
                showDebug ? "bg-accent-primary" : "bg-bg-elevated",
              )}
            >
              <span
                aria-hidden
                className={cn(
                  "absolute top-0.5 h-5 w-5 rounded-full bg-fg-primary transition-transform",
                  showDebug ? "translate-x-5" : "translate-x-0.5",
                )}
              />
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
