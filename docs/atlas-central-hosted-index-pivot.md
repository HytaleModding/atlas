# Atlas - Central-Hosted Index Pivot

## Context

Atlas (D:\CodeProjects\Atlas) is a Tauri 2 + Rust + React/TS desktop tool for Hytale modders. Through Phase 2 M3 it was architected as a fully local tool: the user installs it, decompiles the Hytale server JAR on their machine, builds a Tantivy + LanceDB hybrid index locally, and searches over the decompiled source.

Two things prompted this pivot:

1. **V1 scope now spans four data sources** - Hytale server source + HM docs (public GitHub repo) + Hypixel docs + assets.zip - and individual users cannot aggregate these themselves. Central indexing is the only realistic path.
2. **Hytale Modding has offered to host.** The admins will run the central build pipeline and own artifact storage (GitHub Releases to start). This is a committed offer, not speculation.

Plus two tactical wins we get for free:
- The 7.5 GB peak-RAM problem we hit during local indexing goes away on the client - heavy indexing moves to CI infrastructure.
- Search quality becomes team-maintained. HM can iterate chunking, embedding, scraping, and all users benefit via an "Update index" action.

**The outcome we're targeting:** modders install Atlas → it finds their Hytale install → downloads the signed, versioned index artifact matching their build → searches across source + docs + assets from day zero. The client no longer does Vineflower, embedding, or bulk indexing unless the user opts into "index my own mod project." MCP endpoints are live from v1 on both the client and the central service so agents can use Atlas as a tool without running Atlas locally.

Full V1 feature pitch is memorialised in `C:\Users\matth\.claude\projects\D--CodeProjects\memory\project_atlas_vision.md`.

**Decisions locked in this round (recorded here so the plan is self-contained):**
- Artifact ships **decompiled source** alongside the index. Client skips local Vineflower by default.
- **Ed25519 signing** from v1; pubkey embedded in Atlas binary; client refuses to mount unsigned artifacts.
- **Multi-index catalog** generalized now; `SearchCatalog` moves from the two-slot enum to keyed-by-ID.

---

## Target architecture (end-state)

**Central (Hytale Modding-operated, GitHub Actions + GitHub Releases):**
- Fetches server JAR + HM docs repo + Hypixel docs + assets.zip for a given Hytale build
- Decompiles (pinned Vineflower + JDK version)
- Chunks (tree-sitter for Java, pass-through for docs/assets)
- Embeds chunks with BGE-small (384-dim L2-normalized)
- Builds Tantivy index + Lance vector store + symbols.sqlite
- Packages everything + decompiled source as `.tar.zst` with signed manifest
- Uploads to GitHub Releases keyed by compound version

**Shared Rust workspace - one router, three main.rs wrappers:**
- `atlas-desktop` (Tauri app, current entrypoint)
- `atlas-build` (central CI binary; zero UI)
- `atlas-serve` (future hosted MCP/HTTP service for agents)

All three share: `SearchCatalog`, MCP tool definitions, `router()` from `http.rs:31`. This is load-bearing - if local MCP and hosted MCP can diverge silently, every downstream agent config breaks.

**Client (Atlas desktop, per user):**
- Detects Hytale install + build number (`patcher/version.rs:22-30` already extracts `HytaleVersion`)
- Checks catalog for a matching artifact; shows banner if newer is available
- Downloads (with HTTP Range resume), verifies signature + SHA256, atomically extracts to `<data_dir>/indexes/<build_id>/`
- Runs hybrid search locally against the mounted artifact - the hybrid engine built in Phase 2 M3 is unchanged
- Embeds the user's query at query-time only (batch=1, short text; trivial RAM)
- Local decompile + local indexing remain available as the "index my own project" path for Phase 5

---

## Pre-phase: Dev unblock

**Goal:** unblock real-time testing of what's already built. Not architectural; do it first because every Phase 3 task benefits.

