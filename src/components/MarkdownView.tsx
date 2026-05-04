import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { cn } from "@/lib/utils";
import { highlightToHtml } from "@/lib/shiki";

/** Map an HM repo `rel_path` (e.g. `content/docs/en/guides/plugin/foo.mdx`)
 *  to the live public URL on hytalemodding.dev. */
export function hmDocUrl(relPath: string | undefined | null): string | null {
  if (!relPath) return null;
  const p = relPath.replace(/\\/g, "/");
  if (!p.startsWith("content/docs/")) return null;
  let stripped = p.slice("content/".length);
  stripped = stripped.replace(/\.(mdx?|md)$/i, "");
  stripped = stripped.replace(/\/index$/i, "");
  stripped = stripped.replace(/^docs\/[a-z]{2}(?:-[A-Z]{2})?(\/|$)/, "docs$1");
  return `https://hytalemodding.dev/${stripped}`;
}

/** Renders an HM Modding `.mdx` document inside the file viewer.
 *
 *  upgrades:
 *    - Sticky right-gutter TOC built from h2 / h3 with active-section
 *      tracking via IntersectionObserver.
 *    - Code blocks rendered via shiki (same theme as source viewer).
 *    - Headings use the sans-serif stack at heavier weights; the
 *      decorative display face is reserved for app branding now.
 *    - Body line-height drops from 7 to 6 for tighter prose.
 */
