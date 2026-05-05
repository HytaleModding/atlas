import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";
import { runSearch, type SearchHit } from "@/lib/indexer";
import type { Slot } from "@/lib/patcher";

/** UI-level section selection. Maps to the backend `source_types` filter
 *  via [`sectionToSourceTypes`]. The `all` value sends an undefined
 *  filter (no narrowing). */
export type Section =
  | "all"
  | "source"
  | "hm_guides"
  | "javadocs"
  | "assets";

function sectionToSourceTypes(c: Section): string[] | undefined {
  switch (c) {
    case "all":
      return undefined;
    case "source":
      return ["source"];
    case "hm_guides":
      return ["hm_doc"];
    case "javadocs":
      return ["hypixel_doc"];
    case "assets":
      return ["asset"];
  }
}

/**
 * Single active query with debounce. The SearchPage writes to `query` on
 * every keystroke; this store holds a debounce timer and only issues a
 * Tauri call once the user pauses.
 *
 * We keep the selected hit here too so the RightPanel can render the
 * viewer without prop-drilling through AppShell.
 *
 * Per-branch state preservation: the top-level fields hold the *active*
 * branch's view. When the user flips branches via branchStore, we snapshot
 * the current state into `bySlot[oldSlot]` and restore from
 * `bySlot[newSlot]` so each branch behaves like its own tab - its query,
 * results, selection, viewer history, and section chip survive the flip.
 */
type SlotSnapshot = {
  query: string;
  section: Section;
  status: "idle" | "pending" | "ready" | "error";
  hits: SearchHit[];
  elapsedMs: number | null;
  error: string | null;
  selectedHit: SearchHit | null;
  limit: number;
  hasMore: boolean;
  viewerHistory: SearchHit[];
  viewerHistoryIndex: number;
};

type SearchState = {
  query: string;
  slot: Slot | null;
  /** Active section chip. Drives the backend `source_types` filter. */
  section: Section;
  status: "idle" | "pending" | "ready" | "error";
  hits: SearchHit[];
  elapsedMs: number | null;
  error: string | null;
  selectedHit: SearchHit | null;
  lastRequestId: number;
  /**
   * How many hits the backend was asked for on the most recent request.
   * Grows by PAGE_SIZE each time the user clicks "Show more". Reset to
   * PAGE_SIZE whenever the query changes.
   */
  limit: number;
  /** True if the last response returned exactly `limit` hits, i.e. there
   * may be more results behind a paged request. */
  hasMore: boolean;

  /** Most-recent-first list of distinct queries the user has run.
   *  Capped at HISTORY_MAX. Persisted across sessions. */
  history: string[];
  /** Back/forward stack for the file viewer. Pushing onto the stack
   *  truncates any forward entries past the current index. Both this
   *  and `viewerHistoryIndex` are session-only (not persisted). */
  viewerHistory: SearchHit[];
  viewerHistoryIndex: number;

  /** Which slot the top-level fields belong to. `null` until the first
   *  `switchSlot` call lands. */
  currentSlot: Slot | null;
  /** Stashed per-slot snapshots for branches the user isn't currently
   *  looking at. Populated lazily on `switchSlot`. */
  bySlot: Partial<Record<Slot, SlotSnapshot>>;

  setQuery: (q: string, slot: Slot | null) => void;
  /** Switch section; if a query is active, re-run it through the new filter. */
  setSection: (c: Section) => void;
  /** Fire immediately, bypassing debounce. */
  runNow: (q: string, slot: Slot, limit?: number) => Promise<void>;
  /** Fetch the next page (current limit + PAGE_SIZE, server-capped at 100). */
  showMore: () => void;
  setSelected: (hit: SearchHit | null) => void;
  /** Step through viewer history. Returns the new selected hit, or null
   *  if the bound was hit. */
  viewerBack: () => SearchHit | null;
  viewerForward: () => SearchHit | null;
  /** Push a query onto history, dedup-MRU. */
  pushHistory: (q: string) => void;
  /** Clear all persisted query history. */
  clearHistory: () => void;
  /** Snapshot the current branch's state, then restore the other branch's
   *  saved state (or defaults if it's never been visited). Called from
   *  `branchStore.set` whenever the branch toggle flips. */
  switchSlot: (newSlot: Slot) => void;
  reset: () => void;
};

function emptySnapshot(): SlotSnapshot {
  return {
    query: "",
    section: "all",
    status: "idle",
    hits: [],
    elapsedMs: null,
    error: null,
    selectedHit: null,
    limit: PAGE_SIZE,
    hasMore: false,
    viewerHistory: [],
    viewerHistoryIndex: -1,
  };
}