- Add `[profile.dev.package."*"]` with `opt-level = 3` (and inherits-from strategy for our own crate) to `src-tauri/Cargo.toml`. Our crate stays in debug (fast incremental), all deps (fastembed, ort, tokenizers, lance, arrow, datafusion, tantivy) build at -O3.
- Expected: current "~12s/file" debug-mode indexing → sub-second per file, full re-index under a few minutes on a typical dev box.
- First rebuild after this change will be slow (one-time recompile of the dep graph); subsequent dev-loop rebuilds stay fast because our code isn't affected.

---

## Phase 3 - Pivot foundation

### Phase 3 Spike (gates the rest): decompile determinism

Before any code in 3.D or 3.F lands, produce a short decision doc in `docs/` pinning:
- Vineflower version (currently `patcher/vineflower.rs` ensures it; extract the exact version)
- JDK toolchain version the builder will run under
- JVM flags / Vineflower flags used
- Hash-comparison proof: decompile the same JAR twice on two different machines with the pinned toolchain, assert trees are identical

Because we're shipping the decompile inside the artifact (decision), the determinism question is less life-and-death than in the "replay recipe on client" world - but we still want the central builder to produce a stable, inspectable output. The decision doc goes in `docs/decompile-determinism.md` and the pinned versions propagate into the build CLI (3.F) and the manifest (3.D).

### Phase 3.A - Symbol extraction + SQLite sidecar

**Why first:** 3.D cannot finalize the artifact format without knowing the sidecar's shape, and 5.C (diff tracker) needs range queries over symbols (what public methods in package X changed between builds Y and Z) which is painful in JSON. SQLite with an index on `(fqn, signature, build_id)` solves this for ~20 MB extra artifact size.

**Changes:**
- Extend `src-tauri/src/indexer/chunker.rs` (currently emits chunk text + kind + symbol name) to also emit: fully-qualified name, method signature (param types → return type), access modifiers, thrown types, declaring-class FQN.
- New module `src-tauri/src/indexer/symbols.rs` owns `symbols.sqlite` creation + writing during the indexing pass. Tables: `classes`, `methods`, `fields`, with FTS5 on `signature` for fuzzy diff queries.
- During index build, `build_index` (currently in `indexer/mod.rs`) writes to a third sink alongside Tantivy + Lance.

**Reuses:** the chunker already does tree-sitter parsing and walks class/method nodes - we're adding fields, not a new pass. `metadata.rs` ISO-8601 helper patterns carry over for `created_at` columns.

### Phase 3.B - Schema extensions

**Schema fields added to both Tantivy and Lance (Phase 3 populates only `source_type = "source"`; Phase 4 populates the rest without a breaking change):**
- `source_type`: `"source" | "hm_doc" | "hypixel_doc" | "asset"` - the discriminator that lets Phase 4 add sources without a new artifact format.

**`IndexMetadata` (`src-tauri/src/indexer/metadata.rs:18-26`) extended with compound version key:**
```
hytale_impl_version: String         // "2026.03.26-89796e57b"
hytale_patchline: Option<String>    // "release" | "pre-release"
vineflower_version: String
chunker_version: String             // semver, bumped whenever chunking logic changes
embedder_id: String                 // "bge-small-en-v1.5" | "bge-small-en-v1.5-q"
embedder_dim: u32                   // 384
schema_version: u32                 // artifact format version, Atlas-defined
min_client_version: String          // earliest Atlas client that can mount this
created_at: String                  // ISO-8601
signing_pubkey_fingerprint: String  // hex-encoded first 16 bytes of the signing key
```

**Why compound:** "schema version" alone papers over real incompatibility dimensions. Chunker changes more often than the tarball layout; embedder swaps are rarer but catastrophic if mixed; the client needs to know all of them to make a safe mount decision.

### Phase 3.C - `SearchCatalog` generalization

**Current (`indexer/mod.rs:498-507`):** `SearchCatalog` holds `release: Option<Arc<OpenedIndex>>` and `pre_release: Option<Arc<OpenedIndex>>`. Hardcoded to two slots.

