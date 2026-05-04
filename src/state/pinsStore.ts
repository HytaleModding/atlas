import { create } from "zustand";

import {
  noteSet as backendNoteSet,
  pinAdd as backendPinAdd,
  pinList as backendPinList,
  pinRemove as backendPinRemove,
  type Pin,
  type PinKind,
} from "@/lib/state";

/** Pins/notes cache. The backend `state.sqlite` is the source of truth;
 *  this store mirrors the latest snapshot so the LeftNav can render
 *  without re-fetching on every paint. Mutations always roundtrip
 *  through Tauri and re-pull the list on success. */
type PinsState = {
  pins: Pin[];
  loading: boolean;
  error: string | null;

  refresh: () => Promise<void>;
  add: (
    kind: PinKind,
    target: string,
    buildId?: string | null,
    label?: string | null,
  ) => Promise<Pin | null>;
  remove: (id: number) => Promise<void>;
  setNote: (pinId: number, body: string) => Promise<void>;
  /** Lookup helper: returns the pin for `(kind, target, build_id)` or
   *  `null`. Used by the file viewer to toggle the Pin button between
   *  "Pin" and "Pinned". `buildId === null` matches build-agnostic pins;
   *  otherwise the comparison is exact. */
  findPin: (
    kind: PinKind,
    target: string,
    buildId: string | null,
  ) => Pin | null;
};

export const usePinsStore = create<PinsState>((set, get) => ({
  pins: [],
  loading: false,
  error: null,

  refresh: async () => {
    set({ loading: true, error: null });
    try {
      const pins = await backendPinList();
      set({ pins, loading: false });
    } catch (err) {
      set({ error: String(err), loading: false });
    }
  },

  add: async (kind, target, buildId = null, label = null) => {
    try {
      const pin = await backendPinAdd(kind, target, buildId, label);
      // Re-pull rather than splicing locally - the backend is idempotent
      // and may return an existing row, so a full refresh keeps the
      // store consistent with disk.
      await get().refresh();
      return pin;
    } catch (err) {
      set({ error: String(err) });
      return null;
    }
  },

  remove: async (id) => {
    try {
      await backendPinRemove(id);
      await get().refresh();
    } catch (err) {
      set({ error: String(err) });
    }
  },

  setNote: async (pinId, body) => {
    try {
      await backendNoteSet(pinId, body);
      await get().refresh();
    } catch (err) {
      set({ error: String(err) });
    }
  },

  findPin: (kind, target, buildId) => {
    return (
      get().pins.find(
        (p) =>
          p.kind === kind &&
          p.target === target &&
          (p.build_id ?? null) === buildId,
      ) ?? null
    );
  },
}));
