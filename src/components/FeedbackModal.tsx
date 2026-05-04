import { useEffect, useMemo, useState } from "react";
import { X, ExternalLink, Bug, Sparkles } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { useFeedbackStore } from "@/state/feedbackStore";
import { useSearchStore } from "@/state/searchStore";
import { useBranchStore } from "@/state/branchStore";
import { useIndexStore } from "@/state/indexStore";
import {
  buildSnapshot,
  feedbackIssueUrl,
  TUNING_PROBLEMS,
  type BugReport,
  type HytaleVersionInfo,
  type TuningProblem,
  type TuningReport,
} from "@/lib/feedback";
import type { Slot } from "@/lib/patcher";

// package.json version is small + static; pull it once at build time.
// Vite resolves this to the literal string.
import pkg from "../../package.json";
const APP_VERSION: string = pkg.version;

/**
 * "Help us improve Atlas" feedback modal. Two modes:
 *   - Bug:    title + description, plus auto-attached env block.
 *   - Tuning: query (prefilled from current search), problem dropdown,
 *             "what's wrong" + "what I expected to see" prose, plus a
 *             toggle for whether to attach the live results snapshot.
 *             When the snapshot is toggled off the user picks which
 *             mounted index they're commenting on.
 *
 * Submission is a pre-filled GitHub issue URL opened in the OS browser.
 * The user reviews the body before clicking Create on GitHub, so we
 * don't risk sending anything they didn't intend.
 */
export function FeedbackModal() {
  const open = useFeedbackStore((s) => s.open);
  const mode = useFeedbackStore((s) => s.mode);
  const setMode = useFeedbackStore((s) => s.setMode);
  const hide = useFeedbackStore((s) => s.hide);

  // Close on Escape.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") hide();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, hide]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Send feedback"
        className="flex max-h-[90vh] w-full max-w-xl flex-col overflow-hidden rounded-lg border border-border-subtle bg-bg-surface shadow-xl"
      >
        <header className="flex shrink-0 items-center gap-3 border-b border-border-subtle px-4 py-3">
          <Sparkles size={16} className="text-accent-primary" />
          <h2 className="flex-1 text-sm font-medium text-fg-primary">
            Help us improve Atlas
          </h2>
          <button
            type="button"
            onClick={hide}
            aria-label="Close"
            className="rounded p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
          >
            <X size={16} />
          </button>
        </header>
        <div className="flex shrink-0 gap-1 border-b border-border-subtle px-4 pt-3">
          <ModeTab
            label="Search tuning"
            icon={<Sparkles size={13} />}
            active={mode === "tuning"}
            onClick={() => setMode("tuning")}
          />
          <ModeTab
            label="Bug report"
            icon={<Bug size={13} />}
            active={mode === "bug"}
            onClick={() => setMode("bug")}
          />
        </div>
        <div className="min-h-0 flex-1 overflow-auto px-4 py-4">
          {mode === "tuning" ? <TuningForm /> : <BugForm />}
        </div>
      </div>
    </div>
  );
}

function ModeTab({
  label,
  icon,
  active,
  onClick,
}: {
  label: string;
  icon: React.ReactNode;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-t border-b-2 px-3 py-1.5 text-xs",
        active
          ? "border-accent-primary text-fg-primary"
          : "border-transparent text-fg-secondary hover:text-fg-primary",
      )}
    >
      {icon}
      {label}
    </button>
  );
}

function useHytaleVersion(): HytaleVersionInfo {
  const activeBranch = useBranchStore((s) => s.active);
  const overview = useIndexStore((s) => s.overview);
  return {
    active_branch: activeBranch,
    release_indexed_at: overview?.release.indexed_at ?? null,
    pre_release_indexed_at: overview?.pre_release.indexed_at ?? null,
    app_version: APP_VERSION,
  };
}

function BugForm() {
  const hide = useFeedbackStore((s) => s.hide);
  const version = useHytaleVersion();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const canSubmit = title.trim().length > 0;

  const submit = () => {
    const report: BugReport = {
      kind: "bug",
      title: title.trim(),
      description: description.trim(),
      hytale_version: version,
    };
    void openUrl(feedbackIssueUrl(report));
    toast.success("Opening GitHub to finish your report");
    hide();
  };

  return (
    <div className="flex flex-col gap-3">
      <Field label="Title">
        <input
          type="text"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="Short summary of what broke"
          className={inputClass}
        />
      </Field>
      <Field label="Description">
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="What were you doing when it happened? What did you expect, and what actually happened?"
          rows={6}
          className={cn(inputClass, "resize-y")}
        />
      </Field>
      <VersionPreview info={version} />
      <SubmitRow disabled={!canSubmit} onSubmit={submit} onCancel={hide} />
    </div>
  );
}

