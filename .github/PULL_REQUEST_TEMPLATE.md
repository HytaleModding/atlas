<!-- Title format: GH-<issue number>: <conventional commit message> -->
<!-- Example:      GH-42: feat(search): add fuzzy FQN matching -->

Closes #<!-- issue number -->

## What

Short description of what this PR does.

## Why

What the linked issue asks for, in your own words.

## Restricted areas touched

- [ ] No
- [ ] Yes - search ranker / indexer / walker / artifact format (see [CONTRIBUTING.md](../CONTRIBUTING.md#restricted-areas))

If yes: link the issue comment from the project lead clearing this work.

## Checklist

- [ ] PR title prefixed with `GH-<issue number>`
- [ ] Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` passes
- [ ] `npx tsc --noEmit` passes
- [ ] New behavior covered by a test
