# Contributing to Atlas

Atlas is open source under HytaleModding. Fork it, run it, ship your own version. PRs back to this repo are welcome under the rules below.

Project lead: [Vibe Theory](https://github.com/vibetheory).

## Before you open a PR

1. **Open an issue first.** Every PR must reference an issue.
2. **Title format:** prefix the PR title with `GH-<issue number>`. Same convention as the [HytaleModding/site](https://github.com/HytaleModding/site) repo.
3. **Conventional Commits.** `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`. PRs that don't follow this won't be accepted.
4. **Tests pass.** `cargo test --manifest-path src-tauri/Cargo.toml` and `npx tsc --noEmit` both green. New behavior should land with a test.

## Restricted areas

A few parts of Atlas are load-bearing for everyone using the shared reference data. Don't open a PR that touches these without an issue *and* a comment from the project lead clearing you:

- **Search ranker** (`src-tauri/src/search/`): RRF weights, query-shape heuristic, reranker logic
- **Indexing pipeline** (`src-tauri/src/indexer/`): chunker, embedder integration, schema
- **Walker / file ingestion** (`src-tauri/src/indexer/walker.rs`, `hm_docs.rs`, `hypixel_docs.rs`)
- **Build artifact format** (`src-tauri/src/fetcher/manifest.rs` and `atlas-build` packaging)
- **Signing key material** (`src-tauri/signing/atlas-pubkey.hex`): the trust root the client checks against. The committed key is a development placeholder; the production key is rotated in by Hytale Modding CI before any signed release. Do not change it in PRs.

These all affect search quality and artifact compatibility for every Atlas user. Approval needs the project lead plus one other caretaker.

Everything else (UI, command surface, tooling, docs, tests, build scripts) is open: open the issue, propose what you want to do, code.

## Development setup

See [README.md](README.md).

## Reporting bugs / feedback

Inside Atlas: click **Help us improve Atlas** in the left nav. It opens a feedback form that pre-fills a GitHub issue with your search context. The snapshot it attaches makes triage much faster than a screenshot.

## Community

- Discord: [HytaleModding](https://discord.gg/hytalemodding)
- GitHub Discussions on this repo
