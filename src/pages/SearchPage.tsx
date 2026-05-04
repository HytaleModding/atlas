import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Database, BookOpen, ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { hmDocUrl } from "@/components/MarkdownView";
import { cn } from "@/lib/utils";
import { useBranchStore } from "@/state/branchStore";
import { useIndexStore } from "@/state/indexStore";
import { useFetchStore } from "@/state/fetchStore";
import { useNavStore } from "@/state/navStore";
import { useSearchStore } from "@/state/searchStore";
import { useTabsStore } from "@/state/tabsStore";
import { useUIPrefsStore } from "@/state/uiPrefsStore";
import { useKeymap } from "@/lib/keymap";
import { slotLabel, type Slot } from "@/lib/patcher";
import {
  findSibling,
  findSourceSiblings,
  prefetchHit,
  type HitDebug,
  type SearchHit,
} from "@/lib/indexer";
import { SiblingPane } from "@/components/SiblingPane";
import { KeyboardCheatsheet } from "@/components/KeyboardCheatsheet";

/** Color key for the result list. Source / Guides / Javadocs are
 *  always-on and can't be filtered: Guides have their own lane,
 *  Javadoc text inlines into source results, and Source is the spine
 *  of every search. The legend is just visual reference for the row
 *  stripes - not a filter. Assets gets its own dedicated toggle below
 *  because once it's available it's a section you might want to mute. */
const LEGEND: { label: string; color: string }[] = [
  { label: "Source", color: "var(--section-source)" },
  { label: "Guides", color: "var(--section-guides)" },
  { label: "Javadocs", color: "var(--section-javadocs)" },
];

/** A grouped result row. `source` is the primary; `partner` is the
 *  Javadoc twin when one exists..
 *
 *  augments rows with `globalRank`, a 1-based ascending
 *  rank across the post-fusion result list so the FileGroup renderer
 *  can label each row with a `#3`-style chip and sort files by their
 *  best contained rank. */
type RowItem =
  | { kind: "single"; hit: SearchHit; globalRank: number }
  | {
      kind: "married";
      primary: SearchHit;
      partner: SearchHit;
      globalRank: number;
    };

