import { invoke } from "@tauri-apps/api/core";
import type { Slot } from "@/lib/patcher";

/** Mirrors `IndexerPhase` in src-tauri/src/indexer/status.rs. */
export type IndexerPhase = "walking" | "indexing" | "committing";

/** Mirrors `IndexerStatus`. */
export type IndexerStatus =
  | { kind: "idle" }
  | { kind: "phase"; slot: Slot; phase: IndexerPhase }
  | {
      kind: "progress";
      slot: Slot;
      phase: IndexerPhase;
      current: number;
      total: number;
      /** Chunks embedded + written so far during the `indexing` phase. */
      chunks?: number;
    }
  | { kind: "done"; slot: Slot; docs: number }
  | { kind: "error"; slot: Slot; message: string };

export type SlotIndexSummary = {
  slot: Slot;
  ready: boolean;
  docs: number | null;
  indexed_at: string | null;
  decompile_mtime_at_index: string | null;
  stale: boolean;
};

export type IndexOverview = {
  release: SlotIndexSummary;
  pre_release: SlotIndexSummary;
};

export type ChunkKind = "type" | "method" | "constructor" | "file";

/** Mirrors `HitDebug` in src-tauri/src/indexer/mod.rs. Populated by the
 * hybrid blender so the UI can show why a hit ranked where it did. */
export type HitDebug = {
  bm25_rank: number | null;
  bm25_score: number | null;
  vector_rank: number | null;
  vector_distance: number | null;
  rrf_score: number;
  weight_bm25: number;
  weight_vector: number;
};

export type SearchHit = {
  slot: Slot;
  /** Which section this hit lives in. `source` = decompiled Java,
   * `hm_doc` = Hytale Modding markdown guides, `hypixel_doc` = Hypixel
   * Javadoc-derived API docs, `asset` = assets.zip metadata. The empty
   * string is a defensive fallback for older artifacts that didn't
   * carry the field; treat it as `source`. */
  source_type: string;
  path: string;
  fqn: string;
  package: string;
  filename: string;
  score: number;
  line_count: number;
  /** 1-based line to scroll to in the file viewer: start of the best chunk. */
  preview_line: number | null;
  preview: string | null;
  /** What kind of chunk matched: type = class header/TOC, method = method body. */
  chunk_kind: ChunkKind | "";
  /** Simple symbol name (class or method). Empty when chunk_kind == "file". */
  symbol_name: string;
  start_line: number | null;
  end_line: number | null;
  /** Per-hit ranking breakdown. Always present in dev; may be omitted by
   * older backends, treat as optional. */
  debug: HitDebug | null;
  /** Frontend-only marker set by SearchPage's Javadoc-fold pass when a
   *  source row was synthesized from a Javadoc-only match (the Javadoc
   *  text was the actual scorer, but we surface the source file because
   *  the user wants to read the implementation).. */
  matchedIn?: "javadoc";
  /** Comma-joined author names for HM guide hits, pulled from the doc
   *  frontmatter at index time. Absent on every other section. */
  authors?: string | null;
};

export type SearchResponse = {
  hits: SearchHit[];
  query: string;
  slot: Slot;
  elapsed_ms: number;
};

export async function getIndexOverview(): Promise<IndexOverview> {
  return invoke<IndexOverview>("index_overview");
}

export async function getIndexStatus(): Promise<IndexerStatus> {
  return invoke<IndexerStatus>("index_status");
}

export async function startIndex(slot: Slot): Promise<void> {
  await invoke<void>("index_start", { slot });
}

export async function clearIndex(slot: Slot): Promise<void> {
  await invoke<void>("clear_index", { slot });
}

export async function runSearch(
  slot: Slot,
  query: string,
  limit?: number,
  sourceTypes?: string[],
): Promise<SearchResponse> {
  return invoke<SearchResponse>("search", {
    slot,
    query,
    limit,
    sourceTypes,
  });
}

export async function readSource(
  slot: Slot,
  path: string,
  sourceType: string,
): Promise<string> {
  return invoke<string>("read_source", { slot, path, sourceType });
}

/** Bounded LRU keyed by string. Tiny on purpose: source files for one
 *  search session fit easily, and we'd rather drop old entries than
 *  hold every file the user has ever opened. Maps to a Promise so
 *  concurrent callers (e.g. the viewer fetching while a prefetch is
 *  in flight) share the same network round trip. */
function makeLru<T>(capacity: number) {
  const map = new Map<string, Promise<T>>();
  return {
    get(key: string): Promise<T> | undefined {
      const v = map.get(key);
      if (v === undefined) return undefined;
      // Touch: re-insert to mark as most recently used.
      map.delete(key);
      map.set(key, v);
      return v;
    },
    set(key: string, value: Promise<T>) {
      if (map.has(key)) map.delete(key);
      map.set(key, value);
      while (map.size > capacity) {
        const oldest = map.keys().next().value;
        if (oldest === undefined) break;
        map.delete(oldest);
      }
    },
    drop(key: string) {
      map.delete(key);
    },
    clear() {
      map.clear();
    },
  };
}