**New:** `SearchCatalog` holds `HashMap<IndexId, Arc<OpenedIndex>>` where `IndexId` is a newtype over `String` derived from the manifest (e.g., `release-2026.03.26-89796e57b` or `user-project-<uuid>`). The catalog learns to:
- Enumerate mounted indexes (powers the Index Catalog UX in 3.H)
- Open on-demand given a path - no change in spirit, just keyed
- Invalidate per-ID

**Callers to migrate:**
- `commands.rs:302` `search` - currently takes `Slot`, will take `IndexId` or a convenience wrapper
- `search/hybrid.rs` - takes slot through; swap to IndexId
- The `Slot` enum (`config.rs:16-36`) stays for user-facing "which Hytale branch am I on" UI state, but it's decoupled from what the search engine addresses

### Phase 3.D - Artifact format

**Format:** `atlas-index-<build_id>.tar.zst`, where `build_id = <patchline>-<impl_version_short_sha>`.

**Layout inside:**
```
manifest.json          # The compound version key + fingerprint + file hashes
manifest.json.sig      # Ed25519 detached signature of manifest.json (see 3.E)
tantivy/               # Tantivy segment files, ready to mmap
lance/                 # Lance table directories
symbols.sqlite         # Compact FTS5 symbol index (3.A)
decompile/             # Full decompiled source tree (decision: shipped in artifact)
SHA256SUMS             # Per-file hashes; cross-checked during extract
```

**Compression:** zstd level 19 long-form. Decompile text compresses well (~6-8×); total artifact estimated 500-800 MB for a typical build.

**Manifest file hashes:** every file inside the tarball is hashed in `SHA256SUMS` and the signature covers `manifest.json` which covers the `SHA256SUMS` hash. Tampering anywhere fails signature verification.

### Phase 3.E - Ed25519 signing

- Generate HM signing keypair out-of-band; private key lives in HM's CI secrets, public key is committed to the repo in `src-tauri/signing/atlas-pubkey.hex`.
- `atlas-build` (3.F) signs `manifest.json` after writing it.
- Client (3.G) verifies with `ed25519-dalek` before any extraction proceeds. A missing or bad signature is a hard failure with a specific error shown to the user - not a warning.
- Key rotation path: new pubkey shipped in a new Atlas release; old clients can't mount artifacts signed by the new key until they update. This is a feature (graceful upgrade pressure), not a bug.

### Phase 3.F - Build-side CLI (`atlas-build`)

**Structure:** new bin target in the same workspace. Thin `main.rs` that orchestrates the existing modules:
- `patcher::decompile` → produce decompiled source tree
- `indexer::chunker` + `indexer::build_index` → build Tantivy + Lance + symbols.sqlite
- (Phase 4 adds) `sources::hm_docs`, `sources::hypixel_docs`, `sources::assets` → ingest other sources
- Package → tar + zstd + write manifest + sign + upload

**CI integration:**
- GitHub Actions workflow triggered on **release tag** or **manual dispatch** - explicitly NOT on push (build is ~20-30 min of CPU)
- Workflow lives in the repo initially, HM can fork/migrate later
- Uploads artifact to GitHub Releases under a predictable naming scheme

**Shared with desktop:** this is not a separate project. Same Cargo workspace, same `SearchCatalog` (used for validation round-trips), same chunker, same everything. `atlas-build` is just a different `main.rs`.

### Phase 3.G - Client fetch flow (`index_fetch` Tauri command)

**Shape - exactly mirrors the existing `PatcherStatus` pattern at `patcher/status.rs:34-55`:**

New `FetchStatus` enum in `src-tauri/src/fetcher/status.rs` (new module):
```
Idle
Phase { build_id, phase: FetchPhase }   // Resolving | Downloading | Verifying | Extracting | Mounting
Downloading { build_id, received, total }
Extracting { build_id, current, total }
Done { build_id }
Error { build_id, message }
```

New events: `fetch:phase`, `fetch:progress`, `fetch:done`, `fetch:error`. Same slot-tagging convention. Frontend Zustand store (`fetchStore.ts`) mirrors the enum - follows the exact pattern of `src/state/indexStore.ts:82-154`.

