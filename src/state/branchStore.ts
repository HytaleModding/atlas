import { create } from "zustand";
import type { Slot } from "@/lib/patcher";
import { useConfigStore } from "@/state/configStore";

/**
 * Which slot the app is currently focused on. Hydrated from
 * `AtlasConfig.active_branch` on load, and written back to config whenever
 * the user flips the toggle so the next launch remembers their pick.
 *
 * Kept separate from `useConfigStore` so branch flips don't thrash the
 * whole config snapshot and force every subscriber to re-render.
 */
type BranchState = {
  active: Slot;
  hydrated: boolean;
  /** Pull the persisted default out of config and into local state. */
  hydrate: () => void;
  /** Flip the active branch and persist the choice. */
  set: (slot: Slot) => Promise<void>;
};

export const useBranchStore = create<BranchState>((set, get) => ({
  active: "release",
  hydrated: false,

  hydrate: () => {
    const cfg = useConfigStore.getState().snapshot?.config;
    if (!cfg) return;
    set({ active: cfg.active_branch, hydrated: true });
  },

  set: async (slot) => {
    if (get().active === slot) return;
    set({ active: slot });
    // Persist. If the write fails we keep the in-memory flip so the UI
    // doesn't feel sticky; configStore surfaces its own error state.
    await useConfigStore.getState().update({ active_branch: slot });
  },
}));
