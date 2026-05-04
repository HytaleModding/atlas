import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Minimize2, X } from "lucide-react";
import {
  applyFindMarks,
  clearFindMarks,
  setActiveMark,
} from "@/lib/findInPage";
import { cn } from "@/lib/utils";

/** Floating Ctrl+F-style find overlay docked top-right inside the file
 *  viewer. Drives DOM-level highlighting via `lib/findInPage.ts` against
 *  whatever element `containerRef` points at, so it works for source
 *  code, markdown HM docs, and Hypixel Javadoc text alike.
 *
 *  upgrades:
 *    - F3 / Shift+F3 step next/prev even when the input isn't focused
 *    - User-typed queries persist across hit changes within a session
 *      (parent decides when to re-seed)
 *    - Optional collapse-to-context toggle (parent renders only ±5
 *      lines around each match in its source view)
 *    - Optional section-tinted border
 */
export function FindOverlay({
  open,
  onOpenChange,
  containerRef,
  seedQuery,
  contentVersion,
  collapseToContext,
  onToggleCollapse,
  borderColorVar,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  containerRef: React.RefObject<HTMLElement | null>;
  seedQuery: string;
  contentVersion: number;
  /** When true, the parent renders a context-only view of the source.
   *  This component just toggles the prop; the parent decides what
   *  "context" means. */
  collapseToContext?: boolean;
  onToggleCollapse?: () => void;
  /** CSS variable name (without `var()` wrapper) used to tint the
   *  overlay's border. Falls back to the subtle border token. */
  borderColorVar?: string;
}) {
  const [query, setQuery] = useState(seedQuery);
  const [matches, setMatches] = useState<HTMLElement[]>([]);
  const [activeIdx, setActiveIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const userTouchedRef = useRef(false);
  const lastSeedRef = useRef(seedQuery);

  // Re-seed when seedQuery actually changes. once the user has
  // typed, their query survives subsequent hit swaps; the parent owns
  // when to push a new seed by changing the prop.
  useEffect(() => {
    if (seedQuery === lastSeedRef.current) return;
    lastSeedRef.current = seedQuery;
    if (!userTouchedRef.current || seedQuery.length > 0) {
      setQuery(seedQuery);
    }
  }, [seedQuery]);

  useEffect(() => {
    if (open) {
      requestAnimationFrame(() => inputRef.current?.select());
    } else {
      const root = containerRef.current;
      if (root) clearFindMarks(root);
      setMatches([]);
      setActiveIdx(0);
      userTouchedRef.current = false;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Re-apply highlights on query / content / open change.
  useEffect(() => {
    if (!open) return;
    const root = containerRef.current;
    if (!root) return;
    clearFindMarks(root);
    if (query.length === 0) {
      setMatches([]);
      setActiveIdx(0);
      return;
    }
    const found = applyFindMarks(root, query);
    setMatches(found);
    setActiveIdx(0);
    if (found.length > 0) {
      setActiveMark(found, 0, "instant");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, query, contentVersion]);

  useEffect(() => {
    if (matches.length === 0) return;
    setActiveMark(matches, activeIdx, "smooth");
  }, [matches, activeIdx]);

  useEffect(() => {
    const root = containerRef.current;
    return () => {
      if (root) clearFindMarks(root);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // F3 / Shift+F3 step matches even when the input isn't focused.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F3") return;
      e.preventDefault();
      step(e.shiftKey ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, matches.length]);

  if (!open) return null;

  const total = matches.length;
  const display = total === 0 ? "0/0" : `${activeIdx + 1}/${total}`;

  function step(delta: number) {
    if (matches.length === 0) return;
    setActiveIdx((i) => {
      const n = matches.length;
      return ((i + delta) % n + n) % n;
    });
  }

  return (
    <div
      className="absolute right-3 top-2 z-10 flex items-center gap-1 rounded-md border bg-bg-elevated/95 px-2 py-1 shadow-lg backdrop-blur"
      style={{
        borderColor: borderColorVar
          ? `color-mix(in srgb, var(${borderColorVar}) 45%, transparent)`
          : "var(--border-subtle)",
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onOpenChange(false);
        } else if (e.key === "Enter") {
          e.stopPropagation();
          step(e.shiftKey ? -1 : 1);
        }
      }}
    >
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => {
          userTouchedRef.current = true;
          setQuery(e.target.value);
        }}
        placeholder="Find in page"
        className="w-44 border-none bg-transparent font-mono text-xs text-fg-primary placeholder:text-fg-muted focus:outline-none"
        spellCheck={false}
      />
      <span className="select-none font-mono text-[11px] text-fg-muted">
        {display}
      </span>
      <button
        type="button"
        onClick={() => step(-1)}
        disabled={total === 0}
        aria-label="Previous match"
        className="rounded-sm p-0.5 text-fg-muted hover:bg-bg-surface hover:text-fg-primary disabled:opacity-40"
      >
        <ChevronUp size={14} strokeWidth={1.75} />
      </button>
      <button
        type="button"
        onClick={() => step(1)}
        disabled={total === 0}
        aria-label="Next match"
        className="rounded-sm p-0.5 text-fg-muted hover:bg-bg-surface hover:text-fg-primary disabled:opacity-40"
      >
        <ChevronDown size={14} strokeWidth={1.75} />
      </button>
      {onToggleCollapse && (
        <button
          type="button"
          onClick={onToggleCollapse}
          aria-label="Toggle collapse to context"
          title="Collapse to ±5 lines around matches"
          className={cn(
            "rounded-sm p-0.5 hover:bg-bg-surface hover:text-fg-primary",
            collapseToContext ? "text-accent-primary" : "text-fg-muted",
          )}
        >
          <Minimize2 size={14} strokeWidth={1.75} />
        </button>
      )}
      <button
        type="button"
        onClick={() => onOpenChange(false)}
        aria-label="Close find"
        className="rounded-sm p-0.5 text-fg-muted hover:bg-bg-surface hover:text-fg-primary"
      >
        <X size={14} strokeWidth={1.75} />
      </button>
    </div>
  );
}