**Download pipeline:**
1. Resolve: hit the GH Releases API, find the best artifact for the client's current Hytale `implementation_version`. Return metadata without downloading yet.
2. Download: `reqwest` with HTTP Range resume. Partial download lives at `<data_dir>/indexes/.tmp/<build_id>.tar.zst.partial`. Resume on retry by checking partial-hash against manifest on reconnect.
3. Verify: Ed25519 signature on `manifest.json`, then SHA256 on each file as extraction proceeds.
4. Extract: stream-extract to `<data_dir>/indexes/.tmp/<build_id>/`. Never into the final path - avoids half-extracted mounts.
5. Mount: atomic rename `.tmp/<build_id>` → `<build_id>`, then write `<build_id>/.ok` marker. `SearchCatalog` only mounts directories with `.ok` present.

**Crash safety:** on startup, the client scans `<data_dir>/indexes/` for directories without `.ok` and treats them as cleanup targets (or resumable if the partial `.tar.zst` is still present).

### Phase 3.H - Index Catalog UX

**Not a banner - a catalog.** Phase 5 diff tracking needs the user to keep an older build mounted while a new pre-release is being compared against it. Single-banner "update now" semantics break that.

**UI:**
- New page/tab "Index Catalog" (or a panel in an existing view) shows a list:
  - Each row is a mounted build: build_id, patchline, created_at, size on disk, chunk count
  - One row is the *active* build (the one search targets by default)
  - Actions per row: set active, delete, export
- "Update available" state: when the resolver finds a newer build than the active one, surface a non-invasive card in the catalog *and* a subtle indicator in the existing `StatusBar` (`src/components/StatusBar.tsx`). User clicks → fetch → option to set new build active or keep current.

**Reuses:**
- `sonner` (already in `package.json`) for toast feedback on fetch completion
- `BranchCard`'s phased-status UI pattern as a template for how each row renders state
- Existing `invoke`-wrapper convention in `src/lib/`; new `src/lib/fetcher.ts`

### Phase 3.I - MCP contract + local implementation

**Delivered in three artifacts, not one:**

**3.I.1 - Tool contract specification** (`docs/mcp-contract.md`, versioned):
- Tool list: `search`, `get_source`, `get_doc`, `get_asset`, `find_symbol`
- Per-tool input JSON Schema (params: query, source_type filter, limit, build_id override, etc.)
- Per-tool output JSON Schema (hit shape, content shape, pagination cursors)
- Error taxonomy: `IndexNotMounted`, `ArtifactVersionMismatch`, `SymbolNotFound`, `InvalidQuery`, `RateLimited`
- Versioning policy: MCP surface version separate from artifact schema version; contract changes follow semver

This is the most important deliverable of Phase 3I because every downstream agent (Claude Code, Cursor, internal tooling) embeds these shapes.

**3.I.2 - Local MCP implementation:**
- Add `rmcp` dependency
- Mount at `http.rs:31` alongside existing `/healthz`
- Tools wired to real `SearchCatalog` + source reader + docs/assets readers (the latter empty until Phase 4, but the tool surface exists and returns `SourceTypeNotIndexed`)

**3.I.3 - Contract tests:**
- Test suite in `src-tauri/tests/mcp_contract.rs` that validates every tool's input/output against the schemas in `docs/mcp-contract.md`
- Runs against the local MCP mount; when hosted MCP (Phase 8) lands, same suite runs against both to prove they're interchangeable

**Hosted MCP (deferred to a later phase):** when HM stands up `atlas-serve`, it reuses the same `router()` + tool definitions. Contract tests prove the two implementations match.

---

## Phase 4 - Expanded sources (V1 unified search)

Schema and artifact format already accept `source_type` from Phase 3, so this phase is additive to the build pipeline, not a rewrite.

