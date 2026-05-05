import { invoke } from "@tauri-apps/api/core";
import type { Slot } from "@/lib/patcher";

/**
 * Dev-only fallback URLs. Production builds resolve through the central
 * GitHub Releases repo (`index_resolve_remote`), so this map only carries
 * the localhost staging URLs that let a dev iterate against a
 * `python -m http.server 8000` without touching CI. The Settings →
 * "Reference data source" dropdown gates which path the UI takes.
 */
export const DEV_FETCH_URL_BY_SLOT: Record<Slot, FetchRequest> = import.meta.env
  .DEV
  ? {
      release: {
        buildId: "release-test",
        url: "http://localhost:8000/atlas-test.tar.zst",
      },
      "pre-release": {
        buildId: "pre-release-test",
        url: "http://localhost:8000/atlas-test-pre.tar.zst",
      },
    }
  : ({} as Record<Slot, FetchRequest>);

/** Mirrors `FetchPhase` in src-tauri/src/fetcher/status.rs. */
export type FetchPhase =
  | "resolving"
  | "downloading"
  | "verifying"
  | "extracting"
  | "mounting";

/** Mirrors `FetchStatus`: kebab-case tagged enum serialized via serde. */
export type FetchStatus =
  | { kind: "idle" }
  | { kind: "phase"; buildId: string; phase: FetchPhase }
  | {
      kind: "downloading";
      buildId: string;
      received: number;
      total: number | null;
    }
  | {
      kind: "extracting";
      buildId: string;
      current: number;
      total: number;
    }
  | { kind: "done"; buildId: string }
  | { kind: "error"; buildId: string; message: string };

/** Mirrors `fetcher::manifest::Manifest`: the full compound version key
 *  the client needs to decide whether to trust an artifact. */
export type Manifest = {
  build_id: string;
  hytale_impl_version: string;
  hytale_patchline: string | null;
  vineflower_version: string;
  chunker_version: string;
  schema_version: number;
  embedder_id: string;
  embedder_dim: number;
  min_client_version: string;
  signing_pubkey_fingerprint: string;
  created_at: string;
  sha256sums_sha256: string;
};

/** One row in the Index Catalog. */
export type MountedIndexEntry = {
  build_id: string;
  path: string;
  manifest: Manifest;
  size_bytes: number;
};

export type FetchRequest = {
  buildId: string;
  url: string;
};

export async function startFetch(request: FetchRequest): Promise<void> {
  // Serde expects snake_case `build_id`; convert at the IPC boundary.
  await invoke<void>("index_fetch", {
    request: { build_id: request.buildId, url: request.url },
  });
}

/** Mount a `.tar.zst` artifact already on disk. The Rust side reuses
 *  the same status + events as `index_fetch`, so the UI doesn't need a
 *  parallel state machine; it just sees a fetch that skips Downloading. */
export async function mountLocal(tarballPath: string): Promise<void> {
  await invoke<void>("index_mount_local", { tarballPath });
}

export async function getFetchStatus(): Promise<FetchStatus> {
  const raw = await invoke<RawFetchStatus>("index_fetch_status");
  return normalizeFetchStatus(raw);
}

export async function listMountedIndexes(): Promise<MountedIndexEntry[]> {
  return invoke<MountedIndexEntry[]>("index_catalog");
}

/** What `index_resolve_remote` returns. Mirrors `RemoteBuildResolution`
 *  in commands.rs. `null` means the central repo has no published build
 *  for the requested patchline yet. */
export type RemoteBuildResolution = {
  build_id: string;
  url: string;
  release_tag: string;
  hytale_impl_version: string | null;
};

/** Ask the central repo what the latest published build is for a
 *  patchline. Returns `null` when the central repo hasn't published one
 *  yet (the catalog UX surfaces this as "no update available"). */
export async function resolveRemoteBuild(
  patchline: Slot,
): Promise<RemoteBuildResolution | null> {
  const raw = await invoke<RemoteBuildResolution | null>(
    "index_resolve_remote",
    { patchline },
  );
  return raw;
}

/** Delete a mounted build. The backend refuses if it's the only build
 *  mounted for its patchline. Throws with the backend's error string on
 *  refusal so the caller can surface it in a toast. */
export async function removeIndex(buildId: string): Promise<void> {
  await invoke<void>("index_remove", { buildId });
}

/** Pick which mounted build search uses for a patchline. */
export async function setActiveIndex(
  patchline: Slot,
  buildId: string,
): Promise<void> {
  await invoke<void>("index_set_active", { patchline, buildId });
}

/** Human-readable label for the download phase shown in the UI. */
export function fetchPhaseLabel(phase: FetchPhase): string {
  switch (phase) {
    case "resolving":
      return "Looking up the latest version…";
    case "downloading":
      return "Downloading…";
    case "verifying":
      return "Verifying download…";
    case "extracting":
      return "Unpacking…";
    case "mounting":
      return "Finishing up…";
  }
}

/** Internal: the serde shape before the camelCase normalization. */
type RawFetchStatus =
  | { kind: "idle" }
  | { kind: "phase"; build_id: string; phase: FetchPhase }
  | {
      kind: "downloading";
      build_id: string;
      received: number;
      total: number | null;
    }
  | { kind: "extracting"; build_id: string; current: number; total: number }
  | { kind: "done"; build_id: string }
  | { kind: "error"; build_id: string; message: string };

/** Normalizes the serde snake_case payload to the camelCase shape used
 *  in TypeScript. Kept here so the store + components only deal with one
 *  convention. */
export function normalizeFetchStatus(raw: RawFetchStatus): FetchStatus {
  switch (raw.kind) {
    case "idle":
      return { kind: "idle" };
    case "phase":
      return { kind: "phase", buildId: raw.build_id, phase: raw.phase };
    case "downloading":
      return {
        kind: "downloading",
        buildId: raw.build_id,
        received: raw.received,
        total: raw.total,
      };
    case "extracting":
      return {
        kind: "extracting",
        buildId: raw.build_id,
        current: raw.current,
        total: raw.total,
      };
    case "done":
      return { kind: "done", buildId: raw.build_id };
    case "error":
      return { kind: "error", buildId: raw.build_id, message: raw.message };
  }
}

/** Format `size_bytes` in IEC units for the catalog row. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}