export function MarkdownView({
  source,
  path,
}: {
  source: string;
  path?: string;
}) {
  const md = mdxToMarkdown(source);
  const url = hmDocUrl(path);

  const proseRef = useRef<HTMLDivElement>(null);
  const [headings, setHeadings] = useState<{ id: string; text: string; level: 2 | 3 }[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);

  // Collect headings after each render for the TOC.
  useEffect(() => {
    const el = proseRef.current;
    if (!el) return;
    const found: { id: string; text: string; level: 2 | 3 }[] = [];
    el.querySelectorAll<HTMLElement>("h2, h3").forEach((h) => {
      if (!h.id) return;
      found.push({
        id: h.id,
        text: h.textContent ?? "",
        level: h.tagName === "H2" ? 2 : 3,
      });
    });
    setHeadings(found);
  }, [md]);

  // Track the currently-visible section. Pick the heading whose top
  // is closest to the viewport top from above.
  useEffect(() => {
    const el = proseRef.current;
    if (!el || headings.length === 0) return;
    const targets = headings
      .map((h) => el.querySelector<HTMLElement>(`#${cssEscape(h.id)}`))
      .filter((x): x is HTMLElement => x !== null);
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (visible) setActiveId(visible.target.id);
      },
      { rootMargin: "-10% 0px -75% 0px", threshold: 0 },
    );
    targets.forEach((t) => observer.observe(t));
    return () => observer.disconnect();
  }, [headings]);

  return (
    <div className="relative flex">
      <div
        ref={proseRef}
        className={cn(
          "prose-atlas mx-auto w-full max-w-[88ch]",
          "px-6 py-5 text-[14px] leading-6 text-fg-secondary",
        )}
      >
        {url && (
          <a
            href={url}
            onClick={(e) => {
              // Tauri webviews block target="_blank"; route through the
              // opener plugin so the OS browser actually opens.
              e.preventDefault();
              void openUrl(url);
            }}
            className="mb-4 inline-flex items-center gap-1.5 rounded border border-border-subtle bg-bg-elevated px-2.5 py-1 text-xs text-fg-secondary hover:border-accent-secondary hover:text-accent-secondary"
          >
            View on hytalemodding.dev
            <ExternalLink size={12} strokeWidth={1.75} />
          </a>
        )}
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          components={{
            h1: ({ node: _node, children, ...p }) => (
              <h1
                id={slugify(textOf(children))}
                className="mb-4 mt-2 border-b border-border-subtle pb-2 text-2xl font-semibold text-fg-primary"
                {...p}
              >
                {children}
              </h1>
            ),
            h2: ({ node: _node, children, ...p }) => (
              <h2
                id={slugify(textOf(children))}
                className="mb-3 mt-6 text-xl font-semibold text-fg-primary"
                {...p}
              >
                {children}
              </h2>
            ),
            h3: ({ node: _node, children, ...p }) => (
              <h3
                id={slugify(textOf(children))}
                className="mb-2 mt-5 text-lg font-semibold text-fg-primary"
                {...p}
              >
                {children}
              </h3>
            ),
            h4: ({ node: _node, ...p }) => (
              <h4 className="mb-2 mt-4 text-base font-semibold text-fg-primary" {...p} />
            ),
            p: ({ node: _node, ...p }) => <p className="my-3" {...p} />,
            ul: ({ node: _node, ...p }) => (
              <ul className="my-3 list-disc space-y-1 pl-6" {...p} />
            ),
            ol: ({ node: _node, ...p }) => (
              <ol className="my-3 list-decimal space-y-1 pl-6" {...p} />
            ),
            li: ({ node: _node, ...p }) => <li className="leading-6" {...p} />,
            a: ({ node: _node, href, onClick: _onClick, ...p }) => (
              <a
                className="text-accent-secondary underline underline-offset-2 hover:text-accent-primary"
                href={href}
                onClick={(e) => {
                  // Tauri webviews block target="_blank"; route external
                  // links through the opener plugin instead.
                  if (!href) return;
                  e.preventDefault();
                  void openUrl(href);
                }}
                {...p}
              />
            ),
            blockquote: ({ node: _node, ...p }) => (
              <blockquote
                className="my-4 border-l-2 border-accent-secondary bg-bg-elevated/50 px-4 py-2 leading-7 text-fg-secondary"
                {...p}
              />
            ),
            hr: ({ node: _node, ...p }) => (
              <hr className="my-6 border-border-subtle" {...p} />
            ),
            table: ({ node: _node, ...p }) => (
              <div
                className="my-4 overflow-auto rounded border border-border-subtle"
                style={{
                  maskImage:
                    "linear-gradient(to right, black 0, black calc(100% - 24px), transparent 100%)",
                }}
              >
                <table className="w-full border-collapse text-sm" {...p} />
              </div>
            ),
            th: ({ node: _node, ...p }) => (
              <th
                className="border border-border-subtle bg-bg-elevated px-3 py-1.5 text-left font-medium text-fg-primary"
                {...p}
              />
            ),
            td: ({ node: _node, ...p }) => (
              <td className="border border-border-subtle px-3 py-1.5" {...p} />
            ),
            code: ({ node: _node, className, children, ...p }) => {
              const isBlock = /\blanguage-/.test(className ?? "");
              if (isBlock) {
                const lang = (className ?? "")
                  .split(/\s+/)
                  .find((c) => c.startsWith("language-"))
                  ?.replace(/^language-/, "") ?? "";
                return (
                  <ShikiBlock
                    code={String(children).replace(/\n$/, "")}
                    lang={lang}
                  />
                );
              }
              return (
                <code
                  className="rounded bg-bg-elevated px-1 py-0.5 font-mono text-[12.5px] text-fg-primary"
                  {...p}
                >
                  {children}
                </code>
              );
            },
            pre: ({ node: _node, children }) => {
              // When the inner code is one of our ShikiBlocks it manages
              // its own pre, so just pass-through.
              return <>{children}</>;
            },
            img: ({ node: _node, ...p }) => (
              // eslint-disable-next-line jsx-a11y/alt-text
              <img className="my-3 max-w-full rounded" loading="lazy" {...p} />
            ),
            strong: ({ node: _node, ...p }) => (
              <strong className="font-semibold text-fg-primary" {...p} />
            ),
            em: ({ node: _node, ...p }) => <em className="italic" {...p} />,
          }}
        >
          {md}
        </ReactMarkdown>
      </div>
      {headings.length > 1 && (
        <Toc headings={headings} activeId={activeId} containerRef={proseRef} />
      )}
    </div>
  );
}