- HM docs ingestion: clone the public GitHub repo at build time, chunk per page (default; refine based on page length distribution), index with `source_type = "hm_doc"`.
- Hypixel docs ingestion: fetch from the official docs site (API, RSS, or scrape depending on what HM provides access to). Same pattern.
- Assets.zip ingestion: per-file chunking for JSON assets, preserve top-level keys as searchable metadata. Exact chunking strategy is a sub-decision during Phase 4 - likely per-file for small files, per-top-level-key for larger ones.
- UI: source-type filter chips in search, distinct rendering per type (code panel for source, markdown for docs, JSON tree for assets).
- Degradation: reuse the pattern at `commands.rs:302` (Lance missing → keyword-only). If a source type isn't present in a given artifact, search reports partial availability instead of erroring.

---

## Phase 5 - Project features (V1)

**5.A - Favorites, pins, notes, search history.** First SQLite in the project. Lives at `<data_dir>/state.sqlite`. Schema kept minimal and additive; `plan.md` already anticipates this DB arriving for the tracker.

**5.B - Dynamic display.** Click a search hit → embed that hit's content as the next query vector → re-run hybrid search with the original keyword text as the BM25 side + the hit's vector as the semantic side. Infrastructure already supports this (`hybrid::run` takes a query string; we introduce `hybrid::run_from_anchor(hit)` that skips the query embedder and uses the hit's vector directly).

**5.C - Pre-release project-aware diff tracking.**
- User configures a mod project path; we walk their `.java` files with the same chunker
- Extract their API references (imports + qualified method calls) via tree-sitter
- Resolve against `symbols.sqlite` of the currently-active release index - this is the big payoff from Phase 3.A choosing SQLite over JSON
- When a new pre-release artifact arrives, diff: which of the user's resolved refs are gone / renamed / signature-changed?
- Report UI: grouped by severity (removed, signature-changed, deprecated), with "jump to call site" actions

Requires `SearchCatalog` generalized from Phase 3.C (project index is just another mounted index).

---

## Phase 6 - Polish & performance

- 50-query search-quality regression benchmark (matches `plan.md` existing target)
- Swap query-time embedder to BGE-small-Q - triggers a new `embedder_id`, client compatibility check already in place from 3.B; older artifacts still mountable so long as embedder IDs match
- Fetch-flow test coverage: network mocks, corrupted artifact, signature-mismatch, version-mismatch, resume from partial
- Log unifier (V2 feature, flagged here in case the value case pulls it earlier)

---

## Critical files (and why they matter)

- `src-tauri/Cargo.toml` - dev-profile opt-level change (pre-phase); new `[[bin]]` entry for `atlas-build` (3.F)
- `src-tauri/src/indexer/chunker.rs` - symbol extraction upgrade (3.A)
- `src-tauri/src/indexer/symbols.rs` *(new)* - SQLite writer (3.A)
- `src-tauri/src/indexer/schema.rs` - add `source_type` field (3.B)
- `src-tauri/src/indexer/metadata.rs:18-26` - extend `IndexMetadata` with compound key (3.B)
- `src-tauri/src/indexer/mod.rs:498-507` - `SearchCatalog` generalization (3.C)
- `src-tauri/src/fetcher/mod.rs` + `fetcher/status.rs` *(new)* - artifact fetch pipeline (3.G); **clone the shape of `patcher/status.rs:12-55` exactly**
- `src-tauri/src/fetcher/manifest.rs` *(new)* - manifest serde + signature verify (3.D, 3.E)
- `src-tauri/src/http.rs:31` - mount MCP router alongside `/healthz` (3.I)
- `src-tauri/src/mcp/` *(new module)* - tool definitions shared across desktop + build + serve
- `src-tauri/signing/atlas-pubkey.hex` *(new)* - embedded Ed25519 pubkey (3.E)
- `src/state/fetchStore.ts` *(new)* - mirrors `indexStore.ts:82-154`
- `src/lib/fetcher.ts` *(new)* - invoke wrappers, follows `src/lib/indexer.ts` conventions
- `src/pages/IndexCatalog.tsx` *(new)* - multi-build mount UI (3.H)
- `docs/decompile-determinism.md` *(new)* - decision doc (Phase 3 Spike)
- `docs/mcp-contract.md` *(new)* - versioned tool surface spec (3.I.1)
- `docs/plan.md` - update to reflect pivoted architecture; current plan predates this pivot

## Reused patterns (don't reinvent)

- **Status enum + Arc<Mutex<>> + kebab-case tagged serde** - `patcher/status.rs:12-55`, `indexer/status.rs:9-53`. Copy for `FetchStatus`.
- **`PatcherStatus::Downloading { received, total: Option<u64> }`** - literally the shape we need for artifact downloads (3.G).
- **`<dir>/atlas-meta.json` as a readiness marker** - `indexer/metadata.rs`. The `.ok` marker in 3.G is the same idea.
- **Slot-tagged event multiplexing** - both patcher and indexer emit events tagged with slot so the UI can render per-branch progress. Same pattern for build-id-tagged fetch events.
- **Graceful-degradation search** - `commands.rs:302` falls back to keyword-only when Lance is missing. Reuse this pattern when a source type isn't indexed yet.
- **`router()` extracted for shared use** - `http.rs:31`. Same router becomes the MCP mount point and gets reused by `atlas-serve`.
- **Thin `invoke<T>` wrappers in `src/lib/`** - direct Tauri IPC, no middleware. Keep this convention for fetcher + catalog.

## Open design decisions (flagged defaults; revisit in execution)

- **Artifact naming on GH Releases**: `atlas-index-<patchline>-<impl_version>-<created_at>.tar.zst`. Confirm with HM before the first release is cut.
- **Assets.zip chunking strategy**: default to per-file for files < 32 KB, per-top-level-key above that. Tune during Phase 4 based on search quality.
- **Doc chunking**: default to per-page for HM docs (their GitHub repo is Markdown files, one file per page). If pages are very long, split by top-level heading. Tune during Phase 4.
- **Older-artifact retention**: default to keeping the last 3 mounted builds on disk; user can manually keep more via the Index Catalog. Prunes oldest automatically unless the user has pinned them.
- **CI trigger specifics**: default to "on tag push matching `hytale-*`" or "manual dispatch." Never "on every commit."
- **GH Actions runner size**: free tier 7 GB may be borderline for indexing with embedder; if indexing OOMs, move to HM-owned larger runner. Check this empirically on first full CI run.

## Verification

**Pre-phase (dev unblock):** re-run a local index against a small decompile tree; confirm elapsed time drops from ~12s/file to well under 1s/file.

**Phase 3 end-to-end:**
1. `atlas-build` produces a signed artifact locally; manifest verifies against embedded pubkey; tarball is < 1 GB for a typical Hytale build.
2. Artifact uploaded to a test GitHub Release.
3. Client `index_fetch` command: downloads with progress events, kills the download mid-flight, restarts, resumes from partial via Range - no re-download of verified bytes.
4. Signature verification: manually corrupt one byte of the downloaded tarball → extraction fails with a clear `SignatureInvalid` error; no partial mount left behind.
5. Mount succeeds → `SearchCatalog` sees the new build_id → search returns hits tagged with the correct build.
6. Older build stays mounted simultaneously; switching active build in Index Catalog changes search target without restarting the app.
7. MCP endpoint: `curl` a `search` tool call against the local `/mcp` route; response validates against `docs/mcp-contract.md` schema.
8. Contract tests pass (`cargo test --test mcp_contract`).

**Regression suite for Phase 3:**
- Unit tests for manifest parsing, signature verify, version-key compatibility check
- Integration test that builds a tiny artifact end-to-end (build → sign → fetch → mount → search) in a single `cargo test`
- Frontend: Vitest for `fetchStore` state machine (though Vitest isn't set up yet - first Phase 3 testing infra addition)

**Success looks like:** a clean-install Atlas instance on a machine with zero Java tooling can, from cold, go from "enter Hytale install path" → "search works across the full server source" in under the time it takes to download the artifact, without running Vineflower once.
