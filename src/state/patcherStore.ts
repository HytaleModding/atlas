import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  clearDecompile as invokeClearDecompile,
  getPatcherStatus,
  startDecompile as invokeStartDecompile,
  type PatcherPhase,
  type PatcherStatus,
  type Slot,
} from "@/lib/patcher";

/**
 * There is ever only one decompile in flight; the backend rejects a second
 * `start_decompile` while the `SharedStatus` is busy. So this store holds a
 * single `status` + `progress` pair and tags them with whichever slot the
 * current run belongs to (`activeSlot`). The slot card component uses that
 * to decide whether to render its own inline progress or stay static.
 */
type PatcherState = {
  status: PatcherStatus;
  activeSlot: Slot | null;
  progress: {
    phase: PatcherPhase | null;
    current: number;
    total: number | null;
    /** For downloads (bytes received). */
    received: number | null;
  };
  starting: boolean;
  errorMessage: string | null;

  start: (slot: Slot) => Promise<void>;
  clear: (slot: Slot) => Promise<void>;
  refreshStatus: () => Promise<void>;
  /** Attach Tauri event listeners. Returns an unlisten disposer. */
  subscribe: () => Promise<UnlistenFn>;
  clearError: () => void;
};

const idleProgress = {
  phase: null as PatcherPhase | null,
  current: 0,
  total: null as number | null,
  received: null as number | null,
};

export const usePatcherStore = create<PatcherState>((set, get) => ({
  status: { kind: "idle" },
  activeSlot: null,
  progress: { ...idleProgress },
  starting: false,
  errorMessage: null,

  start: async (slot) => {
    if (get().starting) return;
    set({
      starting: true,
      errorMessage: null,
      activeSlot: slot,
      progress: { ...idleProgress },
    });
    try {
      await invokeStartDecompile(slot);
      await get().refreshStatus();
    } catch (err) {
      set({ errorMessage: String(err), activeSlot: null });
    } finally {
      set({ starting: false });
    }
  },

  clear: async (slot) => {
    try {
      await invokeClearDecompile(slot);
      set({ errorMessage: null });
    } catch (err) {
      set({ errorMessage: String(err) });
    }
  },

  refreshStatus: async () => {
    try {
      const status = await getPatcherStatus();
      set({ status });
      if (status.kind === "error") {
        set({ errorMessage: status.message });
      }
    } catch (err) {
      set({ errorMessage: String(err) });
    }
  },

  subscribe: async () => {
    const offs: UnlistenFn[] = [];

    offs.push(
      await listen<{ slot: Slot; phase: PatcherPhase }>(
        "decompile:phase",
        (event) => {
          set({
            status: { kind: "phase", phase: event.payload.phase },
            activeSlot: event.payload.slot,
            progress: { ...idleProgress, phase: event.payload.phase },
          });
        },
      ),
    );

    offs.push(
      await listen<{
        slot: Slot;
        phase: PatcherPhase;
        current?: number;
        total?: number;
        received?: number;
      }>("decompile:progress", (event) => {
        const { slot, phase, current, total, received } = event.payload;
        set((s) => ({
          activeSlot: slot,
          progress: {
            phase,
            current: current ?? s.progress.current,
            total: total ?? s.progress.total,
            received: received ?? s.progress.received,
          },
        }));
      }),
    );

    offs.push(
      await listen<{ slot: Slot; outputDir: string }>(
        "decompile:done",
        (event) => {
          set({
            status: { kind: "done", output_dir: event.payload.outputDir },
            activeSlot: null,
            progress: { ...idleProgress },
          });
        },
      ),
    );

    offs.push(
      await listen<{ slot: Slot; message: string }>(
        "decompile:error",
        (event) => {
          set({
            status: { kind: "error", message: event.payload.message },
            activeSlot: null,
            errorMessage: event.payload.message,
          });
        },
      ),
    );

    return () => offs.forEach((off) => off());
  },

  clearError: () => set({ errorMessage: null }),
}));
