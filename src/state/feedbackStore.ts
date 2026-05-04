import { create } from "zustand";
import type { FeedbackKind } from "@/lib/feedback";

/** Tiny store for the feedback modal: open/close + which tab is active.
 *  All form state lives inside the modal component itself (uncontrolled
 *  fields + local useState) so a stale draft doesn't survive a remount. */
type FeedbackState = {
  open: boolean;
  mode: FeedbackKind;
  show: (mode?: FeedbackKind) => void;
  hide: () => void;
  setMode: (mode: FeedbackKind) => void;
};

export const useFeedbackStore = create<FeedbackState>((set) => ({
  open: false,
  mode: "tuning",
  show: (mode) => set({ open: true, mode: mode ?? "tuning" }),
  hide: () => set({ open: false }),
  setMode: (mode) => set({ mode }),
}));