/** Search page per ui-spec.md § Search Page. */
export function SearchPage() {
  const activeSlot = useBranchStore((s) => s.active);

  const overview = useIndexStore((s) => s.overview);
  const indexActiveSlot = useIndexStore((s) => s.activeSlot);
  const indexProgress = useIndexStore((s) => s.progress);

  const fetchStatus = useFetchStore((s) => s.status);
  const fetchActiveBuildId = useFetchStore((s) => s.activeBuildId);
  const fetchProgress = useFetchStore((s) => s.progress);

  const query = useSearchStore((s) => s.query);
  const setQuery = useSearchStore((s) => s.setQuery);
  const status = useSearchStore((s) => s.status);
  const hits = useSearchStore((s) => s.hits);
  const elapsedMs = useSearchStore((s) => s.elapsedMs);
  const searchError = useSearchStore((s) => s.error);
  const selected = useSearchStore((s) => s.selectedHit);
  const setSelected = useSearchStore((s) => s.setSelected);
  const resetSearch = useSearchStore((s) => s.reset);
  const hasMore = useSearchStore((s) => s.hasMore);
  const showMore = useSearchStore((s) => s.showMore);
  const history = useSearchStore((s) => s.history);

  const cheatsheetSeen = useUIPrefsStore((s) => s.cheatsheetSeen);
  const showDebug = useUIPrefsStore((s) => s.showDebug);

  const [cheatsheetOpen, setCheatsheetOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyIdx, setHistoryIdx] = useState(0);

  // Sibling pane: still useful for asset+source pairs, hidden when the
  // active source hit has its Javadoc rendered inline in the viewer.
  const [sibling, setSibling] = useState<SearchHit | null>(null);
  const [siblingCollapsed, setSiblingCollapsed] = useState(false);
  const inlineJavadocsEnabled = useUIPrefsStore(
    (s) => s.inlineJavadocsEnabled,
  );

  useEffect(() => {
    setSibling(null);
    setSiblingCollapsed(false);
    if (!selected) return;
    const slot = selected.slot;
    const fqn = selected.fqn;
    const sourceType = selected.source_type;
    if (!fqn) return;
    let cancelled = false;
    findSibling(slot, fqn, sourceType)
      .then((hit) => {
        if (cancelled) return;
        setSibling(hit);
      })
      .catch(() => {
        if (!cancelled) setSibling(null);
      });
    return () => {
      cancelled = true;
    };
  }, [selected]);

  // Hide the sibling pane when a source hit's Javadoc is already rendered
  // inline above the class declaration (Phase married view).
  const siblingHiddenByInline =
    inlineJavadocsEnabled &&
    !!selected &&
    selected.source_type === "source" &&
    !!sibling &&
    sibling.source_type === "hypixel_doc";

  const slotSummary =
    activeSlot === "release" ? overview?.release : overview?.pre_release;
  const searchable = !!slotSummary?.ready;
  const indexingThisSlot = indexActiveSlot === activeSlot;

  // Async pipeline: substitute Javadoc-only hits with their source
  // sibling rows, then run married fusion, then assign
  // a 1-based globalRank to every row by descending score.
  // While the substitution is in flight we render the unsubstituted
  // groupMarried output to avoid a blank flash.
  const baseRows = useMemo(
    () => assignRanks(dedupeWithinFile(groupMarried(hits))),
    [hits],
  );
  const [foldedRows, setFoldedRows] = useState<RowItem[] | null>(null);

  useEffect(() => {
    setFoldedRows(null);
    if (hits.length === 0) return;
    // Group by (slot, fqn) to find Javadoc-only FQNs.
    const sourceFqns = new Set<string>();
    for (const h of hits) {
      if (h.source_type === "source" && h.fqn) {
        sourceFqns.add(`${h.slot}:${h.fqn}`);
      }
    }
    const javadocOnly: { hit: SearchHit; key: string }[] = [];
    for (const h of hits) {
      if (h.source_type !== "hypixel_doc" || !h.fqn) continue;
      const key = `${h.slot}:${h.fqn}`;
      if (sourceFqns.has(key)) continue;
      javadocOnly.push({ hit: h, key });
    }
    if (javadocOnly.length === 0) {
      setFoldedRows(null);
      return;
    }
    // Bulk-resolve siblings, grouped by slot (we expect one slot per
    // search response in practice, but the API supports any).
    const bySlot = new Map<Slot, string[]>();
    for (const { hit } of javadocOnly) {
      const arr = bySlot.get(hit.slot);
      if (arr) {
        if (!arr.includes(hit.fqn)) arr.push(hit.fqn);
      } else {
        bySlot.set(hit.slot, [hit.fqn]);
      }
    }
    let cancelled = false;
    Promise.all(
      Array.from(bySlot.entries()).map(([slot, fqns]) =>
        findSourceSiblings(slot, fqns)
          .then((m) => ({ slot, map: m }))
          .catch(() => ({ slot, map: {} as Record<string, SearchHit | null> })),
      ),
    ).then((results) => {
      if (cancelled) return;
      const lookup = new Map<string, SearchHit | null>();
      for (const { slot, map } of results) {
        for (const [fqn, hit] of Object.entries(map)) {
          lookup.set(`${slot}:${fqn}`, hit);
        }
      }
      const rewritten: SearchHit[] = [];
      for (const h of hits) {
        if (h.source_type === "hypixel_doc" && h.fqn) {
          const key = `${h.slot}:${h.fqn}`;
          if (!sourceFqns.has(key)) {
            const sib = lookup.get(key);
            if (sib) {
              rewritten.push({
                ...sib,
                score: h.score,
                preview: h.preview,
                debug: h.debug,
                matchedIn: "javadoc",
              });
              continue;
            }
          }
        }
        rewritten.push(h);
      }
      setFoldedRows(assignRanks(dedupeWithinFile(groupMarried(rewritten))));
    });
    return () => {
      cancelled = true;
    };
  }, [hits]);

  const rows = foldedRows ?? baseRows;
  const fileCount = useMemo(() => {
    const set = new Set<string>();
    for (const r of rows) {
      const h = r.kind === "married" ? r.primary : r.hit;
      set.add(`${h.slot}:${h.path}`);
    }
    return set.size;
  }, [rows]);

  const dataLoading = fetchActiveBuildId !== null;

  useEffect(() => {
    resetSearch();
  }, [activeSlot, resetSearch]);

  // Build a flat hit list to drive j/k navigation: married rows expose
  // their primary as the navigable hit.
  const flatHits: SearchHit[] = useMemo(
    () => rows.map((r) => (r.kind === "married" ? r.primary : r.hit)),
    [rows],
  );

  function selectByIndex(idx: number) {
    if (idx < 0 || idx >= flatHits.length) return;
    setSelected(flatHits[idx]);
  }

  function currentIndex(): number {
    if (!selected) return -1;
    return flatHits.findIndex(
      (h) => h.slot === selected.slot && h.path === selected.path,
    );
  }

  // Warm the source + inline-Javadoc caches for the j/k neighbors of
  // the active hit so the next/previous viewer load is instant. The
  // helper dedupes against any in-flight read started by the viewer
  // itself, so this is purely additive: no extra round trip when the
  // user actually opens the same hit.
  useEffect(() => {
    if (!selected) return;
    const idx = flatHits.findIndex(
      (h) => h.slot === selected.slot && h.path === selected.path,
    );
    if (idx === -1) return;
    const prev = idx > 0 ? flatHits[idx - 1] : null;
    const next = idx < flatHits.length - 1 ? flatHits[idx + 1] : null;
    if (prev) prefetchHit(prev);
    if (next) prefetchHit(next);
  }, [selected, flatHits]);

  // Search-page keyboard bindings.
  useKeymap(
    [
      {
        key: "j",
        scope: "search-page",
        handler: () => selectByIndex(Math.max(0, currentIndex() + 1)),
      },
      {
        key: "ArrowDown",
        scope: "search-page",
        handler: () => selectByIndex(Math.max(0, currentIndex() + 1)),
      },
      {
        key: "k",
        scope: "search-page",
        handler: () => selectByIndex(Math.max(0, currentIndex() - 1)),
      },
      {
        key: "ArrowUp",
        scope: "search-page",
        handler: () => selectByIndex(Math.max(0, currentIndex() - 1)),
      },
      {
        key: "Escape",
        scope: "app",
        handler: () => inputRef.current?.focus(),
      },
      {
        // `/` jumps focus to the search input from anywhere on the
        // search page. `app` scope means it doesn't fire while the user
        // is already typing in an input, so the literal `/` character
        // still works inside text fields.
        key: "/",
        scope: "app",
        handler: () => inputRef.current?.focus(),
      },
      {
        key: "?",
        shift: true,
        scope: "global",
        handler: () => setCheatsheetOpen(true),
      },
    ],
    [flatHits, selected],
  );

  const openTab = useTabsStore((s) => s.openTab);

  function onSelectHit(hit: SearchHit) {
    // openTab also pipes to searchStore.setSelected; the tab strip and
    // viewer stay in lockstep without the callsite knowing.
    openTab(hit);
  }

  // Keep the global cheatsheet visible at least once; sets the
  // seen flag when the user opens it. We do not auto-pop it here.
  void cheatsheetSeen;

  return (
    <div className="flex h-full flex-col">
      <header
        className="flex shrink-0 flex-col justify-center gap-2 border-b border-border-subtle bg-bg-base px-6"
        style={{ height: "80px" }}
      >
        <div className="relative">
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value, activeSlot);
              setHistoryOpen(false);
            }}
            onFocus={() => {
              if (query.length === 0 && history.length > 0) {
                // Don't auto-open; user opens via ArrowUp.
              }
            }}
            onKeyDown={(e) => {
              if (
                e.key === "ArrowUp" &&
                query.length === 0 &&
                history.length > 0
              ) {
                e.preventDefault();
                setHistoryOpen(true);
                setHistoryIdx(0);
              } else if (historyOpen) {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setHistoryIdx((i) => Math.min(history.length - 1, i + 1));
                } else if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setHistoryIdx((i) => Math.max(0, i - 1));
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  const pick = history[historyIdx];
                  if (pick) {
                    setQuery(pick, activeSlot);
                    setHistoryOpen(false);
                  }
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  setHistoryOpen(false);
                }
              }
            }}
            disabled={!searchable}
            placeholder={
              searchable
                ? "Search Hytale source, guides, and Javadocs…"
                : indexingThisSlot
                  ? "Setting up…"
                  : `No Hytale data loaded for ${slotLabel(activeSlot)} yet`
            }
            className={cn(
              "w-full rounded-md border border-border-subtle bg-bg-surface",
              "px-3 py-2 font-mono text-base text-fg-primary placeholder:text-fg-muted",
              "focus:border-accent-primary focus:outline-none",
              !searchable && "cursor-not-allowed opacity-60",
            )}
          />
          {historyOpen && history.length > 0 && (
            <ul
              className="absolute left-0 right-0 top-full z-20 mt-1 max-h-64 overflow-auto rounded-md border border-border-subtle bg-bg-surface shadow-lg"
              role="listbox"
            >
              {history.map((q, i) => (
                <li key={q}>
                  <button
                    type="button"
                    onMouseDown={(e) => {
                      e.preventDefault();
                      setQuery(q, activeSlot);
                      setHistoryOpen(false);
                    }}
                    onMouseEnter={() => setHistoryIdx(i)}
                    className={cn(
                      "block w-full px-3 py-1.5 text-left font-mono text-sm",
                      i === historyIdx
                        ? "bg-bg-elevated text-fg-primary"
                        : "text-fg-secondary",
                    )}
                  >
                    {q}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {LEGEND.map((entry) => (
              <span
                key={entry.label}
                className="flex items-center gap-1.5 text-xs text-fg-muted"
                title={entry.label}
              >
                <span
                  aria-hidden
                  className="inline-block h-2 w-2 rounded-full"
                  style={{ backgroundColor: entry.color }}
                />
                {entry.label}
              </span>
            ))}
            <button
              type="button"
              disabled
              title="Assets are not yet available"
              className={cn(
                "flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs",
                "cursor-not-allowed bg-bg-surface text-fg-secondary opacity-50",
              )}
            >
              <span
                aria-hidden
                className="inline-block h-2 w-2 rounded-full"
                style={{ backgroundColor: "var(--section-assets)" }}
              />
              Assets: off
            </button>
          </div>
          <span className="text-xs text-fg-muted">
            {searchable && elapsedMs !== null && status === "ready"
              ? `${hits.length} match${hits.length === 1 ? "" : "es"} in ${fileCount} file${fileCount === 1 ? "" : "s"} · ${elapsedMs}ms`
              : "keyword"}
          </span>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-hidden">
        {!searchable ? (
          <EmptyState
            activeSlot={activeSlot}
            indexing={indexingThisSlot}
            indexProgress={indexProgress}
            dataLoading={dataLoading}
            fetchProgress={fetchProgress}
            fetchStatusKind={fetchStatus.kind}
          />
        ) : query.trim().length === 0 ? (
          <IdlePrompt />
        ) : status === "pending" && hits.length === 0 ? (
          <CenterNote>Searching…</CenterNote>
        ) : status === "ready" && hits.length === 0 ? (
          <NoResultsState />
        ) : status === "error" ? (
          <CenterNote>
            <span className="flex flex-col items-center gap-1 font-mono text-xs text-destructive">
              <span>Search failed.</span>
              {searchError && (
                <span className="max-w-lg whitespace-pre-wrap break-all text-fg-muted">
                  {searchError}
                </span>
              )}
            </span>
          </CenterNote>
        ) : (
          <Results
            rows={rows}
            selected={selected}
            onSelect={onSelectHit}
            hasMore={hasMore}
            loadingMore={status === "pending"}
            onShowMore={showMore}
            showDebug={showDebug}
          />
        )}
      </div>
      {sibling && !siblingCollapsed && !siblingHiddenByInline && (
        <SiblingPane
          sibling={sibling}
          onSwap={() => {
            openTab(sibling);
          }}
          onCollapse={() => setSiblingCollapsed(true)}
        />
      )}
      <KeyboardCheatsheet
        open={cheatsheetOpen}
        onClose={() => setCheatsheetOpen(false)}
      />
    </div>
  );
}

function EmptyState({
  activeSlot,
  indexing,
  indexProgress,
  dataLoading,
  fetchProgress,
  fetchStatusKind,
}: {
  activeSlot: Slot;
  indexing: boolean;
  indexProgress: { phase: string | null; current: number; total: number };
  dataLoading: boolean;
  fetchProgress: ReturnType<typeof useFetchStore.getState>["progress"];
  fetchStatusKind: ReturnType<typeof useFetchStore.getState>["status"]["kind"];
}) {
  const setPage = useNavStore((s) => s.setPage);
  if (
    dataLoading &&
    fetchStatusKind !== "idle" &&
    fetchStatusKind !== "done"
  ) {
    const total = fetchProgress.total ?? 0;
    const pct =
      total > 0 ? Math.min(100, (fetchProgress.current / total) * 100) : 0;
    const label = friendlyEmptyPhaseLabel(fetchProgress.phase);
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
        <span className="text-sm text-fg-secondary">{label}</span>
        <div className="h-1.5 w-56 overflow-hidden rounded-full bg-bg-elevated">
          <div
            className="h-full bg-accent-primary transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="font-mono text-xs text-fg-muted">
          {total > 0
            ? `${formatBytes(fetchProgress.current)} / ${formatBytes(total)}`
            : formatBytes(fetchProgress.current)}
        </span>
      </div>
    );
  }

  if (indexing) {
    const pct =
      indexProgress.total > 0
        ? Math.min(100, (indexProgress.current / indexProgress.total) * 100)
        : 0;
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
        <span className="text-sm text-fg-secondary">Setting up search…</span>
        <div className="h-1.5 w-56 overflow-hidden rounded-full bg-bg-elevated">
          <div
            className="h-full bg-accent-primary transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="font-mono text-xs text-fg-muted">
          {indexProgress.total > 0
            ? `${indexProgress.current.toLocaleString()} / ${indexProgress.total.toLocaleString()} files`
            : indexProgress.phase ?? ""}
        </span>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      <Database size={32} strokeWidth={1.5} className="text-fg-muted" />
      <span className="text-sm font-medium text-fg-primary">
        Atlas doesn&apos;t have Hytale {slotLabel(activeSlot).toLowerCase()}{" "}
        data yet.
      </span>
      <span className="max-w-md text-xs text-fg-muted">
        Atlas will download Hytale&apos;s source code, modding guides, and
        API docs automatically. Hang tight, this is on the way.
      </span>
      <button
        type="button"
        onClick={() => setPage("settings")}
        className="mt-2 rounded-md bg-accent-primary px-3 py-1.5 text-xs font-medium text-accent-primary-fg hover:brightness-110"
      >
        Set up Hytale install
      </button>
    </div>
  );
}

function NoResultsState() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
      <span className="text-sm text-fg-secondary">No matches.</span>
      <span className="text-xs text-fg-muted">
        Try a different word or rephrase the query.
      </span>
    </div>
  );
}

