import { invoke } from "@tauri-apps/api/core";

/** Pin categories. Mirrors `PinKind` in `src-tauri/src/state/mod.rs`.
 *  Kept as a string union (not an enum) so the serde-tagged value
 *  passes through Tauri without a manual conversion. */
export type PinKind = "file" | "query" | "symbol";

export type Pin = {
  id: number;
  kind: PinKind;
  /** File path / query string / symbol FQN, depending on `kind`. */
  target: string;
  /** Optional build context. `null` means the pin is build-agnostic. */
  build_id: string | null;
  /** Friendly label shown in the LeftNav. Falls back to `target`. */
  label: string | null;
  created_at: string;
  /** Eager-joined from `notes`. `null` when no note has been saved. */
  note: string | null;
};

export type RecentFile = {
  path: string;
  build_id: string;
  opened_at: string;
};

export async function pinAdd(
  kind: PinKind,
  target: string,
  buildId: string | null = null,
  label: string | null = null,
): Promise<Pin> {
  return invoke<Pin>("state_pin_add", {
    kind,
    target,
    buildId,
    label,
  });
}

export async function pinRemove(id: number): Promise<void> {
  await invoke<void>("state_pin_remove", { id });
}

export async function pinList(): Promise<Pin[]> {
  return invoke<Pin[]>("state_pin_list");
}

export async function noteSet(pinId: number, body: string): Promise<void> {
  await invoke<void>("state_note_set", { pinId, body });
}

export async function noteGet(pinId: number): Promise<string | null> {
  return invoke<string | null>("state_note_get", { pinId });
}

export async function recordRecentFile(
  path: string,
  buildId: string,
): Promise<void> {
  await invoke<void>("state_recent_file_record", { path, buildId });
}

export async function listRecentFiles(): Promise<RecentFile[]> {
  return invoke<RecentFile[]>("state_recent_files");
}
