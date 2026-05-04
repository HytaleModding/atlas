# Atlas

A mod author dashboard for Hytale. One workspace for decompiled server source, community docs, asset inspection, project health, API change tracking, and log viewing - with an MCP server so AI coding tools can use it as a tool.

**Status:** active development. Phase 3 of the [project plan](docs/plan.md) is in flight - the central-hosted index pivot. Expect breakage on `main`.

## Why this exists

Modding Hytale today means alt-tabbing between decompiled JARs, scattered docs, asset zips, and your editor. Atlas pulls all of that into one searchable surface. The index is built centrally by Hytale Modding and shipped to your machine signed; you never run a decompiler or an embedder yourself.

## Why Rust + Tauri (not Java)

Tauri gives a real native window with system integration (file pickers, OS notifications, deep links) at a fraction of the binary size and RAM of an Electron app. Rust on the backend gets us Tantivy (BM25) + LanceDB (vectors) without a JVM in the install. Modders shouldn't need a Java toolchain just to *read* about Hytale's Java API.

## Run it

Pre-built downloads will live on the GitHub Releases page once `v0.1.0` is tagged. Until then, build from source via the steps below.

## Develop it

```
git clone https://github.com/HytaleModding/atlas.git
cd atlas
npm install
npm run tauri dev
```

Tauri's dev mode hot-reloads both sides: save a `.tsx` and the UI updates; save a `.rs` and the backend rebuilds.

Production build:

```
npm run tauri build
```

Output lands in `src-tauri/target/release/bundle/`.

### Prerequisites

- Node 20+ and npm
- Rust toolchain (stable) via [rustup](https://rustup.rs/)
- Windows: Microsoft C++ Build Tools (Tauri requirement)

## How updates work

Atlas pulls two things from this repo's GitHub Releases:

| What | How |
| --- | --- |
| The Atlas app | Tauri's built-in updater, signed |
| Hytale source + docs + assets | In-app fetcher, Ed25519 signed |

Both are signature-verified before mount. The user sees an "Update available" prompt; everything else is invisible.

> **Note on `src-tauri/signing/atlas-pubkey.hex`.** The public key currently in the repo is a development placeholder so the signed-data-package code path is exercisable end-to-end during local builds. Hytale Modding CI will replace it with the production public key before any signed release is cut. See `src-tauri/signing/atlas-pubkey.hex` for the in-file note.

## What's shipped vs what's coming

**Shipped:**
- Hybrid search across decompiled server source (BM25 + vector RRF)
- HytaleModding docs and Hypixel Javadocs in the same result list
- Inline Javadocs rendered above each method in the source viewer
- Java syntax highlighting, keyboard navigation, find-in-page
- In-app "Help us improve Atlas" feedback that opens a pre-filled GitHub issue

**Phase 3 (in flight):** central-hosted index pivot. Indexes are built by HytaleModding CI, signed, and shipped as artifacts. Your client downloads, verifies, and mounts them - no more local Vineflower or local embedding. MCP endpoint goes live alongside.

**Roadmap (post-Phase 3):**

- **Pre-release pipeline.** Fast index turnaround on pre-release builds and a clean way to flip between release and pre-release without losing the prior index. Today the toggle exists but pre-release indexing is the same heavy local path as release.
- **Diff tracker.** Clear visual breakdown of what changed between two builds: removed methods, signature changes, deprecations. Pulls Hytale's own update blog posts in alongside the diff so you can read the official "what's new" inside Atlas, with API breakage callouts pinned at the top.
- **Project manager.** Point Atlas at your mod project; it indexes your code with the same chunker, resolves your `import` and method-call references against the active Hytale index, and tells you which of your call sites break on the next pre-release.
- **Log reader.** Pull client and server logs, parse them, and link stack frames into your project index so a `NullPointerException` jumps you straight to the line that threw it.
- **Asset inspection.** Search and preview `assets.zip` contents (block defs, items, prefabs) alongside source and docs.

The full feature pitch and architecture lives in [docs/plan.md](docs/plan.md).

## License

Apache 2.0. See [LICENSE](LICENSE).

## Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) - in particular the rules around touching the search ranker, the indexer, and the chunker.

Maintained by [Vibe Theory](https://github.com/vibetheory) and housed under [HytaleModding](https://github.com/HytaleModding).
