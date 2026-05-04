# Contributing to Atlas

Atlas is open source under HytaleModding. Fork it, run it, ship your own version - that's all fine. PRs back to this repo are welcome under the rules below.

Project lead: [Vibe Theory](https://github.com/vibetheory).

## Before you open a PR

1. **Open an issue first.** Every PR must reference an issue. Bug or feature, doesn't matter - we want the conversation to start there, not buried in a diff.
2. **Title format:** prefix the PR title with `GH-<issue number>`. Same convention as the [HytaleModding/site](https://github.com/HytaleModding/site) repo.
3. **Conventional Commits.** `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`. PRs that don't follow this won't be accepted.
4. **Tests pass.** `cargo test --manifest-path src-tauri/Cargo.toml` and `npx tsc --noEmit` both green. New behavior should land with a test.

## Restricted areas

A few parts of Atlas are load-bearing for everyone using the shared index. Don't open a PR that touches these without an issue *and* a comment from the project lead saying you're cleared:

- **Search ranker** - `src-tauri/src/search/` (RRF weights, query-shape heuristic, reranker logic)
- **Indexing pipeline** - `src-tauri/src/indexer/` (chunker, embedder integration, schema, sanitize)
- **Walker / file ingestion** - `src-tauri/src/indexer/walker.rs` and source-type ingestion under `indexer/hm_docs.rs`, `indexer/hypixel_docs.rs`
- **Build artifact format** - `src-tauri/src/fetcher/manifest.rs` and `atlas-build` packaging
- **Signing key material** - `src-tauri/signing/atlas-pubkey.hex` is the trust root the client checks against. The key currently checked in is a development placeholder so the verify path can be exercised in local builds. The production key is rotated in by Hytale Modding CI before any signed release is published; do not change it in PRs.

These all affect index quality and artifact compatibility for every Atlas user. Approval needs the project lead plus at least one other caretaker. Open the issue, propose what you want to do, get a green light, then code.

Everything else (UI, command surface, tooling, docs, tests, build scripts) - go for it.

## Development setup

See [README.md § Develop it](README.md#develop-it).

## Reporting bugs / feedback

Inside Atlas: click **Help us improve Atlas** in the left nav. It opens a feedback modal that pre-fills a GitHub issue with your search context (or a freeform bug report). That's the preferred path - the snapshot it attaches makes triage much faster than a screenshot.

## Community

- Discord: [HytaleModding](https://discord.gg/hytalemodding)
- GitHub Discussions on this repo

That's it. Keep PRs focused, link the issue, and we'll get to it.