function TuningForm() {
  const hide = useFeedbackStore((s) => s.hide);
  const version = useHytaleVersion();

  const currentQuery = useSearchStore((s) => s.query);
  const currentSlot = useSearchStore((s) => s.slot);
  const currentSection = useSearchStore((s) => s.section);
  const currentHits = useSearchStore((s) => s.hits);
  const branchSlot = useBranchStore((s) => s.active);

  const [query, setQuery] = useState(currentQuery);
  const [problem, setProblem] = useState<TuningProblem>(TUNING_PROBLEMS[0]);
  const [description, setDescription] = useState("");
  const [callout, setCallout] = useState("");
  const [includeSnapshot, setIncludeSnapshot] = useState(true);
  const [manualSlot, setManualSlot] = useState<Slot>(currentSlot ?? branchSlot);

  const snapshot = useMemo(() => {
    if (!includeSnapshot) return null;
    if (!currentSlot || currentHits.length === 0) return null;
    return buildSnapshot({
      query: currentQuery,
      slot: currentSlot,
      section: currentSection,
      hits: currentHits,
    });
  }, [includeSnapshot, currentQuery, currentSlot, currentSection, currentHits]);

  // No-snapshot fallback: only mounts that have ever been indexed are
  // selectable. We surface both slots; disabled rows clue the user in
  // when only one is available.
  const overview = useIndexStore((s) => s.overview);
  const slotOptions: { slot: Slot; label: string; available: boolean }[] = [
    {
      slot: "release",
      label: "Release",
      available: overview?.release.ready === true,
    },
    {
      slot: "pre-release",
      label: "Pre-release",
      available: overview?.pre_release.ready === true,
    },
  ];

  const canSubmit = query.trim().length > 0;
  const submit = () => {
    const report: TuningReport = {
      kind: "tuning",
      query: query.trim(),
      problem,
      description: description.trim(),
      callout: callout.trim(),
      results_snapshot: snapshot,
      manual_slot: includeSnapshot ? null : manualSlot,
      hytale_version: version,
    };
    void openUrl(feedbackIssueUrl(report));
    toast.success("Opening GitHub to finish your report");
    hide();
  };

  return (
    <div className="flex flex-col gap-3">
      <Field label="Query">
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="The search you ran"
          className={inputClass}
        />
      </Field>
      <Field label="Problem">
        <select
          value={problem}
          onChange={(e) => setProblem(e.target.value as TuningProblem)}
          className={inputClass}
        >
          {TUNING_PROBLEMS.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Callout specific results we should focus on">
        <textarea
          value={callout}
          onChange={(e) => setCallout(e.target.value)}
          placeholder="Optional. Name the file/guide(s) relevant to your issue. Items that should be present that aren't, or are displaying too high or low given your search query, etc."
          rows={3}
          className={cn(inputClass, "resize-y")}
        />
      </Field>
      <Field label="What's wrong with these results?">
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="Optional. The more specific, the more we can act on it."
          rows={3}
          className={cn(inputClass, "resize-y")}
        />
      </Field>

      <div className="flex flex-col gap-2 rounded border border-border-subtle bg-bg-base px-3 py-2.5">
        <label className="flex items-start gap-2 text-xs text-fg-primary">
          <input
            type="checkbox"
            checked={includeSnapshot}
            onChange={(e) => setIncludeSnapshot(e.target.checked)}
            className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-accent-primary"
          />
          <span>
            Attach my current search results{" "}
            <span className="text-fg-muted">
              (Attaching results makes things easier for us to track, so if you
              uncheck this please explain why above)
            </span>
          </span>
        </label>
        {includeSnapshot ? (
          <p className="pl-5 text-[11px] text-fg-muted">
            {snapshot
              ? `${snapshot.hit_count} result${snapshot.hit_count === 1 ? "" : "s"} from "${snapshot.query || "(empty query)"}" will be attached.`
              : "No active results to attach. Run a search first or untoggle this."}
          </p>
        ) : (
          <div className="pl-5">
            <p className="mb-1.5 text-[11px] text-fg-muted">
              Tag your report with the index it applies to:
            </p>
            <div className="flex gap-1">
              {slotOptions.map((opt) => (
                <button
                  key={opt.slot}
                  type="button"
                  disabled={!opt.available}
                  onClick={() => setManualSlot(opt.slot)}
                  className={cn(
                    "rounded border px-2 py-1 text-[11px]",
                    manualSlot === opt.slot && opt.available
                      ? "border-accent-primary bg-bg-elevated text-fg-primary"
                      : "border-border-subtle text-fg-secondary hover:border-accent-secondary",
                    !opt.available && "cursor-not-allowed opacity-40",
                  )}
                >
                  {opt.label}
                  {!opt.available && " (not built)"}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      <VersionPreview info={version} />
      <SubmitRow disabled={!canSubmit} onSubmit={submit} onCancel={hide} />
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[11px] font-medium uppercase tracking-wide text-fg-muted">
        {label}
      </span>
      {children}
    </label>
  );
}

const inputClass = cn(
  "w-full rounded border border-border-subtle bg-bg-base px-2.5 py-1.5",
  "text-xs text-fg-primary placeholder:text-fg-muted",
  "focus:border-accent-primary focus:outline-none",
);

function VersionPreview({ info }: { info: HytaleVersionInfo }) {
  return (
    <div className="rounded border border-border-subtle bg-bg-base px-3 py-2 text-[11px] text-fg-muted">
      <span className="text-fg-secondary">Auto-attached:</span>{" "}
      branch <code className="text-fg-primary">{info.active_branch}</code>,
      Atlas <code className="text-fg-primary">{info.app_version}</code>
    </div>
  );
}

function SubmitRow({
  disabled,
  onSubmit,
  onCancel,
}: {
  disabled: boolean;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="mt-1 flex items-center justify-end gap-2">
      <button
        type="button"
        onClick={onCancel}
        className="rounded border border-border-subtle bg-bg-base px-3 py-1.5 text-xs text-fg-secondary hover:border-accent-secondary hover:text-fg-primary"
      >
        Cancel
      </button>
      <button
        type="button"
        onClick={onSubmit}
        disabled={disabled}
        className={cn(
          "inline-flex items-center gap-1.5 rounded border px-3 py-1.5 text-xs",
          disabled
            ? "cursor-not-allowed border-border-subtle bg-bg-base text-fg-muted opacity-60"
            : "border-accent-primary bg-accent-primary text-accent-primary-fg hover:opacity-90",
        )}
      >
        Open report on GitHub
        <ExternalLink size={12} strokeWidth={1.75} />
      </button>
    </div>
  );
}
