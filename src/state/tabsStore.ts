import { create } from "zustand";
import type { SearchHit } from "@/lib/indexer";
import type { Slot } from "@/lib/patcher";
import { useSearchStore } from "@/state/searchStore";

/**
 * A tab is identified by `${slot}:${path}` so opening another chunk of
 * the same file reuses the existing tab and just nudges the viewer to
 * the new line. The hit stored on the tab is the *most recent* hit the
 * user opened for that file (line range, selected chunk, etc.) so the
 * viewer can pick up cleanly when the tab is reactivated.
 *
 * Per-branch tab strips: `tabs` and `activeId` reflect the *currently
 * active branch only*. When the user flips branches we snapshot
 * `{ tabs, activeId }` into `bySlot[oldSlot]` and restore from
 * `bySlot[newSlot]`, so each branch keeps its own open files and active
 * tab. `scrollByTabId` stays global because the tab id already includes
 * the slot prefix, so positions don't collide across branches.
 */
type TabsSlotSnapshot = {
  tabs: SearchHit[];
  activeId: string | null;
};

type TabsState = {
  tabs: SearchHit[];
  activeId: string | null;
  /** Per-tab scroll position. Saved as the user scrolls, restored when
   *  the tab is reactivated so flipping between tabs preserves where
   *  the reader was. Keyed by `${slot}:${path}`. */
  scrollByTabId: Record<string, number>;
  /** Which slot the top-level fields belong to. `null` until the first
   *  `switchSlot` lands. */
  currentSlot: Slot | null;
  /** Stashed per-slot { tabs, activeId } for the inactive branch. */
  bySlot: Partial<Record<Slot, TabsSlotSnapshot>>;
  openTab: (hit: SearchHit) => void;
  closeTab: (id: string) => void;
  setActive: (id: string) => void;
  setScroll: (id: string, top: number) => void;
  /** Snapshot the active branch's tab strip into bySlot, restore the
   *  other branch's. Called from `branchStore.set`. */
  switchSlot: (newSlot: Slot) => void;
  reset: () => void;
};

export function tabIdOf(hit: SearchHit): string {
  return `${hit.slot}:${hit.path}`;
}

export const useTabsStore = create<TabsState>((set, get) => ({
  tabs: [],
  activeId: null,
  scrollByTabId: {},
  currentSlot: null,
  bySlot: {},

  openTab: (hit) => {
    const id = tabIdOf(hit);
    const { tabs, scrollByTabId } = get();
    const existingIdx = tabs.findIndex((t) => tabIdOf(t) === id);
    let nextTabs: SearchHit[];
    let nextScroll = scrollByTabId;
    if (existingIdx >= 0) {
      const prevHit = tabs[existingIdx];
      // Refresh the stored hit so the viewer scrolls to the new chunk.
      nextTabs = tabs.slice();
      nextTabs[existingIdx] = hit;
      // If the user clicked a different chunk in the same file (different
      // preview_line), drop the saved scroll so the viewer scrolls to the
      // new chunk instead of jumping back to the old reading position.
      if (prevHit.preview_line !== hit.preview_line && id in scrollByTabId) {
        nextScroll = { ...scrollByTabId };
        delete nextScroll[id];
      }
    } else {
      nextTabs = [...tabs, hit];
    }
    set({ tabs: nextTabs, activeId: id, scrollByTabId: nextScroll });
    // Keep the viewer in sync. The viewer reads from searchStore so we
    // don't have to refactor RightPanel to read from tabs.
    useSearchStore.getState().setSelected(hit);
  },

  closeTab: (id) => {
    const { tabs, activeId, scrollByTabId } = get();
    const idx = tabs.findIndex((t) => tabIdOf(t) === id);
    if (idx < 0) return;
    const nextTabs = tabs.slice(0, idx).concat(tabs.slice(idx + 1));
    let nextActive: string | null = activeId;
    if (activeId === id) {
      // Activate the neighbor: prefer the tab to the right, fall back
      // to the left, fall back to nothing.
      const fallback = nextTabs[idx] ?? nextTabs[idx - 1] ?? null;
      nextActive = fallback ? tabIdOf(fallback) : null;
      useSearchStore.getState().setSelected(fallback);
    }
    let nextScroll = scrollByTabId;
    if (id in scrollByTabId) {
      nextScroll = { ...scrollByTabId };
      delete nextScroll[id];
    }
    set({ tabs: nextTabs, activeId: nextActive, scrollByTabId: nextScroll });
  },

  setActive: (id) => {
    const { tabs } = get();
    const tab = tabs.find((t) => tabIdOf(t) === id);
    if (!tab) return;
    set({ activeId: id });
    useSearchStore.getState().setSelected(tab);
  },

  setScroll: (id, top) => {
    const { scrollByTabId } = get();
    if (scrollByTabId[id] === top) return;
    set({ scrollByTabId: { ...scrollByTabId, [id]: top } });
  },

  switchSlot: (newSlot) => {
    const state = get();
    if (state.currentSlot === newSlot) {
      if (state.currentSlot === null) set({ currentSlot: newSlot });
      return;
    }
    const bySlot = { ...state.bySlot };
    if (state.currentSlot !== null) {
      bySlot[state.currentSlot] = {
        tabs: state.tabs,
        activeId: state.activeId,
      };
    }
    const restore = bySlot[newSlot] ?? { tabs: [], activeId: null };
    set({
      tabs: restore.tabs,
      activeId: restore.activeId,
      currentSlot: newSlot,
      bySlot,
    });
    // We deliberately do NOT mirror activeId's hit back into
    // searchStore.selectedHit here — searchStore.switchSlot already
    // restored its own selectedHit, which may differ if the user used
    // Alt+← viewer history before the flip.
  },

  reset: () =>
    set({
      tabs: [],
      activeId: null,
      scrollByTabId: {},
      bySlot: {},
      currentSlot: null,
    }),
}));