function Toc({
  headings,
  activeId,
  containerRef,
}: {
  headings: { id: string; text: string; level: 2 | 3 }[];
  activeId: string | null;
  containerRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <nav className="sticky top-0 hidden h-fit max-h-screen shrink-0 overflow-auto px-4 py-5 text-xs lg:block">
      <p className="mb-2 text-[10px] font-medium uppercase tracking-wide text-fg-muted">
        On this page
      </p>
      <ul className="flex flex-col gap-1 border-l border-border-subtle pl-3">
        {headings.map((h) => {
          const active = h.id === activeId;
          return (
            <li key={h.id} className={h.level === 3 ? "ml-3" : ""}>
              <button
                type="button"
                onClick={() => {
                  const el = containerRef.current?.querySelector(
                    `#${cssEscape(h.id)}`,
                  );
                  el?.scrollIntoView({ behavior: "smooth", block: "start" });
                }}
                className={cn(
                  "block w-full truncate text-left transition-colors",
                  active
                    ? "text-fg-primary"
                    : "text-fg-muted hover:text-fg-secondary",
                )}
                title={h.text}
              >
                {h.text}
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

/** Async shiki-highlighted code block. Falls back to a plain `<pre>`
 *  before the highlighter resolves on first paint. */
function ShikiBlock({ code, lang }: { code: string; lang: string }) {
  const [html, setHtml] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    highlightToHtml(code, lang)
      .then((h) => {
        if (!cancelled) setHtml(h);
      })
      .catch(() => {
        if (!cancelled) setHtml(null);
      });
    return () => {
      cancelled = true;
    };
  }, [code, lang]);
  if (html) {
    return (
      <div
        className="my-4 overflow-auto rounded-md border border-border-subtle bg-bg-base text-[12.5px] leading-5"
        // dangerouslySetInnerHTML is bounded to shiki output we generated
        // ourselves from local content; no untrusted HTML reaches this.
        dangerouslySetInnerHTML={{ __html: html }}
      />
    );
  }
  return (
    <pre className="my-4 overflow-auto rounded-md border border-border-subtle bg-bg-base p-3 font-mono text-[12.5px] leading-5 text-fg-primary">
      <code>{code}</code>
    </pre>
  );
}

function textOf(children: React.ReactNode): string {
  if (typeof children === "string") return children;
  if (typeof children === "number") return String(children);
  if (Array.isArray(children)) return children.map(textOf).join("");
  if (children && typeof children === "object" && "props" in children) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return textOf((children as any).props?.children ?? "");
  }
  return "";
}

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-")
    .slice(0, 80);
}

function cssEscape(s: string): string {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  if (typeof (window as any).CSS?.escape === "function") {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return (window as any).CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, (c) => `\\${c}`);
}

/** Convert HM's `.mdx` to plain markdown the renderer can handle. */
function mdxToMarkdown(src: string): string {
  let out = src;

  out = out.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "");
  out = out.replace(/^[ \t]*import\s+[^\n]*\n/gm, "");
  out = out.replace(/^[ \t]*export\s+[^\n]*\n/gm, "");

  out = out.replace(
    /<Callout\b([^>]*)>([\s\S]*?)<\/Callout>/g,
    (_m, attrs: string, body: string) => {
      const title = attrMatch(attrs, "title");
      const type = attrMatch(attrs, "type");
      const heading = title
        ? `**${title}**`
        : type
          ? `**${capitalize(type)}**`
          : "**Note**";
      const indented = body
        .trim()
        .split(/\r?\n/)
        .map((line) => `> ${line}`)
        .join("\n");
      return `> ${heading}\n>\n${indented}\n`;
    },
  );

  out = out.replace(
    /<OfficialDocumentationNotice\b[^>]*\/?>(?:[\s\S]*?<\/OfficialDocumentationNotice>)?/g,
    "_Excerpted from official Hypixel documentation._\n",
  );

  out = out.replace(/<[A-Z][A-Za-z0-9]*\b[^>]*\/>/g, "");

  for (let i = 0; i < 4; i++) {
    const next = out.replace(
      /<([A-Z][A-Za-z0-9]*)\b[^>]*>([\s\S]*?)<\/\1>/g,
      (_m, _tag, body: string) => body,
    );
    if (next === out) break;
    out = next;
  }

  out = out.replace(/<\/?[A-Z][A-Za-z0-9]*\b[^>]*>/g, "");

  return out;
}

function attrMatch(attrs: string, name: string): string | null {
  const m = new RegExp(`${name}\\s*=\\s*["']([^"']*)["']`).exec(attrs);
  return m ? m[1] : null;
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}