function friendlyEmptyPhaseLabel(phase: string | null): string {
  switch (phase) {
    case "resolving":
      return "Looking up Hytale data…";
    case "downloading":
      return "Downloading…";
    case "verifying":
      return "Checking integrity…";
    case "extracting":
      return "Setting up…";
    case "mounting":
      return "Finishing up…";
    default:
      return "Getting ready…";
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

const IDLE_EXAMPLES: string[] = [
  "PageManager",
  "getComponent",
  "ItemStack",
  "World",
  "Entity",
  "Inventory",
  "BlockPos",
  "ServerPlayer",
  "Recipe",
  "DamageSource",
  "spawn entity",
  "scheduled task",
];

function pickIdleExamples(): string[] {
  const pool = [...IDLE_EXAMPLES];
  const out: string[] = [];
  for (let i = 0; i < 3 && pool.length > 0; i++) {
    const idx = Math.floor(Math.random() * pool.length);
    out.push(pool.splice(idx, 1)[0]);
  }
  return out;
}

function IdlePrompt() {
  const examples = useMemo(pickIdleExamples, []);
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
      <span className="text-lg text-fg-secondary">
        Search the Hytale codebase
      </span>
      <span className="text-xs text-fg-muted">
        Tip: try{" "}
        {examples.map((e, i) => (
          <span key={e}>
            <span className="font-mono">{e}</span>
            {i < examples.length - 1 ? ", " : ""}
          </span>
        ))}
      </span>
      <span className="text-[11px] text-fg-muted">
        Press <kbd className="rounded border border-border-subtle bg-bg-elevated px-1 py-0.5 font-mono text-[10px]">?</kbd>{" "}
        for keyboard shortcuts
      </span>
    </div>
  );
}

function CenterNote({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-full items-center justify-center px-6 text-sm text-fg-muted">
      {children}
    </div>
  );
}

function Results({
  rows,
  selected,
  onSelect,
  hasMore,
  loadingMore,
  onShowMore,
  showDebug,
}: {
  rows: RowItem[];
  selected: SearchHit | null;
  onSelect: (hit: SearchHit) => void;
  hasMore: boolean;
  loadingMore: boolean;
  onShowMore: () => void;
  showDebug: boolean;
}) {
  // Split results into a code/javadoc lane (top) and an HM-guides lane
  // (bottom). When both have content, the guides take a dedicated 50%
  // of the viewport so they're never buried under code hits. When only
  // one kind of hit exists, that lane fills the area as before.
  const { codeRows, guideRows } = useMemo(() => {
    const code: RowItem[] = [];
    const guide: RowItem[] = [];
    for (const r of rows) {
      const h = r.kind === "married" ? r.primary : r.hit;
      if (h.source_type === "hm_doc") guide.push(r);
      else code.push(r);
    }
    return { codeRows: code, guideRows: guide };
  }, [rows]);

  const codeGroups = useMemo(() => groupByFile(codeRows), [codeRows]);
  // Guides render as a flat list of clickable cards (one per doc), so
  // we dedupe rows that point at the same file and keep best-rank order.
  //
  // Two-stage filter to suppress low-correlation noise (common funnel
  // docs like "How to modify Hytale?" or generic tutorials matching one
  // weak term):
  //   1. Absolute BM25 floor - Tantivy scores below ~1.5 are usually
  //      single-keyword glances rather than topical matches.
  //   2. Relative floor - keep only cards within 65% of the top hit's
  //      score, so weak matches don't ride the coattails of a strong
  //      one. With a high top score the gap to noise is wide; with a
  //      low top score nothing relevant exists anyway.
  const guideCards = useMemo(() => {
    const seen = new Set<string>();
    const all: { hit: SearchHit; rank: number }[] = [];
    for (const r of guideRows) {
      const h = r.kind === "married" ? r.primary : r.hit;
      const key = `${h.slot}:${h.path}`;
      if (seen.has(key)) continue;
      seen.add(key);
      all.push({ hit: h, rank: r.globalRank });
    }
    if (all.length === 0) return all;
    const ABSOLUTE_FLOOR = 1.5;
    const RELATIVE_FRACTION = 0.65;
    const topScore = all.reduce((m, c) => Math.max(m, c.hit.score), 0);
    const floor = Math.max(ABSOLUTE_FLOOR, topScore * RELATIVE_FRACTION);
    return all.filter((c) => c.hit.score >= floor);
  }, [guideRows]);
  // Cap displayed cards. "Show more" expands to the full filtered list.
  const GUIDES_PAGE_SIZE = 10;
  const [guidesExpanded, setGuidesExpanded] = useState(false);
  // Reset the expand state whenever the underlying card list changes
  // (new query, new section filter) so the user re-affirms intent.
  useEffect(() => {
    setGuidesExpanded(false);
  }, [guideCards]);
  const visibleGuideCards = guidesExpanded
    ? guideCards
    : guideCards.slice(0, GUIDES_PAGE_SIZE);
  const guidesHasMore = guideCards.length > visibleGuideCards.length;

  const showMore = hasMore ? (
    <div className="flex justify-center py-4">
      <button
        type="button"
        onClick={onShowMore}
        disabled={loadingMore}
        className={cn(
          "rounded-full border border-border-subtle bg-bg-surface px-4 py-1.5",
          "text-xs text-fg-secondary transition-colors",
          "hover:border-accent-primary hover:text-fg-primary",
          loadingMore && "cursor-not-allowed opacity-60",
        )}
      >
        {loadingMore ? "Loading…" : "Show more"}
      </button>
    </div>
  ) : null;

  const codeLane = (
    <ResultLane
      groups={codeGroups}
      selected={selected}
      onSelect={onSelect}
      showDebug={showDebug}
      footer={showMore}
    />
  );

  const guideLane = (
    <GuideCardLane
      cards={visibleGuideCards}
      selected={selected}
      onSelect={onSelect}
      hasMore={guidesHasMore}
      onShowMore={() => setGuidesExpanded(true)}
      hiddenCount={guideCards.length - visibleGuideCards.length}
    />
  );

  // Single lane: only one of code/guides has content. Fill the area.
  if (codeRows.length === 0) {
    return (
      <div className="flex h-full flex-col">
        <GuideLaneHeader count={guideCards.length} />
        <div className="min-h-0 flex-1 overflow-auto">{guideLane}</div>
      </div>
    );
  }
  if (guideCards.length === 0) {
    // Even with zero matched guides, keep the lane header pinned at
    // the bottom so the "Browse all docs" link is always reachable.
    return (
      <div className="flex h-full flex-col">
        <div className="min-h-0 flex-1 overflow-auto">{codeLane}</div>
        <GuideLaneHeader count={0} />
      </div>
    );
  }

  // Split layout: code lane on top, guides on the bottom 50%. Each
  // lane scrolls independently so a chatty guides section doesn't push
  // code rows out of view.
  return (
    <div className="flex h-full flex-col">
      <div className="min-h-0 flex-1 overflow-auto">{codeLane}</div>
      <div
        className="flex min-h-0 shrink-0 flex-col border-t border-border-subtle bg-bg-base"
        style={{ height: "50%" }}
      >
        <GuideLaneHeader count={guideCards.length} />
        <div className="min-h-0 flex-1 overflow-auto">{guideLane}</div>
      </div>
    </div>
  );
}

/** Flat list of guide cards. Each card is just the guide title - no
 *  query-snippet preview. Clicking opens the guide in the viewer. */
function GuideCardLane({
  cards,
  selected,
  onSelect,
  hasMore,
  onShowMore,
  hiddenCount,
}: {
  cards: { hit: SearchHit; rank: number }[];
  selected: SearchHit | null;
  onSelect: (hit: SearchHit) => void;
  hasMore: boolean;
  onShowMore: () => void;
  hiddenCount: number;
}) {
  if (cards.length === 0) return null;
  const stripeColor = "var(--section-guides)";
  return (
    <>
    <ul className="flex flex-col gap-2 px-4 py-3">
      {cards.map(({ hit }) => {
        const active =
          !!selected &&
          selected.slot === hit.slot &&
          selected.path === hit.path;
        const title =
          hit.filename ||
          hit.path.split(/[\\/]/).filter(Boolean).pop() ||
          hit.path;
        const webUrl = hmDocUrl(hit.path);
        return (
          <li key={`${hit.slot}:${hit.path}`}>
            <div
              className={cn(
                "flex w-full items-stretch gap-3 rounded-md border bg-bg-surface",
                "transition-colors hover:bg-bg-elevated",
                active ? "border-accent-primary" : "border-border-subtle",
              )}
            >
              <button
                type="button"
                onClick={() => onSelect(hit)}
                className="flex min-w-0 flex-1 items-center gap-3 px-3 py-2 text-left"
              >
                <span
                  aria-hidden
                  className="h-6 w-1 shrink-0 rounded-full"
                  style={{ background: stripeColor }}
                />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate text-sm text-fg-primary">
                    {title}
                  </span>
                  {hit.authors ? (
                    <span className="truncate text-xs text-fg-muted">
                      by {hit.authors}
                    </span>
                  ) : null}
                </span>
              </button>
              {webUrl ? (
                <a
                  href={webUrl}
                  onClick={(e) => {
                    // Tauri webviews block target="_blank"; route through
                    // the opener plugin so the OS browser actually opens.
                    e.preventDefault();
                    e.stopPropagation();
                    void openUrl(webUrl);
                  }}
                  className="m-2 inline-flex shrink-0 items-center gap-1.5 self-center rounded border border-border-subtle bg-bg-elevated px-2.5 py-1 text-xs text-fg-secondary hover:border-accent-secondary hover:text-accent-secondary"
                >
                  View on hytalemodding.dev
                  <ExternalLink size={12} strokeWidth={1.75} />
                </a>
              ) : null}
            </div>
          </li>
        );
      })}
    </ul>
    {hasMore && (
      <div className="flex justify-center pb-3">
        <button
          type="button"
          onClick={onShowMore}
          className={cn(
            "rounded-full border border-border-subtle bg-bg-surface px-4 py-1.5",
            "text-xs text-fg-secondary transition-colors",
            "hover:border-accent-primary hover:text-fg-primary",
          )}
        >
          Show {hiddenCount} more
        </button>
      </div>
    )}
    </>
  );
}

function ResultLane({
  groups,
  selected,
  onSelect,
  showDebug,
  footer,
}: {
  groups: RowItem[][];
  selected: SearchHit | null;
  onSelect: (hit: SearchHit) => void;
  showDebug: boolean;
  footer?: ReactNode;
}) {
  return (
    <div className="flex flex-col">
      <ul className="divide-y divide-border-subtle">
        {groups.map((group) => (
          <FileGroup
            key={fileKeyOf(group[0])}
            rows={group}
            selected={selected}
            onSelect={onSelect}
            showDebug={showDebug}
          />
        ))}
      </ul>
      {footer}
    </div>
  );
}

function GuideLaneHeader({ count }: { count?: number }) {
  const docsHome = "https://hytalemodding.dev/docs";
  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-border-subtle bg-bg-surface/95 px-4 py-2 backdrop-blur">
      <BookOpen size={14} className="text-fg-secondary" />
      <span className="text-xs font-medium text-fg-primary">
        HytaleModding Documentation
      </span>
      {typeof count === "number" && (
        <span className="font-mono text-[10px] text-fg-muted">
          {count} match{count === 1 ? "" : "es"}
        </span>
      )}
      {/* Always-on guidepost to the upstream docs site. Helps users
       *  discover hytalemodding.dev even when the lane is empty or
       *  their query didn't match any indexed pages. */}
      <a
        href={docsHome}
        onClick={(e) => {
          e.preventDefault();
          void openUrl(docsHome);
        }}
        className="ml-auto inline-flex items-center gap-1.5 rounded border border-border-subtle bg-bg-elevated px-2.5 py-1 text-xs text-fg-secondary hover:border-accent-secondary hover:text-accent-secondary"
      >
        Browse all docs
        <ExternalLink size={12} strokeWidth={1.75} />
      </a>
    </div>
  );
}

