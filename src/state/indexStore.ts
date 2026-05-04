import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  getIndexOverview,
  getIndexStatus,
  startIndex,
  type IndexOverview,
  type IndexerPhase,
  type IndexerStatus,
} from "@/lib/indexer";
import type { Slot } from "@/lib/patcher";

/**
 * Single-run indexer state + cached per-slot overview.
 *
 * Shape mirrors `patcherStore`: one `status`/`progress` pair tagged with
 * whichever slot the current run belongs to, plus a cached `overview` so
 * the branch card and search page can show readiness without re-querying.
 */
type IndexState = {
  status: IndexerStatus;
  activeSlot: Slot | null;
  progress: {
    phase: IndexerPhase | null;
    current: number;
    total: number;
  };
  overview: IndexOverview | null;
  errorMessage: string | null;

  start: (slot: Slot) => Promise<void>;
  refreshStatus: () => Promise<void>;
  refreshOverview: () => Promise<void>;
  subscribe: () => Promise<UnlistenFn>;
  clearError: () => void;
};

const idleProgress = {
  phase: null as IndexerPhase | null,
  current: 0,
  total: 0,
};

export const useIndexStore = create<IndexState>((set, get) => ({
  status: { kind: "idle" },
  activeSlot: null,
  progress: { ...idleProgress },
  overview: null,
  errorMessage: null,

  start: async (slot) => {
    set({ errorMessage: null, activeSlot: slot, progress: { ...idleProgress } });
    try {
      await startIndex(slot);
      await get().refreshStatus();
    } catch (err) {
      set({ errorMessage: String(err), activeSlot: null });
    }
  },

  refreshStatus: async () => {
    try {
      const status = await getIndexStatus();
      set({ status });
      if (status.kind === "error") {
        set({ errorMessage: status.message });
      }
    } catch (err) {
      set({ errorMessage: String(err) });
    }
  },

  refreshOverview: async () => {
    try {
      const overview = await getIndexOverview();
      set({ overview });
    } catch (err) {
      set({ errorMessage: String(err) });
    }
  },

  subscribe: async () => {
    const offs: UnlistenFn[] = [];

    offs.push(
      await listen<{ slot: Slot; phase: IndexerPhase }>(
        "index:phase",
        (event) => {
          set({
            status: {
              kind: "phase",
              slot: event.payload.slot,
              phase: event.payload.phase,
            },
            activeSlot: event.payload.slot,
            progress: { ...idleProgress, phase: event.payload.phase },
          });
        },
      ),
    );

    offs.push(
      await listen<{
        slot: Slot;
        phase: IndexerPhase;
        current: number;
        total: number;
      }>("index:progress", (event) => {
        set({
          status: {
            kind: "progress",
            slot: event.payload.slot,
            phase: event.payload.phase,
            current: event.payload.current,
            total: event.payload.total,
          },
          activeSlot: event.payload.slot,
          progress: {
            phase: event.payload.phase,
            current: event.payload.current,
            total: event.payload.total,
          },
        });
      }),
    );

    offs.push(
      await listen<{ slot: Slot; docs: number }>("index:done", (event) => {
        set({
          status: {
            kind: "done",
            slot: event.payload.slot,
            docs: event.payload.docs,
          },
          activeSlot: null,
          progress: { ...idleProgress },
        });
        void get().refreshOverview();
      }),
    );

    offs.push(
      await listen<{ slot: Slot; message: string }>("index:error", (event) => {
        set({
          status: {
            kind: "error",
            slot: event.payload.slot,
            message: event.payload.message,
          },
          activeSlot: null,
          errorMessage: event.payload.message,
        });
      }),
    );

    return () => offs.forEach((off) => off());
  },

  clearError: () => set({ errorMessage: null }),
}));
