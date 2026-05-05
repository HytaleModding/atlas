import { invoke } from "@tauri-apps/api/core";

/** Mirrors `Slot` in src-tauri/src/config.rs. */
export type Slot = "release" | "pre-release";

/** Mirrors `PatcherPhase` in src-tauri/src/patcher/status.rs. */
export type PatcherPhase =
  | "ensuring-vineflower"
  | "downloading-vineflower"
  | "detecting-java"
  | "extracting"
  | "decompiling";

/** Mirrors `PatcherStatus` in src-tauri/src/patcher/status.rs. */
export type PatcherStatus =
  | { kind: "idle" }
  | { kind: "phase"; phase: PatcherPhase }
  | { kind: "downloading"; received: number; total: number | null }
  | { kind: "extracting"; current: number; total: number }
  | { kind: "done"; output_dir: string }
  | { kind: "error"; message: string };

export type DecompileOverview = {
  output_dir: string;
  decompiled_at: string;
  jar_mtime_at_decompile: string;
  hytale_version: string | null;
  fresh: boolean;
};

export type SlotOverview = {
  slot: Slot;
  configured: boolean;
  install_path: string | null;
  default_path: string | null;
  jar_path: string | null;
  jar_exists: boolean;
  jar_size: number | null;
  jar_mtime: string | null;
  hytale_version: string | null;
  decompile: DecompileOverview | null;
  output_dir: string;
};

export type IdeId =
  | "explorer"
  | "vs-code"
  | "vs-code-insiders"
  | "intellij-community"
  | "intellij-ultimate";

export type DetectedIde = {
  id: IdeId;
  display_name: string;
  executable: string;
};

export type PatcherOverview = {
  release: SlotOverview;
  pre_release: SlotOverview;
  ides: DetectedIde[];
};

export function getPatcherOverview(): Promise<PatcherOverview> {
  return invoke<PatcherOverview>("patcher_overview");
}

export function startDecompile(slot: Slot): Promise<string> {
  return invoke<string>("start_decompile", { slot });
}

export function getPatcherStatus(): Promise<PatcherStatus> {
  return invoke<PatcherStatus>("patcher_status");
}

export function clearDecompile(slot: Slot): Promise<void> {
  return invoke<void>("clear_decompile", { slot });
}

export function openInIde(ideId: IdeId, path: string): Promise<void> {
  return invoke<void>("open_in_ide", { ideId, path });
}

/** Human-readable label for a phase, used in the progress UI. */
export function phaseLabel(phase: PatcherPhase): string {
  switch (phase) {
    case "ensuring-vineflower":
      return "Preparing decompiler…";
    case "downloading-vineflower":
      return "Downloading Vineflower…";
    case "detecting-java":
      return "Detecting Java runtime…";
    case "extracting":
      return "Extracting server JAR…";
    case "decompiling":
      return "Decompiling sources…";
  }
}

/** Short human label for a slot. */
export function slotLabel(slot: Slot): string {
  return slot === "release" ? "Release" : "Pre-release";
}

/** ISO-8601 → short display like "Apr 20". */
export function formatShortDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