function fileKeyOf(r: RowItem): string {
  const h = r.kind === "married" ? r.primary : r.hit;
  return `${h.slot}:${h.path}`;
}

/** Cluster rows by their primary hit's file, sort within each file by
 *  ascending globalRank, and order files by their best (lowest) rank.
 * : rank-driven layout instead of score-driven. */
function groupByFile(rows: RowItem[]): RowItem[][] {
  const map = new Map<string, RowItem[]>();
  for (const r of rows) {
    const key = fileKeyOf(r);
    const arr = map.get(key);
    if (arr) arr.push(r);
    else map.set(key, [r]);
  }
  for (const arr of map.values()) {
    arr.sort((a, b) => a.globalRank - b.globalRank);
  }
  return Array.from(map.values()).sort(
    (a, b) => a[0].globalRank - b[0].globalRank,
  );
}

function scoreOf(r: RowItem): number {
  return r.kind === "married"
    ? Math.max(r.primary.score, r.partner.score)
    : r.hit.score;
}

/** Assigns a 1-based globalRank by sorting rows in descending score
 *  order, then re-emits them in their original order with the rank
 *  attached. Stable: ties are resolved by original position. */
function assignRanks(rows: RowItem[]): RowItem[] {
  const indexed = rows.map((r, i) => ({ r, i, s: scoreOf(r) }));
  const sorted = indexed.slice().sort((a, b) => {
    if (b.s !== a.s) return b.s - a.s;
    return a.i - b.i;
  });
  const rankByIndex = new Map<number, number>();
  sorted.forEach((entry, idx) => rankByIndex.set(entry.i, idx + 1));
  return rows.map((r, i) => ({ ...r, globalRank: rankByIndex.get(i) ?? i + 1 }));
}

