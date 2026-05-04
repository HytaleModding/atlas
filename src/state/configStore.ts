import { create } from "zustand";
import {
  type AtlasConfig,
  type ConfigSnapshot,
  loadConfig,
  saveConfig,
} from "@/lib/config";

type ConfigState = {
  snapshot: ConfigSnapshot | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  update: (partial: Partial<AtlasConfig>) => Promise<void>;
};

const EMPTY_CONFIG: AtlasConfig = {
  hytale_release_path: null,
  hytale_prerelease_path: null,
  first_run_skipped: false,
  active_branch: "release",
  central_repo: "Vibe-Theory/atlastest",
  active_release_build: null,
  active_pre_release_build: null,
};

/** Global Atlas config, kept in sync with the Rust-side JSON file. */
export const useConfigStore = create<ConfigState>((set, get) => ({
  snapshot: null,
  loading: true,
  error: null,

  refresh: async () => {
    set({ loading: true, error: null });
    try {
      const snapshot = await loadConfig();
      set({ snapshot, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  update: async (partial) => {
    const current = get().snapshot?.config ?? EMPTY_CONFIG;
    const next: AtlasConfig = { ...current, ...partial };
    await saveConfig(next);
    await get().refresh();
  },
}));
