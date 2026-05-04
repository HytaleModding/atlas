import { invoke } from "@tauri-apps/api/core";
import type { Slot } from "@/lib/patcher";

/** Mirrors `AtlasConfig` in src-tauri/src/config.rs. */
export type AtlasConfig = {
  hytale_release_path: string | null;
  hytale_prerelease_path: string | null;
  first_run_skipped: boolean;
  active_branch: Slot;
  /** `owner/name` of the GitHub repo hosting reference-data releases. */
  central_repo: string;
  /** Build id the user picked as active for the release patchline. */
  active_release_build: string | null;
  /** Build id the user picked as active for the pre-release patchline. */
  active_pre_release_build: string | null;
};

export type ConfigSnapshot = {
  config: AtlasConfig;
  default_release_candidate: string | null;
  default_prerelease_candidate: string | null;
  detected_release_path: string | null;
  detected_prerelease_path: string | null;
};

export type HytalePathCheck = {
  path: string;
  valid: boolean;
  reason: string | null;
};

export async function loadConfig(): Promise<ConfigSnapshot> {
  return invoke<ConfigSnapshot>("load_config");
}

export async function saveConfig(config: AtlasConfig): Promise<void> {
  await invoke<void>("save_config", { config });
}

export async function validateHytalePath(path: string): Promise<HytalePathCheck> {
  return invoke<HytalePathCheck>("validate_hytale_path", { path });
}

/** True when the first-run modal should block the app. */
export function needsFirstRun(snapshot: ConfigSnapshot): boolean {
  return (
    snapshot.config.hytale_release_path === null &&
    !snapshot.config.first_run_skipped
  );
}
