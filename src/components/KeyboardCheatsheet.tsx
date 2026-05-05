import { useEffect, type ReactNode } from "react";
import { X } from "lucide-react";
import { useUIPrefsStore } from "@/state/uiPrefsStore";

/** Keyboard shortcut reference modal opened with `?`. Lists every binding
 *  registered through `useKeymap` so the user can discover search-page
 *  navigation, find-in-page, viewer history, and section toggles without
 *  rummaging through docs. */
export function KeyboardCheatsheet({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const setSeen = useUIPrefsStore((s) => s.setCheatsheetSeen);

  useEffect(() => {
    if (open) setSeen(true);
  }, [open, setSeen]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className="max-h-[80vh] w-[520px] overflow-auto rounded-md border border-border-subtle bg-bg-surface p-5 shadow-xl"
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-sm font-medium text-fg-primary">
            Keyboard shortcuts
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
          >
            <X size={14} strokeWidth={1.75} />
          </button>
        </div>
        <table className="w-full text-xs">
          <tbody className="divide-y divide-border-subtle">
            {SECTIONS.map((section) => (
              <SectionRows
                key={section.title}
                title={section.title}
                rows={section.rows}
              />
            ))}
          </tbody>
        </table>
        <p className="mt-3 text-[11px] text-fg-muted">
          Press <Kbd>Esc</Kbd> to close.
        </p>
      </div>
    </div>
  );
}

const SECTIONS: { title: string; rows: [string, string][] }[] = [
  {
    title: "Search",
    rows: [
      ["j / ↓", "Next result"],
      ["k / ↑", "Previous result"],
      ["Enter", "Open in viewer"],
      ["Esc", "Focus search input"],
      ["↑ in empty input", "Recall query history"],
    ],
  },
  {
    title: "Viewer",
    rows: [
      ["Alt + ←", "Back through viewer history"],
      ["Alt + →", "Forward through viewer history"],
      ["Ctrl/⌘ + F", "Find in page"],
      ["F3 / Shift + F3", "Next / previous match"],
      ["Ctrl + C (no selection)", "Copy fully-qualified name"],
    ],
  },
  {
    title: "Global",
    rows: [["?", "Show this cheatsheet"]],
  },
];

function SectionRows({
  title,
  rows,
}: {
  title: string;
  rows: [string, string][];
}) {
  return (
    <>
      <tr>
        <th
          colSpan={2}
          className="pb-1 pt-3 text-left text-[10px] font-medium uppercase tracking-wide text-fg-muted"
        >
          {title}
        </th>
      </tr>
      {rows.map(([keys, label]) => (
        <tr key={keys}>
          <td className="w-1/3 py-1 pr-3">
            {keys.split(" / ").map((k, i, arr) => (
              <span key={k}>
                <Kbd>{k}</Kbd>
                {i < arr.length - 1 && (
                  <span className="mx-1 text-fg-muted">/</span>
                )}
              </span>
            ))}
          </td>
          <td className="py-1 text-fg-secondary">{label}</td>
        </tr>
      ))}
    </>
  );
}

function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-border-subtle bg-bg-elevated px-1.5 py-0.5 font-mono text-[10px] text-fg-secondary">
      {children}
    </kbd>
  );
}
