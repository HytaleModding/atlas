import { invoke } from "@tauri-apps/api/core";

/** Mirrors `commands::ProjectListEntry`. The frontend gets all paths as
 *  strings (no PathBuf). `index_ready` is a derived boolean that flips
 *  to true once `atlas-meta.json` exists in the project's index dir. */
export type ProjectListEntry = {
  id: string;
  name: string;
  source_path: string;
  created_at: string;
  last_indexed_at: string | null;
  index_ready: boolean;
};

/** Project indexing progress events. Mirrors `IndexEvent` in
 *  `indexer/mod.rs`, with the project id added by `ProjectSink`. */
export type ProjectPhasePayload = { project_id: string; phase: string };
export type ProjectProgressPayload = {
  project_id: string;
  current: number;
  total: number;
  chunks: number;
};
export type ProjectDonePayload = { project_id: string; docs: number };
export type ProjectErrorPayload = { project_id: string; message: string };

export function projectRegister(
  path: string,
  name?: string,
): Promise<string> {
  return invoke<string>("project_register", { path, name });
}

export function projectList(): Promise<ProjectListEntry[]> {
  return invoke<ProjectListEntry[]>("project_list");
}

export function projectUnregister(id: string): Promise<void> {
  return invoke<void>("project_unregister", { id });
}

export function projectRemoveIndex(id: string): Promise<void> {
  return invoke<void>("project_remove_index", { id });
}

/** Kicks off a project index run; returns immediately. Subscribe to
 *  `project:phase` / `project:progress` / `project:done` / `project:error`
 *  to follow progress. */
export function projectIndex(id: string): Promise<void> {
  return invoke<void>("project_index", { id });
}
