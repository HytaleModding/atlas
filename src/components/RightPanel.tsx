import { useEffect, useMemo, useRef, useState } from "react";
import {
  BookOpen,
  ChevronLeft,
  ChevronRight,
  Copy,
  ExternalLink,
  Hash,
  Pin,
  PinOff,
  StickyNote,
  X,
} from "lucide-react";
import { toast } from "sonner";
import {
  cachedGetInlineJavadocs,
  cachedReadSource,
  type InlineJavadoc,
  type SearchHit,
} from "@/lib/indexer";
import { useSearchStore } from "@/state/searchStore";
import { useUIPrefsStore } from "@/state/uiPrefsStore";
import { usePinsStore } from "@/state/pinsStore";
import { useKeymap } from "@/lib/keymap";
import { editorLabel, editorUrl } from "@/lib/editor";
import { cn } from "@/lib/utils";
import { MarkdownView } from "./MarkdownView";
import { FindOverlay } from "./FindOverlay";
import {
  SourceCode,
  langForPath,
  type InlineAnchor,
} from "./SourceCode";
import { InlineJavadocBox } from "./InlineJavadocBox";
import { TabBar } from "./TabBar";
import { useTabsStore } from "@/state/tabsStore";

/** Source section → CSS variable name, used to tint the preview-line band
 *  and the find-overlay border.. */
function sectionVar(sourceType: string): string {
  switch (sourceType) {
    case "hm_doc":
      return "--section-guides";
    case "hypixel_doc":
      return "--section-javadocs";
    case "asset":
      return "--section-assets";
    case "source":
    case "":
    default:
      return "--section-source";
  }
}

/** File viewer right panel per ui-spec.md § File Viewer.
 *
 *  replaces the inline `<pre>` map with a shiki-highlighted
 *  `SourceCode` viewer, drops Javadoc boxes inline above class/method
 *  declarations, adds copy-FQN / copy-method / open-in-editor
 *  header buttons, and binds Alt+arrow viewer history.
 */
export function RightPanel() {
  const selected = useSearchStore((s) => s.selectedHit);
  const viewerBack = useSearchStore((s) => s.viewerBack);
  const viewerForward = useSearchStore((s) => s.viewerForward);
  const activeTabId = useTabsStore((s) => s.activeId);
  const closeTab = useTabsStore((s) => s.closeTab);
  const closeActiveTab = () => {
    if (activeTabId) closeTab(activeTabId);
  };
  const widthRatio = useUIPrefsStore((s) => s.rightPanelWidthRatio);
  const setWidthRatio = useUIPrefsStore((s) => s.setRightPanelWidthRatio);
  const effectiveRatio = widthRatio ?? 0.5;

  const asideRef = useRef<HTMLElement>(null);
  const [dragging, setDragging] = useState(false);

  function onDragStart(e: React.MouseEvent) {
    e.preventDefault();
    setDragging(true);
  }

  useEffect(() => {
    if (!dragging) return;
    // Drag updates the ratio against the live window width so the panel
    // always reads the correct fraction of viewport on every move.
    // Clamp to [0.2, 0.8] so neither side can collapse below ~20%.
    const onMove = (e: MouseEvent) => {
      const w = window.innerWidth;
      if (w <= 0) return;
      const ratio = (w - e.clientX) / w;
      const clamped = Math.max(0.2, Math.min(0.8, ratio));
      setWidthRatio(clamped);
    };
    const onUp = () => setDragging(false);
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragging, setWidthRatio]);

  // Alt+ArrowLeft/Right step viewer history. Scoped to "viewer" so it
  // only fires while the file viewer is mounted; useKeymap handles
  // input-target guards.
  useKeymap(
    [
      {
        key: "ArrowLeft",
        alt: true,
        scope: "viewer",
        handler: () => {
          viewerBack();
        },
      },
      {
        key: "ArrowRight",
        alt: true,
        scope: "viewer",
        handler: () => {
          viewerForward();
        },
      },
    ],
    [viewerBack, viewerForward],
  );

  return (
    <aside
      ref={asideRef}
      className="relative flex shrink-0 flex-col border-l border-border-subtle bg-bg-surface"
      style={{
        width: `${effectiveRatio * 100}%`,
        userSelect: dragging ? "none" : undefined,
      }}
    >
      <div
        onMouseDown={onDragStart}
        title="Drag to resize"
        className="absolute left-0 top-0 z-20 h-full w-1 cursor-col-resize hover:bg-accent-primary/40"
      />
      <TabBar />
      <Header hit={selected} onClose={closeActiveTab} />
      <NoteCard hit={selected} />
      <SourceView hit={selected} />
    </aside>
  );
}