const sourceCache = makeLru<string>(24);
const inlineJavadocCache = makeLru<InlineJavadoc[]>(48);

/** Drop every cached source body and inline-Javadoc list. Called after
 *  a fetch completes: a re-mount can change the on-disk Javadoc cache
 *  out from under us, and a previously-cached `[]` result (recorded
 *  before the new mount populated `<indexes>/javadocs/<slot>/`) would
 *  otherwise stick forever for that (slot, fqn). The caches are small
 *  and session-scoped; rebuilding them is cheap. */
export function clearViewerCaches(): void {
  sourceCache.clear();
  inlineJavadocCache.clear();
}

/** Cached form of {@link readSource}. Used by the file viewer and by
 *  the j/k prefetch helper; both share the same in-flight promise so a
 *  prefetch immediately followed by a real open never round-trips
 *  twice. Failed fetches are dropped from the cache so the next try
 *  re-issues. */
export function cachedReadSource(
  slot: Slot,
  path: string,
  sourceType: string,
): Promise<string> {
  const key = `${slot}\x00${path}`;
  const hit = sourceCache.get(key);
  if (hit) return hit;
  const p = readSource(slot, path, sourceType).catch((err) => {
    sourceCache.drop(key);
    throw err;
  });
  sourceCache.set(key, p);
  return p;
}

/** Cached form of {@link getInlineJavadocs}. Same dedupe semantics as
 *  {@link cachedReadSource}. */
export function cachedGetInlineJavadocs(
  slot: Slot,
  classFqn: string,
): Promise<InlineJavadoc[]> {
  const key = `${slot}\x00${classFqn}`;
  const hit = inlineJavadocCache.get(key);
  if (hit) return hit;
  const p = getInlineJavadocs(slot, classFqn).catch((err) => {
    inlineJavadocCache.drop(key);
    throw err;
  });
  inlineJavadocCache.set(key, p);
  return p;
}

/** Fire-and-forget warmup of the source + inline-Javadoc caches for a
 *  hit the user is *likely* to open next (j/k neighbor). Errors are
 *  swallowed: a failed prefetch is invisible to the user, and the real
 *  open will surface the error if it persists. */
export function prefetchHit(hit: SearchHit): void {
  void cachedReadSource(hit.slot, hit.path, hit.source_type).catch(() => {});
  if (hit.source_type === "source" && hit.fqn) {
    void cachedGetInlineJavadocs(hit.slot, hit.fqn).catch(() => {});
  }
}

/** Look up a hit's cross-section pair: source ↔ Javadoc by FQN. Returns
 *  null when no pair exists in the index (internal class with no
 *  public Javadoc, or Javadoc page whose source isn't decompiled). The
 *  pair is class-level: clicking a method hit returns the Javadoc for
 *  the *class*, not the specific method.
 *
 *  HM markdown docs and assets have no source pair, those source_types
 *  always resolve to null. */
export async function findSibling(
  slot: Slot,
  fqn: string,
  sourceType: string,
): Promise<SearchHit | null> {
  return invoke<SearchHit | null>("find_sibling", { slot, fqn, sourceType });
}

/** Bulk variant of {@link findSibling}. Given a list of class FQNs from
 *  Javadoc-only hits, resolves each to its source-code sibling (if any).
 *  Used by the SearchPage post-processing step that folds Javadoc-only
 *  hits into their source rows so the result list never shows a docs
 *  hit when source is available. Missing siblings come back as `null`. */
export async function findSourceSiblings(
  slot: Slot,
  fqns: string[],
): Promise<Record<string, SearchHit | null>> {
  return invoke<Record<string, SearchHit | null>>("find_source_siblings", {
    slot,
    fqns,
  });
}

/** Mirrors `InlineJavadoc` in src-tauri/src/commands.rs. One entry per
 *  inline anchor to splice into the source viewer. */
export type InlineJavadoc = {
  /** 1-based line; the anchor renders BEFORE this line. */
  start_line: number;
  /** "class" for the type-level header (always at line 1) or "method". */
  kind: "class" | "method";
  header: string;
  prose: string;
  deprecated: boolean;
};

/** Resolve every inline Javadoc anchor for a given class FQN. The
 *  backend pairs Hypixel Javadoc method entries against `symbols.sqlite`
 *  source-side line numbers so the viewer can render a Javadoc box
 *  above each documented method declaration. Returns `[]` when no
 *  Javadoc is cached for the class. */
export async function getInlineJavadocs(
  slot: Slot,
  classFqn: string,
): Promise<InlineJavadoc[]> {
  return invoke<InlineJavadoc[]>("get_inline_javadocs", {
    slot,
    classFqn,
  });
}

/** Human label for the indexer phase shown in the card/status bar. */
export function indexPhaseLabel(phase: IndexerPhase): string {
  switch (phase) {
    case "walking":
      return "Scanning files…";
    case "indexing":
      return "Preparing search data…";
    case "committing":
      return "Finishing up…";
  }
}
