import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  getFetchStatus,
  listMountedIndexes,
  mountLocal,
  removeIndex,
  resolveRemoteBuild,
  setActiveIndex,
  startFetch,
  type FetchPhase,
  type FetchRequest,
  type FetchStatus,
  type MountedIndexEntry,
  type RemoteBuildResolution,
} from "@/lib/fetcher";
import type { Slot } from "@/lib/patcher";
import { useIndexStore } from "@/state/indexStore";
import { useConfigStore } from "@/state/configStore";

/**
 * Live fetch state + cached catalog listing for the Index Catalog UX.
 *
 * Shape mirrors `indexStore`: one `status`/`progress` pair tagged with
 * whichever build is currently being pulled, plus a cached `mounted`
 * array the Index Catalog page renders. Events emitted by the backend
 * (`fetch:phase`, `fetch:progress`, `fetch:done`, `fetch:error`) are
 * normalized to the same camelCase shape `FetchStatus` uses.
 */
type FetchState = {
  status: FetchStatus;
  activeBuildId: string | null;
  progress: {
    phase: FetchPhase | null;
    /** bytes for download phase; entries for extract phase */
    current: number;
    /** optional for download, definite for extract */
    total: number | null;
  };
  mounted: MountedIndexEntry[];
  errorMessage: string | null;
  refreshing: boolean;
  /** Most recent resolver result per patchline. `null` means "no update
   *  available"; `undefined` means "haven't checked yet". Reset when the
   *  user clicks Refresh or successfully mounts the resolved build. */
  remoteResolutions: Partial<
    Record<Slot, RemoteBuildResolution | null>
  >;
  checkingUpdates: boolean;

  start: (request: FetchRequest) => Promise<void>;
  startLocalMount: (tarballPath: string) => Promise<void>;
  refreshStatus: () => Promise<void>;
  refreshCatalog: () => Promise<void>;
  /** Hit the central repo for both patchlines and store the result. */
  checkForUpdates: () => Promise<void>;
  /** Delete a mounted build. Throws on backend refusal so callers can toast. */
  remove: (buildId: string) => Promise<void>;
  /** Mark a mounted build as the search target for its patchline. */
  setActive: (patchline: Slot, buildId: string) => Promise<void>;
  subscribe: () => Promise<UnlistenFn>;
  clearError: () => void;
};

const idleProgress = {
  phase: null as FetchPhase | null,
  current: 0,
  total: null as number | null,
};