/** Within each file, drop near-duplicate rows so the result list isn't
 *  swamped by structurally-redundant chunks. Two rules:
 *
 *  1. If a file has any non-`type` chunks (methods / constructors /
 *     fields), drop its `type` chunk: the whole-class match is
 *     subsumed by the more specific ones.
 *  2. Within a file, collapse rows that share `(chunk_kind,
 *     symbol_name)` to the highest-scoring one. This kills the common
 *     case of two `constructor` chunks for the same symbol at adjacent
 *     line ranges, while leaving genuinely distinct methods (different
 *     names) intact.
 *
 *  Rows without a `symbol_name` are passed through untouched. */
function dedupeWithinFile(rows: RowItem[]): RowItem[] {
  const byPath = new Map<string, RowItem[]>();
  const order: string[] = [];
  for (const r of rows) {
    const key = fileKeyOf(r);
    if (!byPath.has(key)) {
      byPath.set(key, []);
      order.push(key);
    }
    byPath.get(key)!.push(r);
  }
  const out: RowItem[] = [];
  for (const key of order) {
    const arr = byPath.get(key)!;
    const hasNonType = arr.some((r) => {
      const h = r.kind === "married" ? r.primary : r.hit;
      return h.chunk_kind && h.chunk_kind !== "type";
    });
    const filtered = hasNonType
      ? arr.filter((r) => {
          const h = r.kind === "married" ? r.primary : r.hit;
          return h.chunk_kind !== "type";
        })
      : arr;

    const bestByKey = new Map<string, RowItem>();
    const winners = new Set<RowItem>();
    for (const r of filtered) {
      const h = r.kind === "married" ? r.primary : r.hit;
      if (!h.symbol_name) {
        winners.add(r);
        continue;
      }
      const dedupKey = `${h.chunk_kind || ""}|${h.symbol_name.toLowerCase()}`;
      const existing = bestByKey.get(dedupKey);
      if (!existing || scoreOf(r) > scoreOf(existing)) {
        bestByKey.set(dedupKey, r);
      }
    }
    for (const r of bestByKey.values()) winners.add(r);
    for (const r of filtered) {
      if (winners.has(r)) out.push(r);
    }
  }
  return out;
}