let debounceTimer: ReturnType<typeof setTimeout> | null = null;
/** Keystroke quiet-time before firing a search. 180ms sits in the sweet
 *  spot where typists feel the box is responsive (sub-200ms feels
 *  immediate) but a fast burst of characters still collapses to one
 *  query rather than spamming the backend per keystroke. */
const DEBOUNCE_MS = 180;
/** Initial result count + step size for the "Show more" pill. The
 *  backend caps at `MAX_LIMIT`; once `limit` reaches that, pagination
 *  is exhausted. */
const PAGE_SIZE = 10;
/** Mirrors the server-side hit cap in src-tauri/src/commands.rs
 *  (`run_search` clamps to 100). Keep in sync if the backend cap
 *  changes. */
const MAX_LIMIT = 100;
/** Cap on the persisted recent-query list shown in the search box
 *  dropdown. Bounded so localStorage doesn't grow unboundedly. */
const HISTORY_MAX = 10;
/** Cap on the in-session viewer back/forward stack. Behaves like a
 *  browser history: oldest entries fall off once the user has clicked
 *  through this many results. */
const VIEWER_HISTORY_MAX = 50;

export const useSearchStore = create<SearchState>()(
  persist(
    (set, get) => ({
      query: "",
      slot: null,
      section: "all",
      status: "idle",
      hits: [],
      elapsedMs: null,
      error: null,
      selectedHit: null,
      lastRequestId: 0,
      limit: PAGE_SIZE,
      hasMore: false,
      history: [],
      viewerHistory: [],
      viewerHistoryIndex: -1,
      currentSlot: null,
      bySlot: {},

      setQuery: (q, slot) => {
        // New query → reset paging state.
        set({ query: q, slot, error: null, limit: PAGE_SIZE, hasMore: false });
        if (debounceTimer) clearTimeout(debounceTimer);
        const trimmed = q.trim();
        if (trimmed.length === 0 || slot === null) {
          set({ status: "idle", hits: [], elapsedMs: null, selectedHit: null });
          return;
        }
        set({ status: "pending" });
        debounceTimer = setTimeout(() => {
          void get().runNow(trimmed, slot, PAGE_SIZE);
        }, DEBOUNCE_MS);
      },

      setSection: (c) => {
        const { query, slot } = get();
        set({ section: c });
        const trimmed = query.trim();
        if (trimmed.length === 0 || slot === null) return;
        if (debounceTimer) clearTimeout(debounceTimer);
        set({ status: "pending", limit: PAGE_SIZE, hasMore: false });
        void get().runNow(trimmed, slot, PAGE_SIZE);
      },

      runNow: async (q, slot, limit) => {
        const requestedLimit = limit ?? get().limit;
        const id = get().lastRequestId + 1;
        set({ lastRequestId: id, status: "pending", error: null, limit: requestedLimit });
        try {
          const response = await runSearch(
            slot,
            q,
            requestedLimit,
            sectionToSourceTypes(get().section),
          );
          // Drop stale responses if a newer request fired first.
          if (get().lastRequestId !== id) return;
          // If the backend returned exactly what we asked for AND we haven't
          // hit the server cap, there may be more results behind a bigger
          // limit. The vector-side distance threshold (lance/mod.rs) means
          // requesting more won't pad with garbage; the response is the
          // genuinely-relevant tail or fewer rows.
          const hasMore =
            response.hits.length >= requestedLimit && requestedLimit < MAX_LIMIT;
          // Preserve the current selection when the same file is still in
          // the new hit list. Avoids a full viewer rebuild (readSource +
          // getInlineJavadocs + shiki tokenization) on every keystroke
          // while the user is refining a query. Falls back to the first
          // hit only when the prior selection has been pushed out.
          const prior = get().selectedHit;
          let nextSelected: SearchHit | null = response.hits[0] ?? null;
          if (prior) {
            const stillThere = response.hits.find(
              (h) => h.slot === prior.slot && h.path === prior.path,
            );
            if (stillThere) nextSelected = stillThere;
          }
          set({
            status: "ready",
            hits: response.hits,
            elapsedMs: response.elapsed_ms,
            selectedHit: nextSelected,
            hasMore,
          });
          // Record successful query for history. Only when we got hits, so
          // typos don't pollute the dropdown.
          if (response.hits.length > 0) get().pushHistory(q);
        } catch (err) {
          if (get().lastRequestId !== id) return;
          set({
            status: "error",
            error: String(err),
            hits: [],
            elapsedMs: null,
            hasMore: false,
          });
        }
      },

      showMore: () => {
        const { query, slot, limit, status } = get();
        if (status === "pending") return;
        if (slot === null) return;
        const trimmed = query.trim();
        if (trimmed.length === 0) return;
        const next = Math.min(limit + PAGE_SIZE, MAX_LIMIT);
        if (next === limit) return;
        void get().runNow(trimmed, slot, next);
      },

      setSelected: (hit) => {
        const prev = get().selectedHit;
        set({ selectedHit: hit });
        if (!hit) return;
        // Push onto viewer history unless we're already at the same hit.
        if (
          prev &&
          prev.slot === hit.slot &&
          prev.path === hit.path &&
          prev.start_line === hit.start_line &&
          prev.end_line === hit.end_line
        ) {
          return;
        }
        const { viewerHistory, viewerHistoryIndex } = get();
        // Truncate any forward entries (we just took a new branch).
        const truncated = viewerHistory.slice(0, viewerHistoryIndex + 1);
        truncated.push(hit);
        const trimmed =
          truncated.length > VIEWER_HISTORY_MAX
            ? truncated.slice(truncated.length - VIEWER_HISTORY_MAX)
            : truncated;
        set({ viewerHistory: trimmed, viewerHistoryIndex: trimmed.length - 1 });
      },

      viewerBack: () => {
        const { viewerHistory, viewerHistoryIndex } = get();
        if (viewerHistoryIndex <= 0) return null;
        const nextIdx = viewerHistoryIndex - 1;
        const next = viewerHistory[nextIdx];
        set({ viewerHistoryIndex: nextIdx, selectedHit: next });
        return next;
      },

      viewerForward: () => {
        const { viewerHistory, viewerHistoryIndex } = get();
        if (viewerHistoryIndex >= viewerHistory.length - 1) return null;
        const nextIdx = viewerHistoryIndex + 1;
        const next = viewerHistory[nextIdx];
        set({ viewerHistoryIndex: nextIdx, selectedHit: next });
        return next;
      },

      pushHistory: (q) => {
        const trimmed = q.trim();
        if (trimmed.length === 0) return;
        const prior = get().history;
        // Dedup MRU: drop any prior occurrence, prepend the new one.
        const filtered = prior.filter((h) => h !== trimmed);
        filtered.unshift(trimmed);
        set({ history: filtered.slice(0, HISTORY_MAX) });
      },

      clearHistory: () => set({ history: [] }),

      switchSlot: (newSlot) => {
        const state = get();
        if (state.currentSlot === newSlot) {
          // First call after boot: just record the slot without snapshotting
          // (top-level state is already a clean default for it).
          if (state.currentSlot === null) {
            set({ currentSlot: newSlot, slot: newSlot });
          }
          return;
        }
        // Cancel any pending debounce so a search keystroke from the prior
        // branch doesn't fire against the new one.
        if (debounceTimer) {
          clearTimeout(debounceTimer);
          debounceTimer = null;
        }
        // Snapshot the current branch's state so a flip-back restores it.
        const bySlot = { ...state.bySlot };
        if (state.currentSlot !== null) {
          bySlot[state.currentSlot] = {
            query: state.query,
            section: state.section,
            status: state.status,
            hits: state.hits,
            elapsedMs: state.elapsedMs,
            error: state.error,
            selectedHit: state.selectedHit,
            limit: state.limit,
            hasMore: state.hasMore,
            viewerHistory: state.viewerHistory,
            viewerHistoryIndex: state.viewerHistoryIndex,
          };
        }
        const restore = bySlot[newSlot] ?? emptySnapshot();
        // Bumping `lastRequestId` invalidates any in-flight `runNow` started
        // for the prior branch - its `if (get().lastRequestId !== id) return;`
        // guard will drop the response instead of writing it into the new
        // branch's hit list.
        set({
          query: restore.query,
          section: restore.section,
          status: restore.status,
          hits: restore.hits,
          elapsedMs: restore.elapsedMs,
          error: restore.error,
          selectedHit: restore.selectedHit,
          limit: restore.limit,
          hasMore: restore.hasMore,
          viewerHistory: restore.viewerHistory,
          viewerHistoryIndex: restore.viewerHistoryIndex,
          slot: newSlot,
          currentSlot: newSlot,
          bySlot,
          lastRequestId: state.lastRequestId + 1,
        });
      },

      reset: () => {
        if (debounceTimer) clearTimeout(debounceTimer);
        set({
          query: "",
          status: "idle",
          hits: [],
          elapsedMs: null,
          error: null,
          selectedHit: null,
          limit: PAGE_SIZE,
          hasMore: false,
          viewerHistory: [],
          viewerHistoryIndex: -1,
        });
      },
    }),
    {
      name: "atlas:search",
      storage: createJSONStorage(() => localStorage),
      version: 1,
      // Only persist the history. Everything else (query, hits,
      // selection, viewer history) resets on reload.
      partialize: (state) => ({ history: state.history }),
    },
  ),
);
