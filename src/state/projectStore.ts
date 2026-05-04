import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  projectIndex,
  projectList,
  projectRegister,
  projectRemoveIndex,
  projectUnregister,
  type ProjectDonePayload,
  type ProjectErrorPayload,
  type ProjectListEntry,
  type ProjectPhasePayload,
  type ProjectProgressPayload,
} from "@/lib/project";

/**
 * Live state for the user's mod projects: registered list (source of
 * truth lives in the backend's `projects.json`) plus per-project index
 * progress derived from `project:*` events.
 *
 * Mirrors `useFetchStore`'s shape so the IndexCatalog can render the
 * Projects section with the same affordances as the Hytale data section.
 */
type ProjectIndexProgress = {
  phase: string | null;
  current: number;
  total: number | null;
  chunks: number;
};

type ProjectState = {
  projects: ProjectListEntry[];
  /** Per-project active index progress, keyed by project id. Absent
   *  means idle. */
  progress: Record<string, ProjectIndexProgress>;
  /** Per-project last error message. Cleared when a fresh run starts. */
  errors: Record<string, string>;
  loading: boolean;

  refresh: () => Promise<void>;
  register: (path: string, name?: string) => Promise<string>;
  unregister: (id: string) => Promise<void>;
  removeIndex: (id: string) => Promise<void>;
  startIndex: (id: string) => Promise<void>;
  subscribe: () => Promise<UnlistenFn>;
  clearError: (id: string) => void;
};

const idleProgress: ProjectIndexProgress = {
  phase: null,
  current: 0,
  total: null,
  chunks: 0,
};

export const useProjectStore = create<ProjectState>((set, get) => ({
  projects: [],
  progress: {},
  errors: {},
  loading: false,

  refresh: async () => {
    set({ loading: true });
    try {
      const projects = await projectList();
      set({ projects, loading: false });
    } catch (err) {
      set({ loading: false });
      toast.error("Could not load projects", { description: String(err) });
    }
  },

  register: async (path, name) => {
    const id = await projectRegister(path, name);
    await get().refresh();
    return id;
  },

  unregister: async (id) => {
    await projectUnregister(id);
    set((state) => {
      const { [id]: _p, ...progress } = state.progress;
      const { [id]: _e, ...errors } = state.errors;
      return { progress, errors };
    });
    await get().refresh();
  },

  removeIndex: async (id) => {
    await projectRemoveIndex(id);
    await get().refresh();
  },

  startIndex: async (id) => {
    set((state) => ({
      progress: { ...state.progress, [id]: { ...idleProgress } },
      errors: { ...state.errors, [id]: "" },
    }));
    try {
      await projectIndex(id);
    } catch (err) {
      set((state) => ({
        errors: { ...state.errors, [id]: String(err) },
      }));
      throw err;
    }
  },

  subscribe: async () => {
    const offs: UnlistenFn[] = [];

    offs.push(
      await listen<ProjectPhasePayload>("project:phase", (event) => {
        const { project_id, phase } = event.payload;
        set((state) => ({
          progress: {
            ...state.progress,
            [project_id]: {
              ...(state.progress[project_id] ?? idleProgress),
              phase,
            },
          },
        }));
      }),
    );

    offs.push(
      await listen<ProjectProgressPayload>("project:progress", (event) => {
        const { project_id, current, total, chunks } = event.payload;
        set((state) => ({
          progress: {
            ...state.progress,
            [project_id]: {
              phase: state.progress[project_id]?.phase ?? null,
              current,
              total,
              chunks,
            },
          },
        }));
      }),
    );

    offs.push(
      await listen<ProjectDonePayload>("project:done", (event) => {
        const { project_id, docs } = event.payload;
        set((state) => {
          const { [project_id]: _gone, ...rest } = state.progress;
          return { progress: rest };
        });
        toast.success("Project indexed", {
          description: `${docs.toLocaleString()} files indexed.`,
        });
        // Reload the project list so `index_ready` flips to true and
        // `last_indexed_at` updates without a manual refresh.
        void get().refresh();
      }),
    );

    offs.push(
      await listen<ProjectErrorPayload>("project:error", (event) => {
        const { project_id, message } = event.payload;
        set((state) => {
          const { [project_id]: _gone, ...rest } = state.progress;
          return {
            progress: rest,
            errors: { ...state.errors, [project_id]: message },
          };
        });
        toast.error("Project indexing failed", { description: message });
      }),
    );

    return () => offs.forEach((off) => off());
  },

  clearError: (id) =>
    set((state) => {
      const { [id]: _gone, ...rest } = state.errors;
      return { errors: rest };
    }),
}));