function Header({
  hit,
  onClose,
}: {
  hit: SearchHit | null;
  onClose: () => void;
}) {
  const editorProtocol = useUIPrefsStore((s) => s.editorProtocol);
  const inlineJavadocsEnabled = useUIPrefsStore(
    (s) => s.inlineJavadocsEnabled,
  );
  const setInlineJavadocsEnabled = useUIPrefsStore(
    (s) => s.setInlineJavadocsEnabled,
  );
  const viewerBack = useSearchStore((s) => s.viewerBack);
  const viewerForward = useSearchStore((s) => s.viewerForward);
  const url = hit ? editorUrl(editorProtocol, hit.path, hit.preview_line) : null;
  const showMethodCopy =
    !!hit && !!hit.symbol_name && hit.chunk_kind === "method";
  const showJavadocToggle = !!hit && hit.source_type === "source" && !!hit.fqn;

  async function copyFqn() {
    if (!hit?.fqn) return;
    try {
      await navigator.clipboard.writeText(hit.fqn);
      toast.success("Copied FQN");
    } catch {
      toast.error("Couldn't copy");
    }
  }

  async function copyMethodSig() {
    if (!hit || !showMethodCopy) return;
    const sig = `${hit.fqn}#${hit.symbol_name}`;
    try {
      await navigator.clipboard.writeText(sig);
      toast.success("Copied method signature");
    } catch {
      toast.error("Couldn't copy");
    }
  }

  return (
    <header
      className="flex shrink-0 items-center gap-2 border-b border-border-subtle px-3"
      style={{ height: "48px" }}
    >
      <div className="flex shrink-0 items-center gap-0.5">
        <IconBtn
          onClick={() => {
            viewerBack();
          }}
          label="Back (Alt+←)"
        >
          <ChevronLeft size={14} strokeWidth={1.75} />
        </IconBtn>
        <IconBtn
          onClick={() => {
            viewerForward();
          }}
          label="Forward (Alt+→)"
        >
          <ChevronRight size={14} strokeWidth={1.75} />
        </IconBtn>
      </div>
      <div className="min-w-0 flex-1">
        <p
          className="truncate font-mono text-sm text-fg-primary"
          title={hit?.fqn ?? ""}
        >
          {hit?.filename ?? "no file selected"}
        </p>
        {hit && (
          <p
            className="truncate font-mono text-[11px] text-fg-muted"
            title={hit.path}
          >
            {hit.package || "(default package)"}
          </p>
        )}
      </div>
      {hit && (
        <div className="flex shrink-0 items-center gap-0.5">
          {showJavadocToggle && (
            <button
              type="button"
              onClick={() =>
                setInlineJavadocsEnabled(!inlineJavadocsEnabled)
              }
              title={
                inlineJavadocsEnabled
                  ? "Hide inline Javadocs"
                  : "Show inline Javadocs"
              }
              aria-label="Toggle inline Javadocs"
              aria-pressed={inlineJavadocsEnabled}
              className={cn(
                "rounded-sm p-1 hover:bg-bg-elevated",
                inlineJavadocsEnabled
                  ? "text-section-javadocs"
                  : "text-fg-muted hover:text-fg-primary",
              )}
            >
              <BookOpen size={14} strokeWidth={1.75} />
            </button>
          )}
          <PinButton hit={hit} />
          <IconBtn onClick={copyFqn} label="Copy fully-qualified name">
            <Copy size={14} strokeWidth={1.75} />
          </IconBtn>
          {showMethodCopy && (
            <IconBtn onClick={copyMethodSig} label="Copy method signature">
              <Hash size={14} strokeWidth={1.75} />
            </IconBtn>
          )}
          {url && (
            <a
              href={url}
              title={editorLabel(editorProtocol)}
              className="rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
            >
              <ExternalLink size={14} strokeWidth={1.75} />
            </a>
          )}
        </div>
      )}
      <button
        type="button"
        onClick={onClose}
        aria-label="Close file viewer"
        className="shrink-0 rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
      >
        <X size={14} strokeWidth={1.75} />
      </button>
    </header>
  );
}

