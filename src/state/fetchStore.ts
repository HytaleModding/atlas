import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  getFetchStatus,
  listMountedIndexes,
  mountLocal,
  startFetch,
  type FetchPhase,
  type FetchRequest,
  type FetchStatus,
  type MountedIndexEntry,
} from "@/lib/fetcher";
import { useIndexStore } from "@/state/indexStore";

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

  start: (request: FetchRequest) => Promise<void>;
  startLocalMount: (tarballPath: string) => Promise<void>;
  refreshStatus: () => Promise<void>;
  refreshCatalog: () => Promise<void>;
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
