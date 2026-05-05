import { useEffect, useMemo, useState, type ReactNode, type Ref } from "react";
import type { ThemedToken } from "shiki";
import { ensureHighlighter, SHIKI_THEME, type SupportedLang } from "@/lib/shiki";
import { cn } from "@/lib/utils";

/** A pre-rendered React node to splice into the source render at a given
 *  1-based line. The most common producer is the inline-Javadoc box that
 *  drops above each method declaration. */
export type InlineAnchor = {
  /** 1-based line number; the anchor renders BEFORE this line. */
  startLine: number;
  node: ReactNode;
};

/** Map a file path's extension to a shiki language id. Falls back to
 *  `java` for the common case (decompiled source) and `text` for
 *  unknown extensions, which renders without highlighting. */
export function langForPath(path: string): SupportedLang | "text" {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "java":
      return "java";
    case "kt":
    case "kts":
      return "kotlin";
    case "json":
      return "json";
    case "js":
    case "mjs":
    case "cjs":
      return "javascript";
    case "ts":
      return "typescript";
    case "tsx":
      return "tsx";
    case "rs":
      return "rust";
    case "py":
      return "python";
    case "sh":
    case "bash":
      return "bash";
    case "yaml":
    case "yml":
      return "yaml";
    case "toml":
      return "toml";
    case "xml":
      return "xml";
    case "html":
      return "html";
    case "css":
      return "css";
    case "scala":
      return "scala";
    case "groovy":
    case "gradle":
      return "groovy";
    default:
      return "text";
  }
}

/** Java/Kotlin/etc source viewer with shiki syntax highlighting,
 *  per-line anchors, line numbers, and a highlighted preview line.
 *  Drop-in replacement for the inline `<pre>` block previously hosted
 *  by RightPanel + SiblingPane.
 *
 *  Renders nothing fancy until the highlighter resolves on first paint;
 *  the plain text shows immediately so the viewer never appears empty.
 */
export function SourceCode({
  content,
  language,
  previewLine,
  previewRef,
  inlineAnchors,
  className,
  previewColorVar,
  visibleLines,
}: {
  content: string;
  /** Shiki language id. Pass `"text"` to skip highlighting. */
  language: SupportedLang | "text";
  previewLine: number | null;
  previewRef?: Ref<HTMLSpanElement>;
  /** Pre-rendered nodes to splice in before specific lines. */
  inlineAnchors?: InlineAnchor[];
  className?: string;
  /** CSS variable name used to tint the preview-line band. Defaults to
   *  the accent. tints by the active hit's section. */
  previewColorVar?: string;
  /** When set, only these 1-based line numbers render; everything else
   *  is elided to single "…" rows. Powers FindOverlay's collapse-to-
   *  context toggle. */
  visibleLines?: Set<number>;
}) {
  const lines = useMemo(() => content.split("\n"), [content]);
  const [tokens, setTokens] = useState<ThemedToken[][] | null>(null);

  useEffect(() => {
    if (language === "text") {
      setTokens(null);
      return;
    }
    let cancelled = false;
    ensureHighlighter()
      .then((h) => {
        if (cancelled) return;
        try {
          const result = h.codeToTokens(content, {
            lang: language,
            theme: SHIKI_THEME,
          });
          setTokens(result.tokens);
        } catch {
          if (!cancelled) setTokens(null);
        }
      })
      .catch(() => {
        if (!cancelled) setTokens(null);
      });
    return () => {
      cancelled = true;
    };
  }, [content, language]);

  // Auto-fit the line-number gutter so two-digit files don't waste
  // 40px of horizontal space and ten-digit files don't truncate.
  const lineCount = lines.length;
  const gutterClass =
    lineCount >= 10000
      ? "w-12"
      : lineCount >= 1000
        ? "w-10"
        : lineCount >= 100
          ? "w-9"
          : "w-8";

  const anchorByLine = useMemo(() => {
    const m = new Map<number, ReactNode>();
    if (inlineAnchors) {
      for (const a of inlineAnchors) m.set(a.startLine, a.node);
    }
    return m;
  }, [inlineAnchors]);

  const previewBg = previewColorVar
    ? `color-mix(in srgb, var(${previewColorVar}) 14%, transparent)`
    : "color-mix(in srgb, var(--accent-primary) 12%, transparent)";

  // When collapse-to-context is active, walk the line list and emit
  // gaps as "…" placeholders so users still see that lines were elided.
  const out: ReactNode[] = [];
  let lastVisible = 0;
  for (let idx = 0; idx < lines.length; idx++) {
    const lineNo = idx + 1;
    if (visibleLines && !visibleLines.has(lineNo)) continue;
    if (visibleLines && lineNo - lastVisible > 1 && lastVisible !== 0) {
      out.push(
        <span
          key={`gap-${lineNo}`}
          className="block select-none px-3 py-0.5 text-[10px] text-fg-muted"
        >
          ⋯
        </span>,
      );
    }
    lastVisible = lineNo;
    const rawLine = lines[idx];
    const lineTokens = tokens?.[idx];
    const isPreview = previewLine !== null && previewLine === lineNo;
    const anchor = anchorByLine.get(lineNo);
    out.push(
      <span key={idx} className="block">
        {anchor && <span className="block py-1">{anchor}</span>}
        <span
          ref={isPreview ? previewRef : undefined}
          style={isPreview ? { backgroundColor: previewBg } : undefined}
          className={cn(
            "block whitespace-pre",
            isPreview ? "text-fg-primary" : "text-fg-secondary",
          )}
        >
          <span
            className={cn(
              "mr-3 inline-block select-none text-right text-fg-muted",
              gutterClass,
            )}
          >
            {lineNo}
          </span>
          {lineTokens && lineTokens.length > 0 ? (
            lineTokens.map((t, i) => (
              <span key={i} style={{ color: t.color }}>
                {t.content}
              </span>
            ))
          ) : rawLine.length > 0 ? (
            rawLine
          ) : (
            "\u00A0"
          )}
        </span>
      </span>,
    );
  }

  return (
    <div
      className={cn(
        "px-3 py-2 font-mono text-[12px] leading-5",
        className,
      )}
    >
      {out}
    </div>
  );
}