/** Married hits client-side fusion: group by (slot, fqn). When a group
 *  contains both source and javadoc hits, fuse the highest-scoring
 *  source with the highest-scoring javadoc.. */
function groupMarried(hits: SearchHit[]): RowItem[] {
  const buckets = new Map<string, SearchHit[]>();
  const orderKeys: string[] = [];
  for (const h of hits) {
    const fqn = h.fqn || `${h.path}#${h.start_line ?? 0}`;
    const key = `${h.slot}:${fqn}`;
    if (!buckets.has(key)) {
      buckets.set(key, []);
      orderKeys.push(key);
    }
    buckets.get(key)!.push(h);
  }
  const out: RowItem[] = [];
  for (const key of orderKeys) {
    const bucket = buckets.get(key)!;
    const source = bestOfType(bucket, "source");
    const javadoc = bestOfType(bucket, "hypixel_doc");
    if (source && javadoc) {
      const primary = source.score >= javadoc.score ? source : javadoc;
      const partner = primary === source ? javadoc : source;
      out.push({ kind: "married", primary, partner, globalRank: 0 });
      const consumed = new Set([source, javadoc]);
      for (const h of bucket) {
        if (!consumed.has(h)) out.push({ kind: "single", hit: h, globalRank: 0 });
      }
    } else {
      for (const h of bucket) out.push({ kind: "single", hit: h, globalRank: 0 });
    }
  }
  return out;
}

