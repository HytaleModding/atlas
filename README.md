# Atlas

A mod author dashboard for Hytale. Search across the decompiled server, community docs, and Hypixel Javadocs from one window. Exposes the same data over MCP so AI coding tools can use Atlas as a tool.

> Active development. Expect breakage on `main`.

## Why this exists

Modding Hytale today means alt-tabbing between decompiled JARs, scattered docs, asset zips, and an editor. Atlas pulls all of that into one searchable surface. The reference data is built centrally by Hytale Modding and shipped to your machine signed; you never run a decompiler or an embedder yourself.

## Why Rust + Tauri (not Java)

Tauri gives a real native window with system integration (file pickers, OS notifications, deep links) at a fraction of the binary size and RAM of an Electron app. Rust on the backend gets us Tantivy (BM25) + LanceDB (vectors) without a JVM in the install. Modders shouldn't need a Java toolchain just to *read* about Hytale's Java API.

## Run it

Pre-built downloads live on the GitHub Releases page once `v0.1.0` is tagged. Until then, build from source.

```
git clone https://github.com/HytaleModding/atlas.git
cd atlas
npm install
npm run tauri dev
```

Dev mode hot-reloads both sides: save a `.tsx` and the UI updates; save a `.rs` and the backend rebuilds.

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
| Hytale reference data | In-app fetcher, Ed25519 signed |

Both are signature-verified before mount. The user sees an "Update available" prompt; everything else is invisible.

> The public key currently in `src-tauri/signing/atlas-pubkey.hex` is a development placeholder so the verify path is exercisable in local builds. Hytale Modding CI will replace it with the production key before any signed release is cut.

## What works today

- Hybrid search across decompiled server source (BM25 + vector RRF)
- Hytale Modding docs and Hypixel Javadocs in the same result list
- Inline Javadocs rendered above each method in the source viewer
- Java syntax highlighting, keyboard navigation, find-in-page
- In-app feedback that opens a pre-filled GitHub issue
- Signed central-built reference data with download + verify + mount
- MCP server alongside the Tauri IPC

## What's coming

- Pre-release pipeline polish: faster turnaround, cleaner switch between release and pre-release
- Diff tracker: removed methods, signature changes, and deprecations between two builds, with Hytale's own update posts pulled in next to the diff
- Project mode: register your mod, get told which of your call sites break against a target build before you rebuild
- Log reader: parse client + server logs, link stack frames to source
- Asset inspection: search and preview `assets.zip` contents alongside source and docs

## License

Apache 2.0. See [LICENSE](LICENSE).

## Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the rules around touching the search ranker, the indexer, and the chunker.

Maintained by [Vibe Theory](https://github.com/vibetheory) and housed under [HytaleModding](https://github.com/HytaleModding).
