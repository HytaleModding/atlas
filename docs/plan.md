# Atlas - Project Plan

**Status:** draft v1.0 - plan locked, implementation not started
**Owner:** project lead
**License:** Apache 2.0
**Target audience:** the Hytale modding community (not Horizon-specific)

---

## Table of Contents

1. [Vision](#vision)
2. [Why This Tool Exists](#why-this-tool-exists)
3. [Philosophy](#philosophy)
4. [Pillars](#pillars)
5. [Tech Stack](#tech-stack)
6. [Architecture Overview](#architecture-overview)
7. [Phased Execution Plan](#phased-execution-plan)
8. [Cross-Cutting Concerns](#cross-cutting-concerns)
9. [Strategic Positioning](#strategic-positioning)
10. [Open Decisions](#open-decisions)
11. [Appendix: Key Paths](#appendix-key-paths)

---

## Vision

Atlas is a unified workspace for Hytale mod authors - a single place to
decompile, search, track, update, and debug Hytale plugin projects.

A Hytale modder today juggles: a decompile tool (ours, the patcher), a
file manager for the decompile output, a Java IDE with project-scoped
search, a browser tab on HytaleModding.dev for docs, another tab on the
Hytale blog for release notes, a zip viewer for asset pack JSON, a
terminal for gradle builds, two log directories being manually tailed,
and (if they're us) a folder of in-house research documents. **Atlas
collapses that chaos into one application with one search bar and one
project dashboard.**

---

## Why This Tool Exists

1. **The Hytale API is undocumented.** There are no official javadocs,
   no published reference, no canonical vocabulary. Every modder learns
   by decompiling and grepping. This makes symbol-name-based search
   (grep/IDE search) inadequate - you often don't know what the thing
   is called.
2. **Release cadence breaks plugins without warning.** Hytale ships
   breaking API changes in prose blog posts. Modders detect breakage
   by rebuilding and seeing what fails. A structured Update Tracker
   inverts this: surface breakage before the rebuild.
3. **Knowledge is siloed.** Server source lives in a decompile on disk.
   HytaleModding docs live on the web. Asset pack schemas live inside
   a ZIP. Nobody has built the connective tissue.
4. **AI coding tools are becoming table stakes.** Every AI-assisted
   Hytale session today burns context re-reading the same decompile
   files. An MCP-enabled search substrate lets AI tools answer Hytale
   questions efficiently, making modders more productive without Atlas
   itself having to be an AI product.

---

## Philosophy

These rules are non-negotiable. They tiebreak every design decision.

1. **Help modders manage their mods; do not manage for them.** Atlas
   surfaces information and provides clean access points. It does not
   silently rewrite code, auto-commit, or make decisions on the
   modder's behalf.
2. **AI-agnostic.** Atlas does not ship with an AI brand, an embedded
   chat assistant, or an opinion on which LLM is best. It exposes
   data via MCP + REST so any AI tool can plug in. Users who don't
   use AI see no AI features.
3. **Local-first.** Full functionality without internet. The index,
   search, logs, and project state all run locally. Cloud sync is an
   optional layer, not a dependency.
4. **Zero hardcoded assumptions.** No user, org, or project is assumed.
   All paths configurable, all projects user-registered, all features
   work against arbitrary manifest.json plugins.
5. **Honest, never magic.** Semantic search is embedding-based
   similarity, not "AI understanding." Update Tracker is explicit
   symbol matching, not "migration intelligence." We describe what
   the tool does accurately so users trust it.

---

## Pillars

Atlas rests on four user-visible pillars and one architectural spine.

### Pillar 1 - Semantic Search (the wedge)

Unified hybrid search (BM25 + vector) across:
- Decompiled Hytale server source (main release branch)
- Decompiled Hytale server source (pre-release branch)
- Hytale asset pack JSON (streamed from `Assets.zip`, no extraction)
- HytaleModding.dev docs and guides (cached locally)
- The user's own project source (when registered)

One query, one ranked list, corpus-tagged results. Natural language
queries bridge intent → symbol name for an undocumented API.

### Pillar 2 - Update Tracker

Per-release list of breaking API changes from Hytale's patch notes,
cross-referenced against registered projects. For each change, Atlas
surfaces the specific usages in the user's code, links old → new
symbol, and lets the user mark entries as reviewed / fixed / ignored.
No auto-rewriting; the user does the work, Atlas points at what needs
doing.

### Pillar 3 - Source Manager (dual-branch patcher)

Built on the existing Horizon patcher. Maintains two synchronized
workspaces:
- `workspace/release/decompile/` - tracks `%APPDATA%\Hytale\install\release\...`
- `workspace/pre-release/decompile/` - tracks `%APPDATA%\Hytale\install\pre-release\...`

Re-decompiles on Hytale version change. Diffs between branches to power
the Update Tracker. Both workspaces indexed into search.

### Pillar 4 - Project Manager + Log Viewer

Project detection (walks for `manifest.json` + `build.gradle`), health
dashboard (last-built-against Hytale version, manifest validity, build
status, git state, unresolved tracker entries), and integrated log
viewer with filtering, crash detection, and click-to-search on
stacktrace symbols.

### Architectural spine - MCP + REST access layer

Not a user-facing pillar. Every capability (search, source fetch,
asset fetch, tracker query, project state) is exposed internally as a
function, externally as both a REST endpoint and an MCP tool. This
turns Atlas into infrastructure that AI coding tools can consume
without Atlas itself having AI features.

---

## Tech Stack

### Primary stack (locked)

| Layer | Pick |
|---|---|
| Desktop shell | Tauri 2.x |
| Backend language | Rust |
| Frontend framework | React + TypeScript + Vite |
| Component library | shadcn/ui (Radix primitives + Tailwind) |
| Styling | Tailwind CSS |
| Icons | lucide-react |
| Theming | next-themes (dark default, system light mode) |
| Forms + validation | react-hook-form + zod |
| Toasts | shadcn Sonner |
| Server-state cache | TanStack Query |
| Client state | Zustand (kept minimal) |
| Syntax highlighting | Shiki (preferred) or highlight.js |
| Web server (hosted version) | Axum |
| Keyword search | Tantivy |
| Vector store | LanceDB |
| Local embedding model | BGE-small-en-v1.5 via fastembed-rs |
| Tool-state DB | SQLite via sqlx |
| AST parsing | tree-sitter + tree-sitter-java |
| File watching | notify |
| Zip streaming | zip crate |
| MCP server | rmcp |
| Hypixel Javadoc HTML parsing | reqwest + scraper |
| HytaleModding docs ingestion | git clone of HytaleModding/site |

**Visual direction:** Hytale-inspired palette (warm amber accent,
muted teal secondary, parchment-toned off-whites, deep cool-dark
backgrounds) applied to flat shadcn components. **No Hytale-owned
visual assets shipped.** See [docs/ui-spec.md](ui-spec.md) for the
full Phase 1 UI spec and design tokens.

### ML philosophy: use off-the-shelf, never train

No fine-tuning at v1. BGE-small is pretrained and good enough. We
revisit fine-tuning only when we have real user query/click data to
learn from (6+ months post-launch, minimum). Until then, pretrained
embeddings + hybrid retrieval is the right answer.

### Fallbacks documented (not expected to use)

- **If Rust becomes a blocker before productivity normalizes:** Kotlin
  + Compose Desktop + React, using Lucene + DJL. Same architecture,
  different language. Written down so panic-rewriting is never the
  answer under pressure.
- **If fastembed-rs can't load a chosen embedding model:** `ort`
  (ONNX Runtime bindings), `candle` (HuggingFace Rust ML), or a
  Python sidecar running `sentence-transformers`. Cheap to swap in.

---

## Architecture Overview

```
+---------------------------------------------------------+
|                       Frontend                          |
|   React + TypeScript (runs in Tauri WebView OR browser) |
+---------------------------------------------------------+
                          |
                          | HTTP + Tauri IPC
                          |
+---------------------------------------------------------+
|                   Rust Backend (Axum)                   |
|                                                         |
|   +-------------+   +--------------+   +-------------+  |
|   | Search      |   | Tracker      |   | Projects    |  |
|   | (Tantivy +  |   | (Patch notes |   | (manifest   |  |
|   |  LanceDB +  |   |  parser +    |   |  scanner +  |  |
|   |  fastembed) |   |  symbol      |   |  health     |  |
|   |             |   |  finder)     |   |  monitor)   |  |
|   +-------------+   +--------------+   +-------------+  |
|                                                         |
|   +-------------+   +--------------+   +-------------+  |
|   | Source Mgr  |   | Log Watcher  |   | MCP Server  |  |
|   | (dual-branch|   | (notify-rs + |   | (rmcp -     |  |
|   |  patcher    |   |  crash +     |   |  exposes    |  |
|   |  integration|   |  filter)     |   |  all of the |  |
|   |  + index)   |   |              |   |  above)     |  |
|   +-------------+   +--------------+   +-------------+  |
|                                                         |
|   +-------------+------------+------------+-----------+ |
|   |            SQLite (tool state)                    | |
|   |  projects, tracker resolutions, preferences       | |
|   +---------------------------------------------------+ |
+---------------------------------------------------------+
                          |
                          | reads (not writes)
                          |
+---------------------------------------------------------+
|                  Local data (on disk)                   |
|                                                         |
|   workspace/release/{jar, decompile}                    |
|   workspace/pre-release/{jar, decompile}                |
|   indexes/tantivy/   indexes/lance/                     |
|   cache/hytale-modding/                                 |
|   Assets.zip (indexed in-place, not extracted)          |
+---------------------------------------------------------+
```

### Why this shape

- **Backend is one binary** with feature modules, not microservices.
  Simpler ops, simpler debugging. Tauri bundles it for desktop; Axum
  runs it standalone for web.
- **Frontend is one React app** that detects whether it's in Tauri or
  a browser at runtime. No separate desktop/web codebases.
- **MCP server is in the backend**, not a sidecar. Same capabilities,
  same data, same process. Ships as desktop-only for v1 (local index
  access); remote MCP is a v2+ decision.

---

## Phased Execution Plan

Each phase has a **goal**, **deliverables**, and a **smoke test gate**.
Gates must pass before advancing.

### Phase 0 - Plan + Repo

**Goal:** this doc committed, repo initialized, ready to write code.

**Deliverables:**
- This plan in `docs/plan.md` ✓
- README, LICENSE (Apache 2.0), .gitignore, CONTRIBUTING.md skeleton ✓
- Git initialized
- Pre-release JAR path confirmed: `%APPDATA%\Hytale\install\pre-release\package\game\latest\Server\HytaleServer.jar` ✓

**Gate:** Can answer "what is Atlas, who is it for, what stack, what order?"
in one paragraph without handwaving.

---

### Phase 1 - Foundation Skeleton

**Goal:** Tauri + Rust + React scaffold running; keyword search over
release-branch decompile.

**Deliverables:**
- Tauri project scaffolded (`src-tauri/` Rust, `src/` React+TS)
- Axum routes boot from Tauri with basic `/healthz`
- Patcher integration: Atlas can trigger a decompile of
  `HytaleServer.jar` into `workspace/release/decompile/`. Initial
  approach: shell out to existing Python patcher. Native Rust
  re-implementation deferred unless needed.
- Tantivy index built over Java source files (per-file granularity
  acceptable for v0, chunk refinement in Phase 2)
- React UI: search input, results list with file + line preview,
  file viewer panel
- First-run wizard: detect Hytale install path, confirm with user

**Gate A - Keyword search smoke test:**
- Search `PageManager` → correct files in top 3
- Search `getComponent` → sensible ranking (declarations above usages)
- Search `"com.hypixel.hytale"` (phrase) → works
- Cold query latency < 100ms on full decompile
- First-run wizard gets a non-user through setup in < 5 minutes

---

### Phase 2 - Semantic Layer

**Goal:** hybrid search (keyword + vector), dramatically better recall
for intent-based queries.

**Deliverables:**
- tree-sitter chunker for Java: per-method chunks with class context
- fastembed-rs embedding pipeline generates BGE-small vectors
- LanceDB stores chunks + embeddings + metadata
- Query-type heuristic: symbol-shaped queries → BM25-weighted; natural
  language → vector-weighted; hybrid reciprocal-rank-fusion for blend
- Dev mode: each result shows both score sources for debugging

**Gate B - Semantic quality smoke test:**
- "how do I show a message to a player" → Message, sendMessage,
  ShowEventTitle, PlayerRef methods in top 5
- "spawn projectile in look direction" → projectile factory + velocity
  classes in top 5
- "what happens when a block breaks" → BreakBlockEvent + related
  handlers in top 5
- Exact symbol lookup unchanged: `PlayerRef` still returns the class
  file first
- Query latency < 300ms p50, < 800ms p99
- Full re-index: < 5 minutes, < 2GB peak memory

**Stress test - index scale:**
Re-index full decompile from scratch 5 times. Confirm deterministic
size, no memory leak, no disk leak.

---

### Phase 3 - Multi-Corpus Integration

**Goal:** asset pack + HytaleModding docs searchable alongside source.

**Deliverables:**
- Zip-streaming indexer for `Assets.zip` - reads entries without
  extracting. Verified: Atlas data dir does NOT grow by 10GB.
- HytaleModding docs ingestion: clone of `HytaleModding/site` at
  central build time, walked for `.mdx` pages. Backlinks to the live
  page included in all UI surfaces. (Earlier drafts proposed scraping
  the live site; the partnership made a clone the cleaner path.)
- Corpus filter chips in UI: [Source] [Guides] [Assets] [My Docs]
- Per-result corpus badge + appropriate open-action (in-app viewer
  for source/assets, external browser for guide links)

**Gate C - Multi-corpus smoke test:**
- "how do goblin NPCs work" surfaces `Template_Goblin_*` (assets),
  `NPCRole` (source), matching HytaleModding guides - all in one list
- Disk check: `du -sh` on Atlas data dir shows NO duplicated asset
  content
- Offline: disable network, confirm everything still searches
  (cached guides + local source + local assets)

**Note on HytaleModding partnership:**
HytaleModding hosts the central index build, so docs ingestion runs
against a clone of `HytaleModding/site` from the start. Hypixel
Javadocs are still fetched as HTML and parsed locally during the
central build.

---

### Phase 4 - Project Manager + Health Dashboard

**Goal:** detect and track user's plugin projects; surface health.

**Deliverables:**
- Project scanner: walks user-specified roots, finds
  `manifest.json` + `build.gradle` pairs, classifies as Hytale plugin
- Per-project metadata: name (from manifest), version, path, last
  git commit, dirty state, last-built-against Hytale version (from
  workspace marker), manifest validation status
- Dashboard view: project cards with health chips, color-coded
- Add/remove project manually; auto-detect from common roots
- Manifest validator: schema check on all required fields

**Gate D - Project detection smoke test:**
- Point Atlas at `D:/CodeProjects/Horizon` → detects all plugin
  subprojects automatically (not hardcoded against Horizon naming)
- Point Atlas at a random unrelated directory → detects any valid
  Hytale plugin projects there, regardless of parent naming
- Each card shows correct metadata
- Invalid manifest triggers red chip + error detail on hover

---

### Phase 5 - Update Tracker

**Goal:** surface per-release API changes affecting each project.

**Deliverables:**
- Patch notes ingester: parses Hytale's Modders Warning Section into
  structured records. Input: URL pasted by user (automated scraping
  is v2). Output: `{kind, from_symbol, to_symbol, release, notes,
  severity, source_url}` records.
- Symbol finder: tree-sitter AST search over each project's source
  for usages of `from_symbol`
- Dashboard: per-project "N changes in Release X affect this project"
  summary
- Detail view: each change → list of file:line usages → click to
  open in external IDE (`idea://`, `vscode://` URI)
- Resolution status per entry: [Unseen] [Reviewed] [Fixed]
  [Ignored] stored in SQLite
- Context cards: tabs for old source (from release branch decompile),
  new source (from pre-release branch decompile), guide link,
  blog excerpt

**Gate E - Tracker accuracy smoke test:**
- Using Hytale pre-release Update 5 (real data) against a real
  Horizon plugin
- Manual audit: 100% recall on every breaking change named in the
  Modders Warning Section
- Click-to-jump opens correct file:line in IntelliJ AND VSCode
- False positive rate on symbol matches < 10%

**⬅ PITCH MOMENT**

Phases 1-5 form the MVP pitch to HytaleModding. Everything beyond this
point is polish, expansion, or partnership-dependent.

---

### Phase 6 - Log Viewer

**Goal:** unified live tail across Hytale's two log directories.

**Deliverables:**
- `notify` watchers on both log paths
  (see [Appendix: Key Paths](#appendix-key-paths))
- In-app panel with live append, timestamps, source badge
  (client/server)
- Filter chips: [All] [Client] [Server], plus per-plugin name filter
  (auto-populates from registered projects)
- Crash detection: pattern match on ERROR/Exception, highlight red
- Symbol extraction from stacktrace lines: class names are clickable,
  opens in semantic search panel
- Optional: scan registered `known-issues.md` files for match on
  error patterns

**Gate F - Log viewer smoke test:**
- Start Hytale server, load plugins; Atlas log panel shows lines
  from both dirs within 500ms of write
- Filter to one plugin name → only its lines remain
- Force a known crash → highlighted; click a class in the trace →
  opens that class in search

---

### Phase 7 - MCP Server

**Goal:** Atlas becomes substrate AI coding tools stand on top of.

**Deliverables:**
- rmcp server runs in-process
- Exposed MCP tools:
  - `search(query, corpus?, mode?)` → ranked results with excerpts
  - `get_source(fqn_or_path)` → full source of class/file
  - `get_asset(zip_path)` → asset JSON by path
  - `get_breaking_changes(release?, project?)` → tracker records
  - `find_usages(symbol, project?)` → file:line list
  - `get_project_health(project?)` → dashboard snapshot
- README: one-line install snippet for Claude Code / Cursor /
  Windsurf / Zed

**Gate G - MCP stress test:**
- Claude Code configured with Atlas MCP
- Ask "how does Hytale handle inventory management?" → Claude
  chains search → get_source calls, produces correct answer with
  real SSOT file citations
- Run ~20 MCP tool calls in one conversation; no tool crash,
  p99 < 1s
- Subjective: users report reduced context-burn in Hytale-aware
  AI sessions

---

### Phase 8 - Web Version

**Goal:** browser-accessible instance, ideally hosted on HytaleModding.

**Deliverables:**
- React frontend compiles cleanly for web (Vite web target)
- Axum backend as standalone server binary, reads same index formats
- Auth decision made (probably SSO via HM if partnership gives hooks,
  else open read-only)
- Deployment: TBD by partnership outcome
- Features: search, Update Tracker (read-only). Explicitly missing:
  project management, log viewer, build/deploy (local-filesystem
  features, not portable to web)
- Missing MCP in web version for v1 (security considerations)

**Gate H - Web parity + cost check:**
- Web-side search quality matches desktop on 50-query benchmark
- Hosting cost projection at 100 daily users: known, budgeted
- Cold page load < 2s with CDN-cached frontend assets

**Partnership branch:** if HM doesn't sign on, fall back to self-host
on a small VPS or shelf Phase 8 entirely in favor of Phase 9 polish.

---

### Phase 9 - Community Release

**Goal:** public beta.

**Deliverables:**
- Signed installers for Windows, Mac, Linux (Tauri builders)
- Docs site: install, onboarding, MCP integration guide,
  contribution guide
- Public GitHub repo (transferred from private if applicable)
- HytaleModding announcement post (if partnered) or organic launch
- Issue tracker, Discord presence for feedback

**Gate I - New-user onboarding test:**
- Recruit 3 Hytale modders unfamiliar with Atlas
- Watch them: install → connect a project → run a useful search
- All 3 succeed in < 10 minutes without assistance
- Ship.

---

## Cross-Cutting Concerns

### Testing strategy

- **Rust backend:** unit tests for chunkers, parsers, query builder;
  integration tests for full indexing → query flow; property-based
  tests for schema migration via `proptest`.
- **Frontend:** Vitest for components, Playwright for end-to-end
  flows (run Tauri dev, drive UI, assert results).
- **Search quality:** fixed benchmark of 50 queries with expected
  top-5 results. Run after every embedding pipeline change. Track
  recall@5 over time; any regression blocks merge.
- **Update Tracker accuracy:** fixture of past release notes + known
  breaking changes + expected file matches in a sample plugin.
  Regression test on every parser change.

### Performance budgets

| Metric | Target |
|---|---|
| Cold search query | < 300ms p50, < 800ms p99 |
| Log tail latency (write → display) | < 500ms |
| Full decompile re-index | < 5 min |
| Full re-index incl. assets + guides | < 10 min |
| Desktop RAM at rest | < 500MB |
| Desktop RAM during re-index | < 2GB peak |
| Installed size | < 50MB |

### Telemetry

- **Off by default. Opt-in only.**
- Opt-in data: anonymized query patterns, crash reports, Atlas
  version. Nothing project-specific leaves the machine without
  explicit consent.
- Privacy policy published alongside the first installer.

### Distribution security

- Sign Windows + macOS binaries (cert cost ~$200/yr)
- Auto-updater via Tauri's built-in updater with signed updates only
- Publish SHA256 checksums per release

### Contribution governance (future)

- Currently private. Will open after MVP pitch.
- DCO or CLA decision deferred until public.
- Maintainer model (single / team / community) deferred.

---

## Strategic Positioning

### Partnership strategy - HytaleModding

Not blocked on partnership for Phases 1-7. That's deliberate. Atlas
must be complete and valuable as a standalone desktop tool. Partnership
is upside, not dependency.

**Pitch moment:** end of Phase 5. At that point Atlas demonstrates:
- Semantic search across source + assets + scraped docs
- Working Update Tracker with real data
- Dual-branch patcher keeping both Hytale branches current
- Project dashboard

The pitch to HytaleModding owner: "I built this as a community tool.
It already works. Let's integrate - my users find your docs more
easily, your contributors have a better authoring surface, we both
benefit. If you want a web-hosted instance off your site, I'll build
that as Phase 8."

Partnership success → Phase 8 builds hosted version. Partnership
failure (or delay) → Atlas ships as pure desktop tool with API/MCP
access points. Either outcome is a complete product.

### Positioning against potential Hypixel-official tooling

If Hypixel ships first-party dev tooling at Hytale's full release,
Atlas risks obsolescence. Mitigations:
- Atlas's unique value (semantic search over undocumented API, MCP
  surface) is hard to replicate quickly even for a well-resourced team
- Community-led + open-source positioning makes Atlas the
  "unofficial but beloved" tool in parallel to any official offering
- If Hypixel is open to it, contribute Atlas capabilities upstream;
  being the reference implementation is better than competing

### Positioning against AI coding tools

Atlas is NOT a competitor to Claude Code, Cursor, etc. It's the data
substrate they consume. This framing is important for:
- Users: "does this replace my AI?" → no, it makes your AI better
- Marketing: we don't advertise AI; we advertise unified workspace
- Sustainability: AI coding tools' value grows when Atlas exists;
  they have every incentive to integrate, not compete

---

## Open Decisions

Decisions explicitly deferred, with timing:

| Decision | Deferred until | Why |
|---|---|---|
| Org transfer of GitHub repo | Pre-public-launch | Private OK for now; transfer is clean via `gh repo transfer` |
| Auto-scraping of patch notes | Phase 5+ | Manual paste acceptable for v1 accuracy verification |
| Hypixel `breaking-changes.json` ask | Post-partnership | HM acts as institutional voice if relationship forms |
| Fine-tuning embedding model | 6+ months post-launch | Requires real user query data |
| Remote MCP (web version) | Phase 8 + evaluation | Security model needs design |
| DCO vs CLA | Pre-public-launch | Low stakes while repo is private |
| Use of Hytale-owned UI assets | Only if Hypixel grants written permission | Copyright/trademark risk; partnership-mediated ask (Phase 5+). v1 ships Hytale-*inspired* palette and typography only - no Hypixel assets. |

---

## Appendix: Key Paths

### Hytale release branch
```
%APPDATA%\Hytale\install\release\package\game\latest\
  Server\HytaleServer.jar
  Assets.zip
```

### Hytale pre-release branch
```
%APPDATA%\Hytale\install\pre-release\package\game\latest\
  Server\HytaleServer.jar
  (Assets.zip expected but to be verified)
```

**Confirmed:** release and pre-release install cleanly side-by-side,
separate directory trees. Dual-branch patcher configuration is
straightforward.

### Log directories (for Phase 6)

```
Client:  %APPDATA%\Hytale\UserData\Logs\
Server:  %APPDATA%\Hytale\UserData\Saves\<savename>\logs\
```

### Atlas local workspace (at runtime)

```
<atlas-data>/
  workspace/
    release/{jar, decompile}
    pre-release/{jar, decompile}
  indexes/
    tantivy/
    lance/
  cache/
    hytale-modding/
  atlas.sqlite
```

Location: platform-appropriate app data dir via `directories` crate.
User-configurable.

---

*End of plan.*