function bestOfType(hits: SearchHit[], type: string): SearchHit | null {
  let best: SearchHit | null = null;
  for (const h of hits) {
    if (h.source_type !== type) continue;
    if (!best || h.score > best.score) best = h;
  }
  return best;
}

/** Renders a file group with no collapse: every contained row is its
 *  own clickable nav target, plus a sticky file-header row that opens
 *  the source viewer at line 1. Rows sort by ascending globalRank.
 * . */
function FileGroup({
  rows,
  selected,
  onSelect,
  showDebug,
}: {
  rows: RowItem[];
  selected: SearchHit | null;
  onSelect: (hit: SearchHit) => void;
  showDebug: boolean;
}) {
  const anchor = rows[0];
  const anchorHit = anchor.kind === "married" ? anchor.primary : anchor.hit;
  const partner = anchor.kind === "married" ? anchor.partner : null;
  const meta = sectionMeta(anchorHit.source_type);
  const partnerMeta = partner ? sectionMeta(partner.source_type) : null;

  // The header opens the file at line 1 (top-of-file nav target).
  const headerHit: SearchHit = useMemo(
    () => ({
      ...anchorHit,
      preview_line: 1,
      start_line: anchorHit.start_line ?? 1,
      end_line: anchorHit.end_line ?? 1,
    }),
    [anchorHit],
  );
  const headerActive =
    !!selected &&
    selected.slot === headerHit.slot &&
    selected.path === headerHit.path &&
    selected.preview_line === 1;

  return (
    <li className="flex items-stretch">
      <span
        aria-hidden
        className="w-1 shrink-0"
        style={{
          background: partnerMeta
            ? `linear-gradient(to bottom, ${meta.color} 50%, ${partnerMeta.color} 50%)`
            : meta.color,
        }}
      />
      <div className="flex min-w-0 flex-1 flex-col">
      <button
        type="button"
        onClick={() => onSelect(headerHit)}
        title="Open this file at the top"
        className={cn(
          "sticky top-0 z-10 flex w-full items-baseline justify-between gap-3 px-6 py-2 text-left",
          "bg-bg-base/95 backdrop-blur transition-colors hover:bg-bg-elevated",
          headerActive && "bg-bg-elevated",
        )}
      >
        <div className="flex min-w-0 items-baseline gap-2">
          <SectionBadge meta={meta} />
          {partnerMeta && <SectionBadge meta={partnerMeta} />}
          <span
            className="truncate font-mono text-sm text-fg-primary"
            title={anchorHit.fqn}
          >
            {anchorHit.filename ||
              anchorHit.fqn.split(".").pop() ||
              anchorHit.fqn}
          </span>
          <span
            className="truncate font-mono text-[11px] text-fg-muted"
            title={anchorHit.fqn}
          >
            {anchorHit.package || "(default package)"}
          </span>
        </div>
        {rows.length > 1 && (
          <span className="shrink-0 font-mono text-[10px] tabular-nums text-fg-muted">
            {rows.length} matches
          </span>
        )}
      </button>

      {rows.map((row) => {
        const rowHit = row.kind === "married" ? row.primary : row.hit;
        const rowPartner = row.kind === "married" ? row.partner : null;
        const rowActive = isSameHit(selected, rowHit);
        const rowLabel =
          chunkSymbolLabel(rowHit) ||
          `lines ${rowHit.start_line ?? "?"}-${rowHit.end_line ?? "?"}`;
        return (
          <button
            key={`${rowHit.start_line ?? 0}-${rowHit.end_line ?? 0}-${row.globalRank}`}
            type="button"
            onClick={() => onSelect(rowHit)}
            className={cn(
              "flex w-full items-baseline justify-between gap-3 py-1.5 pl-11 pr-6 text-left",
              "transition-colors hover:bg-bg-elevated",
              rowActive && "bg-bg-elevated",
            )}
          >
            <div className="flex min-w-0 items-baseline gap-2">
              {rowHit.matchedIn === "javadoc" && <ViaDocsChip />}
              {rowPartner && <SectionBadge meta={sectionMeta(rowPartner.source_type)} />}
              <span
                className="truncate font-mono text-[11px] text-fg-secondary"
                title={rowLabel}
              >
                {rowLabel}
              </span>
            </div>
            <span className="shrink-0 font-mono text-[11px] text-fg-muted">
              {lineRangeOf(rowHit)}
              {showDebug && ` · ${rowHit.score.toFixed(2)}`}
            </span>
            {showDebug && rowHit.debug && <DebugRow debug={rowHit.debug} />}
          </button>
        );
      })}
      </div>
    </li>
  );
}