/** Toggles a `file` pin for the active hit. The pin's `build_id` is the
 *  hit's slot string ("release" or "pre-release") so the LeftNav can
 *  reconstruct a viewer hit when the user clicks a pinned row later.
 *  The button is intentionally a single icon: pinned vs unpinned is
 *  carried by the icon swap (Pin / PinOff), tooltip, and aria-pressed.
 */
function PinButton({ hit }: { hit: SearchHit }) {
  const findPin = usePinsStore((s) => s.findPin);
  const add = usePinsStore((s) => s.add);
  const remove = usePinsStore((s) => s.remove);
  // Subscribe to pins so the icon flips when the underlying list changes.
  usePinsStore((s) => s.pins);
  const existing = findPin("file", hit.path, hit.slot);
  const pinned = existing !== null;

  async function toggle() {
    if (pinned && existing) {
      await remove(existing.id);
      toast.success("Unpinned");
    } else {
      await add("file", hit.path, hit.slot, hit.filename);
      toast.success("Pinned");
    }
  }

  return (
    <button
      type="button"
      onClick={() => void toggle()}
      title={pinned ? "Unpin" : "Pin"}
      aria-label={pinned ? "Unpin file" : "Pin file"}
      aria-pressed={pinned}
      className={cn(
        "rounded-sm p-1 hover:bg-bg-elevated",
        pinned
          ? "text-accent-primary"
          : "text-fg-muted hover:text-fg-primary",
      )}
    >
      {pinned ? (
        <PinOff size={14} strokeWidth={1.75} />
      ) : (
        <Pin size={14} strokeWidth={1.75} />
      )}
    </button>
  );
}

/** Renders the user's note for the active file, with an inline editor.
 *  Hidden entirely when the file isn't pinned (you can't note an unpinned
 *  file - notes always hang off a pin row). When pinned with no note,
 *  shows a tiny "Add note" affordance. When pinned with a note, shows
 *  the body in a bordered card matching the InlineJavadocBox visual,
 *  with Edit / Delete actions. The editor is a plain textarea; save
 *  clears empty bodies (which deletes the note row backend-side). */
