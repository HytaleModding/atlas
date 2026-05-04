import { create } from "zustand";

import { diffRun, type DiffReport } from "@/lib/diff";

/** The diff page is parameterised by three ids - project, baseline,
 *  target - that the user picks (or that the project row pre-fills).
 *  We hold them here so navigating to the page doesn't lose context.
 *  Once `report` is populated the page renders results; clearing it
 *  drops the user back to the picker. */
type DiffState = {
  projectId: string | null;
  baselineBuildId: string | null;
  targetBuildId: string | null;
  loading: boolean;
  error: string | null;
  report: DiffReport | null;

  setTarget: (args: {
    projectId: string;
    baselineBuildId?: string | null;
    targetBuildId?: string | null;
  }) => void;
  setBaseline: (buildId: string | null) => void;
  setTargetBuild: (buildId: string | null) => void;
  run: () => Promise<void>;
  reset: () => void;
};

export const useDiffStore = create<DiffState>((set, get) => ({
  projectId: null,
  baselineBuildId: null,
  targetBuildId: null,
  loading: false,
  error: null,
  report: null,

  setTarget: ({ projectId, baselineBuildId, targetBuildId }) =>
    set({
      projectId,
      baselineBuildId: baselineBuildId ?? null,
      targetBuildId: targetBuildId ?? null,
      report: null,
      error: null,
    }),

  setBaseline: (buildId) => set({ baselineBuildId: buildId, report: null }),
  setTargetBuild: (buildId) => set({ targetBuildId: buildId, report: null }),

  run: async () => {
    const { projectId, baselineBuildId, targetBuildId } = get();
    if (!projectId || !baselineBuildId || !targetBuildId) {
      set({ error: "Pick a project, baseline, and target first." });
      return;
    }
    set({ loading: true, error: null });
    try {
      const report = await diffRun({
        projectId,
        baselineBuildId,
        targetBuildId,
      });
      set({ report, loading: false });
    } catch (err) {
      set({ error: String(err), loading: false });
    }
  },

  reset: () =>
    set({
      projectId: null,
      baselineBuildId: null,
      targetBuildId: null,
      loading: false,
      error: null,
      report: null,
    }),
}));