/** Marker on a row whose match came from Javadoc prose, even though
 *  we're showing the source file.. */
function ViaDocsChip() {
  return (
    <span
      className="shrink-0 rounded-sm px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide"
      style={{
        backgroundColor:
          "color-mix(in srgb, var(--section-javadocs) 18%, transparent)",
        color: "var(--section-javadocs)",
      }}
      title="Match scored against Javadoc prose for this class"
    >
      via docs
    </span>
  );
}

function DebugRow({ debug }: { debug: HitDebug }) {
  const fmt = (n: number | null, digits = 3) =>
    n === null ? "·" : n.toFixed(digits);
  const rankFmt = (n: number | null) => (n === null ? "·" : `#${n + 1}`);
  return (
    <span className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-[10px] text-fg-muted">
      <span>
        BM25 {rankFmt(debug.bm25_rank)} · {fmt(debug.bm25_score, 2)}
      </span>
      <span>
        vec {rankFmt(debug.vector_rank)} · d=
        {fmt(debug.vector_distance, 3)}
      </span>
      <span>
        RRF {fmt(debug.rrf_score, 4)} (w {debug.weight_bm25.toFixed(1)}/
        {debug.weight_vector.toFixed(1)})
      </span>
    </span>
  );
}

function isSameHit(a: SearchHit | null, b: SearchHit): boolean {
  return (
    !!a &&
    a.slot === b.slot &&
    a.path === b.path &&
    a.start_line === b.start_line &&
    a.end_line === b.end_line
  );
}

function lineRangeOf(hit: SearchHit): string {
  return hit.start_line !== null && hit.end_line !== null
    ? `L${hit.start_line}-L${hit.end_line}`
    : `${hit.line_count.toLocaleString()} lines`;
}

type SectionMeta = { label: string; color: string };

function sectionMeta(sourceType: string): SectionMeta {
  switch (sourceType) {
    case "hm_doc":
      return { label: "Guides", color: "var(--section-guides)" };
    case "hypixel_doc":
      return { label: "Javadocs", color: "var(--section-javadocs)" };
    case "asset":
      return { label: "Assets", color: "var(--section-assets)" };
    case "source":
    case "":
    default:
      return { label: "Source", color: "var(--section-source)" };
  }
}

function SectionBadge({ meta }: { meta: SectionMeta }) {
  return (
    <span
      className="shrink-0 rounded-sm px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide"
      style={{
        backgroundColor: `color-mix(in srgb, ${meta.color} 20%, transparent)`,
        color: meta.color,
      }}
    >
      {meta.label}
    </span>
  );
}

function chunkSymbolLabel(hit: SearchHit): string {
  if (!hit.symbol_name) return "";
  switch (hit.chunk_kind) {
    case "type":
      return `class ${hit.symbol_name}`;
    case "method":
      return `method ${hit.symbol_name}`;
    case "constructor":
      return `constructor ${hit.symbol_name}`;
    default:
      return hit.symbol_name;
  }
}