function NoteCard({ hit }: { hit: SearchHit | null }) {
  const findPin = usePinsStore((s) => s.findPin);
  const setNote = usePinsStore((s) => s.setNote);
  // Subscribe to pin list so the card flips between empty/filled when
  // pins or notes change elsewhere (e.g. unpinning from LeftNav).
  usePinsStore((s) => s.pins);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  const pin = hit ? findPin("file", hit.path, hit.slot) : null;

  // Reset editor state when the active pin changes so an old draft from
  // file A doesn't leak into file B.
  useEffect(() => {
    setEditing(false);
    setDraft(pin?.note ?? "");
  }, [pin?.id]);

  if (!hit || !pin) return null;

  async function save() {
    if (!pin) return;
    await setNote(pin.id, draft.trim());
    setEditing(false);
    toast.success(draft.trim() ? "Note saved" : "Note cleared");
  }

  function startEdit() {
    setDraft(pin?.note ?? "");
    setEditing(true);
  }

  if (editing) {
    return (
      <div className="border-b border-border-subtle bg-bg-base px-4 py-3">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          rows={4}
          autoFocus
          placeholder="Write a note for this file…"
          className={cn(
            "w-full resize-y rounded-md border border-border-subtle bg-bg-surface",
            "px-2 py-1.5 text-xs text-fg-primary placeholder:text-fg-muted",
            "focus:border-accent-primary focus:outline-none",
          )}
        />
        <div className="mt-2 flex justify-end gap-2 text-xs">
          <button
            type="button"
            onClick={() => setEditing(false)}
            className="rounded-md px-2 py-1 text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void save()}
            className="rounded-md bg-accent-primary px-2 py-1 text-accent-primary-fg hover:brightness-110"
          >
            Save
          </button>
        </div>
      </div>
    );
  }

  if (!pin.note) {
    return (
      <div className="border-b border-border-subtle bg-bg-base px-4 py-2">
        <button
          type="button"
          onClick={startEdit}
          className="flex items-center gap-1.5 text-xs text-fg-muted hover:text-fg-primary"
        >
          <StickyNote size={12} strokeWidth={1.75} />
          <span>Add note</span>
        </button>
      </div>
    );
  }

  return (
    <div className="border-b border-border-subtle bg-bg-base px-4 py-3">
      <div
        className={cn(
          "rounded-md border border-border-subtle bg-bg-surface px-3 py-2",
          "border-l-2 border-l-accent-primary",
        )}
      >
        <div className="flex items-start gap-2">
          <StickyNote
            size={12}
            strokeWidth={1.75}
            className="mt-0.5 shrink-0 text-accent-primary"
          />
          <p className="min-w-0 flex-1 whitespace-pre-wrap break-words text-xs text-fg-secondary">
            {pin.note}
          </p>
          <button
            type="button"
            onClick={startEdit}
            className="shrink-0 rounded-sm px-1.5 py-0.5 text-[11px] text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
          >
            Edit
          </button>
        </div>
      </div>
    </div>
  );
}

function IconBtn({
  onClick,
  label,
  children,
}: {
  onClick: () => void;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={label}
      aria-label={label}
      className="rounded-sm p-1 text-fg-muted hover:bg-bg-elevated hover:text-fg-primary"
    >
      {children}
    </button>
  );
}

