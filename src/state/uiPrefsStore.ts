import { create } from "zustand";
import { persist, createJSONStorage } from "zustand/middleware";

/** External-editor URL scheme used by RightPanel's "Open in editor" action.
 *  `none` hides the button entirely. */
export type EditorProtocol = "vscode" | "idea" | "none";

/** Persisted UI prefs that live across sessions. Anything ephemeral
 *  (search query, selected hit, find-overlay state) belongs in
 *  searchStore or component-local state, not here.
 */
type UIPrefsState = {
  /** Right-panel width as a fraction of the window (0..1). Stored as a
   *  ratio rather than a pixel count so the 50/50 default keeps holding
   *  when the user resizes the window. `null` falls back to 0.5. */
  rightPanelWidthRatio: number | null;
  /** SiblingPane height in CSS px. Driven by drag-resize handle. */
  siblingPaneHeight: number | null;
  /** Default external editor for "Open in editor" button. */
  editorProtocol: EditorProtocol;
  /** Most recently active section chip. Currently dead state - kept so
   *  persisted localStorage doesn't flag a missing field. */
  lastSection: "all" | "source" | "hm_guides" | "javadocs" | "assets";
  /** Whether the user has dismissed the keyboard cheatsheet at least once.
   *  Keeps the `?` modal from re-prompting. */
  cheatsheetSeen: boolean;
  /** Show inline Javadoc boxes inside the source viewer. Default on. */
  inlineJavadocsEnabled: boolean;
  /** Show per-result ranking breakdown (BM25 / vector / RRF) inline in
   *  the search results. Off by default; lives behind Settings ->
   *  Developer so the search UI itself stays free of jargon. */
  showDebug: boolean;

  setRightPanelWidthRatio: (ratio: number | null) => void;
  setSiblingPaneHeight: (px: number | null) => void;
  setEditorProtocol: (p: EditorProtocol) => void;
  setLastSection: (s: UIPrefsState["lastSection"]) => void;
  setCheatsheetSeen: (seen: boolean) => void;
  setInlineJavadocsEnabled: (on: boolean) => void;
  setShowDebug: (on: boolean) => void;
};

export const useUIPrefsStore = create<UIPrefsState>()(
  persist(
    (set) => ({
      rightPanelWidthRatio: null,
      siblingPaneHeight: null,
      editorProtocol: "vscode",
      lastSection: "all",
      cheatsheetSeen: false,
      inlineJavadocsEnabled: true,
      showDebug: false,

      setRightPanelWidthRatio: (ratio) => set({ rightPanelWidthRatio: ratio }),
      setSiblingPaneHeight: (px) => set({ siblingPaneHeight: px }),
      setEditorProtocol: (p) => set({ editorProtocol: p }),
      setLastSection: (s) => set({ lastSection: s }),
      setCheatsheetSeen: (seen) => set({ cheatsheetSeen: seen }),
      setInlineJavadocsEnabled: (on) => set({ inlineJavadocsEnabled: on }),
      setShowDebug: (on) => set({ showDebug: on }),
    }),
    {
      name: "atlas:ui-prefs",
      storage: createJSONStorage(() => localStorage),
      version: 2,
      migrate: (persisted) => {
        // v1 stored `rightPanelWidth` as pixels and `lastCorpus`. Drop
        // both: pixel widths don't survive window-size changes (the
        // bug we're fixing in v2), and `lastCorpus` is dead state.
        if (persisted && typeof persisted === "object") {
          const o = persisted as Record<string, unknown>;
          delete o.rightPanelWidth;
          delete o.lastCorpus;
        }
        return persisted as UIPrefsState;
      },
    },
  ),
);
