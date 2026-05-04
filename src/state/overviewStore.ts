import { create } from "zustand";
import {
  getPatcherOverview,
  type PatcherOverview,
  type Slot,
  type SlotOverview,
} from "@/lib/patcher";

/**
 * Cached `patcher_overview` result. Several components need this at once
 * (slot card, LeftNav version label, Search-empty gating), so we keep one
 * snapshot here and refresh it whenever state on disk could have changed:
 * post-decompile, post-clear, post-config-save.
 */
type OverviewState = {
  overview: PatcherOverview | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  /** Lookup helper for one slot. Returns null until first load. */
  slot: (slot: Slot) => SlotOverview | null;
};

export const useOverviewStore = create<OverviewState>((set, get) => ({
  overview: null,
  loading: false,
  error: null,

  refresh: async () => {
    set({ loading: true, error: null });
    try {
      const overview = await getPatcherOverview();
      set({ overview, loading: false });
    } catch (err) {
      set({ error: String(err), loading: false });
    }
  },

  slot: (slot) => {
    const o = get().overview;
    if (!o) return null;
    return slot === "release" ? o.release : o.pre_release;
  },
}));