function SourceView({ hit }: { hit: SearchHit | null }) {
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const previewRef = useRef<HTMLSpanElement>(null);
  const query = useSearchStore((s) => s.query);
  const inlineJavadocsEnabled = useUIPrefsStore(
    (s) => s.inlineJavadocsEnabled,
  );
  const activeTabId = useTabsStore((s) => s.activeId);
  const setTabScroll = useTabsStore((s) => s.setScroll);
  // Read the saved scroll for the *current* tab without subscribing to
  // every scrollByTabId change (we only need the value at restore time).
  const restoredRef = useRef(false);
  // Last preview_line we scrolled to inside the current tab. Lets us
  // detect "user clicked another row in the same file" and re-scroll.
  const lastPreviewLineRef = useRef<number | null>(null);

  // Inline Javadoc anchors for the active source hit. Backend pairs the
  // Hypixel Javadoc class page against `symbols.sqlite` so each entry
  // carries the real source-side `start_line`. Empty when no Javadoc is
  // cached for this class.
  const [javadocs, setJavadocs] = useState<InlineJavadoc[]>([]);

  const [findOpen, setFindOpen] = useState(false);
  const [findSeed, setFindSeed] = useState("");
  const [findCollapsed, setFindCollapsed] = useState(false);
  const [findMatchLines, setFindMatchLines] = useState<number[]>([]);
  const [contentVersion, setContentVersion] = useState(0);

  // Fetch the active hit's content.
  useEffect(() => {
    if (!hit) {
      setContent(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    cachedReadSource(hit.slot, hit.path, hit.source_type)
      .then((text) => {
        if (cancelled) return;
        setContent(text);
        setContentVersion((v) => v + 1);
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
  }, [hit?.slot, hit?.path]);

  // Pull the inline Javadoc anchor list for source hits. The backend
  // returns class-level prose (entry 0) plus one entry per documented
  // method, each tagged with the matching source line.
  useEffect(() => {
    setJavadocs([]);
    if (!hit || hit.source_type !== "source" || !hit.fqn) return;
    let cancelled = false;
    cachedGetInlineJavadocs(hit.slot, hit.fqn)
      .then((list) => {
        if (cancelled) return;
        setJavadocs(list);
      })
      .catch(() => {
        // Missing Javadoc is expected; not an error to surface.
      });
    return () => {
      cancelled = true;
    };
  }, [hit?.slot, hit?.fqn, hit?.source_type]);

  // When the active tab changes, mark scroll as un-restored so the next
  // content render restores from the saved position (or scrolls to the
  // preview line for first-time opens).
  useEffect(() => {
    restoredRef.current = false;
    lastPreviewLineRef.current = null;
  }, [activeTabId]);

  // Two responsibilities, in priority order:
  //   1. On the first render of a newly-active tab, restore that tab's
  //      saved scroll position (or fall back to the preview line).
  //   2. On every subsequent render where preview_line changes inside
  //      the same tab - i.e. the user clicked a different row in the
  //      same file - scroll to the new preview line. Without this the
  //      highlight band moves but the viewport doesn't follow.
  useEffect(() => {
    if (!content || !scrollRef.current) return;
    if (!restoredRef.current) {
      const saved = activeTabId
        ? useTabsStore.getState().scrollByTabId[activeTabId]
        : undefined;
      if (typeof saved === "number") {
        scrollRef.current.scrollTop = saved;
      } else if (hit?.preview_line && previewRef.current) {
        previewRef.current.scrollIntoView({ block: "center", behavior: "instant" });
      }
      restoredRef.current = true;
      lastPreviewLineRef.current = hit?.preview_line ?? null;
      return;
    }
    if (
      hit?.preview_line &&
      hit.preview_line !== lastPreviewLineRef.current &&
      previewRef.current
    ) {
      previewRef.current.scrollIntoView({ block: "center", behavior: "smooth" });
      lastPreviewLineRef.current = hit.preview_line;
    }
  }, [content, activeTabId, hit?.preview_line]);

  // Persist the active tab's scroll position as the user scrolls. We
  // wait until restore has run so the very first onScroll (triggered by
  // restoring scrollTop) doesn't clobber what we just set.
  function onScroll() {
    if (!restoredRef.current || !scrollRef.current || !activeTabId) return;
    setTabScroll(activeTabId, scrollRef.current.scrollTop);
  }

  // Track Find state across hit swaps. We pre-seed the Find query with
  // the active search so Ctrl+F lands with the input already populated,
  // but we do NOT auto-open the overlay: clicking a doc hit should just
  // show the doc, not pop a search bar.
  useEffect(() => {
    if (!hit || !content) {
      setFindOpen(false);
      return;
    }
    const isDoc =
      hit.source_type === "hm_doc" || hit.source_type === "hypixel_doc";
    if (!isDoc) {
      setFindOpen(false);
      setFindSeed("");
      return;
    }
    setFindSeed(useSearchStore.getState().query.trim());
  }, [hit?.path, hit?.source_type, content]);

  // Ctrl/Cmd+F opens the find overlay.
  useEffect(() => {
    if (!hit || !content) return;
    const onKey = (e: KeyboardEvent) => {
      const isFind =
        (e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F");
      if (!isFind) return;
      e.preventDefault();
      setFindOpen(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hit, content]);

  // Inline Javadoc anchors. Backend pairs each entry to a source line
  // via symbols.sqlite, so we just splice them in directly. The "class"
  // entry lands at line 1; each "method" entry lands above its method
  // declaration..
  const inlineAnchors: InlineAnchor[] = useMemo(() => {
    if (!inlineJavadocsEnabled || !content || javadocs.length === 0) {
      return [];
    }
    const fqn = hit?.fqn || "";
    return javadocs.map((j) => {
      let methodName: string | null = null;
      let signature: string | null = null;
      if (j.kind === "method") {
        // header: "SimpleClass.methodName(simpleParams)"
        const dot = j.header.indexOf(".");
        const paren = j.header.indexOf("(");
        if (dot >= 0 && paren > dot) {
          methodName = j.header.slice(dot + 1, paren);
          signature = j.header.slice(paren);
        } else {
          methodName = j.header;
        }
      }
      return {
        startLine: Math.max(1, j.start_line),
        node: (
          <InlineJavadocBox
            classFqn={fqn}
            methodName={methodName}
            signature={signature}
            body={(j.prose || "").trim()}
            deprecated={j.deprecated}
          />
        ),
      };
    });
  }, [inlineJavadocsEnabled, content, javadocs, hit?.fqn]);

  // Track find-match line numbers when collapse-to-context is active.
  // We compute against the raw content rather than walking DOM marks so
  // the line list stays in sync even before FindOverlay has wrapped its
  // mark spans.
  useEffect(() => {
    if (!findCollapsed || !findOpen || !content) {
      setFindMatchLines([]);
      return;
    }
    const needle = (findSeed || query).trim().toLowerCase();
    if (needle.length < 2) {
      setFindMatchLines([]);
      return;
    }
    const lines = content.split("\n");
    const out: number[] = [];
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].toLowerCase().includes(needle)) out.push(i + 1);
    }
    setFindMatchLines(out);
  }, [findCollapsed, findOpen, content, findSeed, query]);

  if (!hit) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 text-fg-muted">
        <span className="text-base font-medium text-fg-secondary">Page View</span>
        <span className="text-xs">Click a result to open it here</span>
      </div>
    );
  }

  if (loading && content === null) {
    return (
      <div className="flex flex-1 items-center justify-center text-fg-muted">
        <span className="text-sm">Loading…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-1 items-center justify-center px-6 text-center">
        <span className="font-mono text-xs text-destructive">{error}</span>
      </div>
    );
  }

  if (content === null) return null;

  const sectionVarName = sectionVar(hit.source_type);

  const find = (
    <FindOverlay
      open={findOpen}
      onOpenChange={(o) => {
        setFindOpen(o);
        if (!o) {
          setFindSeed("");
          setFindCollapsed(false);
        }
      }}
      containerRef={scrollRef}
      seedQuery={findSeed}
      contentVersion={contentVersion}
      collapseToContext={findCollapsed}
      onToggleCollapse={() => setFindCollapsed((v) => !v)}
      borderColorVar={sectionVarName}
    />
  );

  if (hit.source_type === "hm_doc") {
    return (
      <div className="relative flex min-h-0 flex-1 flex-col">
        {find}
        <div
          ref={scrollRef}
          onScroll={onScroll}
          className="min-h-0 flex-1 overflow-auto bg-bg-base"
        >
          <MarkdownView source={content} path={hit.path} />
        </div>
      </div>
    );
  }

  if (hit.source_type === "hypixel_doc") {
    return (
      <div className="relative flex min-h-0 flex-1 flex-col">
        {find}
        <div
          ref={scrollRef}
          onScroll={onScroll}
          className="min-h-0 flex-1 overflow-auto bg-bg-base px-5 py-4 text-[13px] leading-6 text-fg-secondary"
        >
          <div className="mx-auto max-w-[88ch] whitespace-pre-wrap break-words">
            {content}
          </div>
        </div>
      </div>
    );
  }

  // Source viewer: shiki-highlighted, with optional inline Javadoc
  // boxes above class/method declarations, and a "Javadoc inline"
  // toggle pill so users who want pure source can hide them.
  const visibleLines = computeVisibleLines(
    findCollapsed && findOpen,
    findMatchLines,
    content,
  );

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      {find}
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="min-h-0 flex-1 overflow-auto bg-bg-base"
      >
        <SourceCode
          content={content}
          language={langForPath(hit.path)}
          previewLine={hit.preview_line}
          previewRef={previewRef}
          inlineAnchors={inlineAnchors}
          previewColorVar={sectionVarName}
          visibleLines={visibleLines}
        />
      </div>
    </div>
  );
}

/** Build the visible-line whitelist for collapse-to-context: ±5 lines
 *  around every find-match. Returns undefined when collapse is off so
 *  SourceCode renders every line. */
function computeVisibleLines(
  active: boolean,
  matchLines: number[],
  content: string,
): Set<number> | undefined {
  if (!active || matchLines.length === 0) return undefined;
  const total = content.split("\n").length;
  const set = new Set<number>();
  for (const n of matchLines) {
    for (let i = n - 5; i <= n + 5; i++) {
      if (i >= 1 && i <= total) set.add(i);
    }
  }
  return set;
}
