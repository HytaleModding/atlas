import { useEffect, useRef, useState } from "react";
import { ArrowUpDown, ChevronDown } from "lucide-react";
import { readSource, type SearchHit } from "@/lib/indexer";
import { useUIPrefsStore } from "@/state/uiPrefsStore";
import { MarkdownView } from "./MarkdownView";
import { SourceCode, langForPath } from "./SourceCode";

/** Cross-section pair view: shows the Javadoc when the active hit is
 *  source code, or the source when the active hit is a Javadoc. Phase
 *  fuses source+Javadoc into the main viewer for the common case,
 *  so this pane now shrinks to handling only asset/doc-source pairs
 *  that aren't married into one row.
 *
 *  adds a top drag handle that persists the pane's height;  *  tints the section badge background with the sibling's section colour.
 */
export function SiblingPane({
  sibling,
  onSwap,
  onCollapse,
}: {
  sibling: SearchHit;
  onSwap: () => void;
  onCollapse: () => void;
}) {
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const height = useUIPrefsStore((s) => s.siblingPaneHeight);
  const setHeight = useUIPrefsStore((s) => s.setSiblingPaneHeight);

  const sectionRef = useRef<HTMLElement>(null);
  const [dragging, setDragging] = useState(false);
  const startYRef = useRef(0);
  const startHeightRef = useRef(0);

  function onDragStart(e: React.MouseEvent) {
    setDragging(true);
    startYRef.current = e.clientY;
    startHeightRef.current =
      sectionRef.current?.getBoundingClientRect().height ?? 0;
  }

  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => {
      const delta = startYRef.current - e.clientY;
      const next = Math.max(
        120,
        Math.min(800, startHeightRef.current + delta),
      );
      setHeight(next);
    };
    const onUp = () => setDragging(false);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragging, setHeight]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    readSource(sibling.slot, sibling.path, sibling.source_type)
      .then((text) => {
        if (cancelled) return;
        setContent(text);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(String(err));
        setContent(null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sibling.slot, sibling.path, sibling.source_type]);

  const isSource = sibling.source_type === "source";
  const isJavadoc = sibling.source_type === "hypixel_doc";
  const label = isSource ? "Source" : isJavadoc ? "Javadoc" : "Pair";
  const sectionVar =
    sibling.source_type === "hypixel_doc"
      ? "--section-javadocs"
      : sibling.source_type === "hm_doc"
        ? "--section-guides"
        : sibling.source_type === "asset"
          ? "--section-assets"
          : "--section-source";

  const previewLine = sibling.preview_line;
  const previewRef = (el: HTMLSpanElement | null) => {
    if (el && previewLine !== null) {
      el.scrollIntoView({ block: "center", behavior: "instant" });
    }
  };

  return (
    <section
      ref={sectionRef}
      className="relative flex min-h-0 shrink-0 flex-col border-t border-border-subtle bg-bg-surface"
      style={{
        height: height !== null ? `${height}px` : "50%",
        userSelect: dragging ? "none" : undefined,
      }}
    >
      <div
        onMouseDown={onDragStart}
        title="Drag to resize"
        className="absolute left-0 top-0 z-20 h-1 w-full cursor-row-resize hover:bg-accent-primary/40"
      />
      <header
        className="flex shrink-0 items-center gap-2 border-b border-border-subtle px-3"
        style={{ height: "40px" }}
      >
        <span
          className="shrink-0 rounded-sm px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide"
          style={{
            backgroundColor: `color-mix(in srgb, var(${sectionVar}) 15%, transparent)`,
            color: `var(${sectionVar})`,
          }}
          title={`${label} pair for the active hit`}
        >
          {label}
        </span>
        <div className="min-w-0 flex-1">
          <p
            className="truncate font-mono text-xs text-fg-primary"
            title={sibling.fqn}
          >
            {sibling.filename || sibling.fqn}
          </p>
          <p
            className="truncate font-mono text-[10px] text-fg-muted"
            title={sibling.path}
          >
            {sibling.package || "(default package)"}
          </p>
        </div>
        <button
          type="button"
          onClick={onSwap}
          aria-label="Swap with main view"
          title="Swap with main view"
          className="shrink-0 rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
        >
          <ArrowUpDown size={14} strokeWidth={1.75} />
        </button>
        <button
          type="button"
          onClick={onCollapse}
          aria-label="Collapse pair view"
          title="Collapse"
          className="shrink-0 rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
        >
          <ChevronDown size={14} strokeWidth={1.75} />
        </button>
      </header>

      {loading && content === null ? (
        <div className="flex flex-1 items-center justify-center text-fg-muted">
          <span className="text-xs">Loading…</span>
        </div>
      ) : error ? (
        <div className="flex flex-1 items-center justify-center px-4 text-center">
          <span className="font-mono text-[11px] text-destructive">
            {error}
          </span>
        </div>
      ) : content === null ? null : isJavadoc ? (
        <div className="min-h-0 flex-1 overflow-auto bg-bg-base px-4 py-3 text-[12px] leading-5 text-fg-secondary">
          <div className="whitespace-pre-wrap break-words">{content}</div>
        </div>
      ) : sibling.source_type === "hm_doc" ? (
        <div className="min-h-0 flex-1 overflow-auto bg-bg-base">
          <MarkdownView source={content} path={sibling.path} />
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto bg-bg-base">
          <SourceCode
            content={content}
            language={langForPath(sibling.path)}
            previewLine={previewLine}
            previewRef={previewRef}
            previewColorVar={sectionVar}
          />
        </div>
      )}
    </section>
  );
}
