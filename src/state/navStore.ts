import { create } from "zustand";

/**
 * Tiny page-routing store. Active pages are Search (default), Hytale
 * Data (the user-facing read-only status view, internally `catalog`),
 * and Settings (which carries a Developer section for in-app testing).
 * Projects, Tracker, and Logs land in later phases; a real router is
 * overkill until then.
 */
export type PageId = "search" | "catalog" | "settings";

type NavState = {
  page: PageId;
  setPage: (page: PageId) => void;
};

export const useNavStore = create<NavState>((set) => ({
  page: "search",
  setPage: (page) => set({ page }),
}));
