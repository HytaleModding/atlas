import { create } from "zustand";
import type { Slot } from "@/lib/patcher";
import { useConfigStore } from "@/state/configStore";
import { useSearchStore } from "@/state/searchStore";
import { useTabsStore } from "@/state/tabsStore";

/**
 * Which slot the app is currently focused on. Hydrated from
 * `AtlasConfig.active_branch` on load, and written back to config whenever
 * the user flips the toggle so the next launch remembers their pick.
 *
 * Kept separate from `useConfigStore` so branch flips don't thrash the
 * whole config snapshot and force every subscriber to re-render.
 *
 * Branches act like tabs: each one preserves its own search query, open
 * files, viewer selection, and scroll positions. We achieve that by
 * calling `switchSlot` on `searchStore` + `tabsStore` whenever the active
 * branch flips, so they snapshot the outgoing branch's state into a
 * stash and restore the incoming one.
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
    // First-launch alignment: tell the per-slot stores which branch the
    // current top-level state belongs to. They start with currentSlot=null
    // and use this call to record the boot slot without snapshotting.
    useSearchStore.getState().switchSlot(cfg.active_branch);
    useTabsStore.getState().switchSlot(cfg.active_branch);
  },

  set: async (slot) => {
    if (get().active === slot) return;
    // Snapshot/restore per-slot state BEFORE flipping `active`. Components
    // re-render on `active` change and read from the stores; doing the
    // swap first means they read the restored state on the same tick.
    useSearchStore.getState().switchSlot(slot);
    useTabsStore.getState().switchSlot(slot);
    set({ active: slot });
    // Persist. If the write fails we keep the in-memory flip so the UI
    // doesn't feel sticky; configStore surfaces its own error state.
    await useConfigStore.getState().update({ active_branch: slot });
  },
}));