export const useFetchStore = create<FetchState>((set, get) => ({
  status: { kind: "idle" },
  activeBuildId: null,
  progress: { ...idleProgress },
  mounted: [],
  errorMessage: null,
  refreshing: false,
  remoteResolutions: {},
  checkingUpdates: false,

  start: async (request) => {
    set({
      errorMessage: null,
      activeBuildId: request.buildId,
      progress: { ...idleProgress, phase: "resolving" },
      status: { kind: "phase", buildId: request.buildId, phase: "resolving" },
    });
    try {
      await startFetch(request);
      await get().refreshStatus();
    } catch (err) {
      set({ errorMessage: String(err), activeBuildId: null });
    }
  },

  startLocalMount: async (tarballPath) => {
    // The backend uses the file stem as the provisional buildId until
    // the manifest is parsed; mirror that here so the UI shows
    // *something* useful while verify is running.
    const stem =
      tarballPath
        .split(/[\\/]/)
        .pop()
        ?.replace(/\.tar\.zst$/i, "")
        ?.replace(/\.tar$/i, "") ?? "local-artifact";
    set({
      errorMessage: null,
      activeBuildId: stem,
      progress: { ...idleProgress, phase: "verifying" },
      status: { kind: "phase", buildId: stem, phase: "verifying" },
    });
    try {
      await mountLocal(tarballPath);
      await get().refreshStatus();
    } catch (err) {
      set({ errorMessage: String(err), activeBuildId: null });
    }
  },

  refreshStatus: async () => {
    try {
      const status = await getFetchStatus();
      set({ status });
      if (status.kind === "error") {
        set({ errorMessage: status.message });
      }
    } catch (err) {
      set({ errorMessage: String(err) });
    }
  },

  refreshCatalog: async () => {
    set({ refreshing: true });
    try {
      const mounted = await listMountedIndexes();
      set({ mounted, refreshing: false });
    } catch (err) {
      set({ errorMessage: String(err), refreshing: false });
    }
  },

  checkForUpdates: async () => {
    set({ checkingUpdates: true, errorMessage: null });
    // Start from existing resolutions so a partial-failure run keeps any
    // slot that already resolved on a prior call, and one slot's failure
    // can't blank out the other's value.
    const next: Partial<Record<Slot, RemoteBuildResolution | null>> = {
      ...get().remoteResolutions,
    };
    const slots: Slot[] = ["release", "pre-release"];
    let lastError: string | null = null;
    for (const slot of slots) {
      try {
        next[slot] = await resolveRemoteBuild(slot);
      } catch (err) {
        // Surface every failure as its own toast so the user can tell
        // "couldn't reach the update server" apart from "no update
        // available" (which is the `null` case above). Continue to the
        // next slot rather than aborting the whole check.
        lastError = String(err);
        const slotName = slot === "release" ? "Release" : "Pre-release";
        toast.error(`Couldn't check for ${slotName} updates`, {
          description: String(err),
        });
      }
    }
    set({
      remoteResolutions: next,
      checkingUpdates: false,
      errorMessage: lastError,
    });
  },

  remove: async (buildId) => {
    try {
      await removeIndex(buildId);
      // refreshCatalog after delete so the UI list reflects reality.
      await get().refreshCatalog();
      // Active-build pointer may have been cleared in config; pull it.
      await useConfigStore.getState().refresh();
      // The catalog also feeds the home/search overview, so rebuild that.
      void useIndexStore.getState().refreshOverview();
    } catch (err) {
      set({ errorMessage: String(err) });
      throw err;
    }
  },

  setActive: async (patchline, buildId) => {
    try {
      await setActiveIndex(patchline, buildId);
      await useConfigStore.getState().refresh();
    } catch (err) {
      set({ errorMessage: String(err) });
      throw err;
    }
  },

  subscribe: async () => {
    const offs: UnlistenFn[] = [];

    // Phase events carry `buildId` + `phase`. We pre-populate progress
    // with defaults so the UI can show a placeholder bar before the
    // first progress event lands.
    offs.push(
      await listen<{ buildId: string; phase: FetchPhase }>(
        "fetch:phase",
        (event) => {
          const { buildId, phase } = event.payload;
          set({
            status: { kind: "phase", buildId, phase },
            activeBuildId: buildId,
            progress: { phase, current: 0, total: null },
          });
        },
      ),
    );

    // Progress events are shape-polymorphic: download carries
    // `received`/`total`, extract carries `current`/`total`. We
    // normalize to the store's shared `progress` shape.
    offs.push(
      await listen<ProgressPayload>("fetch:progress", (event) => {
        const payload = event.payload;
        if (payload.phase === "downloading") {
          set({
            status: {
              kind: "downloading",
              buildId: payload.buildId,
              received: payload.received ?? 0,
              total: payload.total ?? null,
            },
            activeBuildId: payload.buildId,
            progress: {
              phase: "downloading",
              current: payload.received ?? 0,
              total: payload.total ?? null,
            },
          });
        } else if (payload.phase === "extracting") {
          set({
            status: {
              kind: "extracting",
              buildId: payload.buildId,
              current: payload.current ?? 0,
              total: payload.total ?? 0,
            },
            activeBuildId: payload.buildId,
            progress: {
              phase: "extracting",
              current: payload.current ?? 0,
              total: payload.total ?? 0,
            },
          });
        }
      }),
    );

    offs.push(
      await listen<{ buildId: string; mountedAt: string }>(
        "fetch:done",
        (event) => {
          set({
            status: { kind: "done", buildId: event.payload.buildId },
            activeBuildId: null,
            progress: { ...idleProgress },
          });
          void get().refreshCatalog();
          // The fetcher just wrote `<indexes>/{tantivy,lance}/<slot>/`
          // and an `atlas-meta.json`. Re-pull the index overview so
          // BranchCard + SearchPage flip from "no index yet" to ready
          // without requiring a manual refresh / app restart.
          void useIndexStore.getState().refreshOverview();
          toast.success("Hytale data updated", {
            description: "Search now uses the new data.",
          });
        },
      ),
    );

    offs.push(
      await listen<{ buildId: string; message: string }>(
        "fetch:error",
        (event) => {
          set({
            status: {
              kind: "error",
              buildId: event.payload.buildId,
              message: event.payload.message,
            },
            activeBuildId: null,
            errorMessage: event.payload.message,
          });
        },
      ),
    );

    // Pull the initial snapshot. The backend may already be busy on
    // a reload (hot-reload or window-reopen).
    try {
      const status = await getFetchStatus();
      set({ status });
    } catch {
      // Swallow: store stays Idle on boot failure.
    }

    return () => offs.forEach((off) => off());
  },

  clearError: () => set({ errorMessage: null }),
}));

type ProgressPayload = {
  buildId: string;
  phase: FetchPhase;
  received?: number;
  total?: number | null;
  current?: number;
};
