# Atlas AI Authorship Audit

Date: 2026-05-03
Scope: `src-tauri/src/**` (Rust backend, ~16k LOC) and `src/**` (React/TS frontend, ~7k LOC). Sampled across indexer, search, fetcher, summarizer, mcp, eval, and the major frontend pages/components.

## TL;DR

Signal is **moderate-to-heavy on the Rust side, light on the frontend**. The code itself is competent and intent-driven; what gives it away is the prose around it. Specifically: pervasive em dashes, royal "we" voice, exhaustive module-level docstrings on every Rust file, and `(Phase X.Y)` callouts threaded through doc comments and user-facing comments. None of the textbook AI giveaways are present (no "robust", "comprehensive", "leverage", "seamless", etc., zero hits across the entire repo). The user has clearly been editing prose. What's left is residue.

The frontend reads as cleaner human-authored work. The Rust backend reads like dictation: the developer drove the design, Claude wrote the surrounding comments and docstrings.

## 1. Where the AI fingerprint is strongest

### 1a. Em-dash density (the loudest tell)

272 em dashes across 36 Rust files. Zero in the TS frontend. The user's stated rule is "no em dashes in prose I see or share" but they have leaked all over `.rs` doc comments. Worst offenders:

- `src-tauri/src/indexer/mod.rs` - 46
- `src-tauri/src/bin/atlas_build.rs` - 22
- `src-tauri/src/indexer/chunker.rs` - 22
- `src-tauri/src/indexer/hypixel_docs.rs` - 19
- `src-tauri/src/indexer/symbols.rs` - 16
- `src-tauri/src/commands.rs` - 12
- `src-tauri/src/search/hybrid.rs` - 11
- `src-tauri/src/fetcher/mod.rs` and `mount.rs` - 10-12 each

A regex-replace pass to swap ` — ` for ` - ` (or split into two sentences) would erase the single biggest stylistic AI signature in this codebase.

### 1b. Module-level "explain the architecture" docstrings on every file

Almost every Rust file opens with a `//!` block that runs 10-30 lines and explains why-this-module-exists, the data flow, and cross-references to other phases. Examples:

- `src-tauri/src/indexer/sanitize.rs:1-22` - 22 lines of preamble for a 250-line file with one public function
- `src-tauri/src/indexer/summarizer.rs:1-22` - reads like a design doc paragraph
- `src-tauri/src/fetcher/signing.rs:1-26` - "Trust model" / "Key rotation" headed sections
- `src-tauri/src/fetcher/mount.rs:1-25` - numbered 6-step flow narrative
- `src-tauri/src/indexer/symbols.rs:1-38` - "Schema shape" subsection with bullets

A solo human typically writes one of these for the gnarly module and a one-liner for the rest. Atlas has them everywhere. Even `config.rs` (177 LOC, almost trivial) opens with a 6-line `//!` block.

To pass as solo-human, keep the long doc comments only on the actually load-bearing modules (`indexer/mod.rs`, `search/hybrid.rs`, `fetcher/signing.rs`, `lance/mod.rs`, `summarizer.rs`) and collapse the rest to a one-line `//!` summary or delete entirely.

### 1c. "Phase X.Y" markers in code comments

84 occurrences across the backend, 27 across the frontend. Examples:

- `src-tauri/src/commands.rs:585` `(Phase UX-O.4)`, `:726` `(Phase UX-O.6)`
- `src-tauri/src/indexer/mod.rs:1149` `(Phase UX-O.2)`
- `src-tauri/src/mcp/mod.rs:1` `(Phase 3.I.2)`, plus 11 more
- `src-tauri/src/indexer/summarizer.rs:73` calibration date `2026-04-30`

This is recognisable as Claude Code echoing the plan.md vocabulary back into the source. Real solo work loses these markers within a few weeks of writing. Strip them; the code still tells the same story without them.

### 1d. Royal "we" voice in comments

19+ occurrences of "we don't / we're not / we can't / we keep / we walk / we strip" in Rust comments. Concentrated in `hybrid.rs`, `chunker.rs`, `hypixel_docs.rs`, `commands.rs`, `atlas_build.rs`. A solo developer's comments tend to be imperative ("strips X", "skip when Y") or first-person-singular implicit ("don't try Z"). The plural is a Claude habit.

### 1e. Exhaustive named tests with literate snake_case

`src-tauri/src/indexer/sanitize.rs:156-254` is the canonical example: 10 tests, each named like `double_slash_inside_string_is_not_a_comment`, `quote_inside_block_comment_is_not_a_string`, `non_source_text_passes_through_unchanged_at_caller`. The naming style is almost too self-documenting. Humans usually shorten to `line_comment`, `nested_quote`, `markdown_passthrough`. Not a smoking gun on its own, but combined with the comment density it reads as Claude.

### 1f. Defensive `with_context` wrapping

Mostly justified, but there are spots where the chain is overdone. Example: `src-tauri/src/fetcher/mount.rs:60-80` and `src-tauri/src/indexer/mod.rs:140-170` chain `.context(...)` on operations whose underlying error already carries the path. Not wrong, just verbose in a way Claude defaults to.

## 2. Where the code reads as human-authored

**The frontend.** `src/components/BranchCard.tsx`, `src/pages/SearchPage.tsx`, `src/state/searchStore.ts`, `src/components/FirstRunModal.tsx` all read clean. Comments are sparse, terse, and anchored to the actual surprising bit ("Re-validate on every path change, debounced via effect" `FirstRunModal.tsx:27`). No em dashes. JSDoc only where the type is non-obvious. Phase markers exist but only 27 across 7k lines, vs 84 across 16k lines on the backend.

**Single-purpose Rust files.** `src-tauri/src/config.rs`, `patcher/version.rs`, `indexer/walker.rs`, `embedder/mod.rs` look fine. Short, idiomatic, light comments.

**Pockets of human voice in the backend.** Watch for these phrases - they are unmistakably the user, not Claude:

- `src-tauri/src/eval/mod.rs:8` "becomes a measurable delta instead of vibes"
- `src-tauri/src/fetcher/mount.rs:15` "is a hard NO"
- `src-tauri/src/indexer/chunker.rs:22` "we punt on it here"
- `src-tauri/src/search/hybrid.rs:178` "without a 'static lifetime hack"

These breaks in the polish are reassuring; they confirm the design is the developer's.

**Empirically-grounded calibration constants.** `src-tauri/src/search/hybrid.rs:25` (RRF_K), `:35` (OVERFETCH), `src-tauri/src/lance/mod.rs:51` (MAX_KNN_DISTANCE) all carry inline rationale tied to a specific dated experiment. That kind of comment ("Validated 2026-04-30 on the 223-file sample") is exactly what a thoughtful human writes for their own future self. AI rarely produces these unprompted.

## 3. Concrete patterns to change

In rough priority order:

1. **Em-dashes**: bulk-replace ` — ` with ` - ` or rewrite the sentence. Highest-impact single edit. Affects 36 files.
2. **Phase markers in code**: strip `(Phase X.Y)` and `Phase UX-O.N` annotations from doc comments. Keep them in `docs/plan.md` where they belong. Affects ~110 sites.
3. **Module-level `//!` blocks**: trim or delete on small/obvious files. Specific candidates:
   - `src-tauri/src/config.rs:1-7` - delete
   - `src-tauri/src/indexer/walker.rs` - keep one line
   - `src-tauri/src/indexer/sanitize.rs:1-22` - cut to 5 lines (just "what" and the security rationale)
   - `src-tauri/src/fetcher/status.rs`, `patcher/status.rs` - one line each
   - `src-tauri/src/indexer/metadata.rs`, `schema.rs` - condense
4. **Royal "we"**: search-replace "we don't" -> imperative, "we keep" -> "kept", etc. in `hybrid.rs`, `chunker.rs`, `hypixel_docs.rs`, `commands.rs`, `atlas_build.rs`.
5. **Test names**: collapse the over-explanatory test names in `sanitize.rs`, `chunker.rs` test modules. `non_source_text_passes_through_unchanged_at_caller` -> `markdown_passthrough`.
6. **Numbered-step doc bullets**: `src-tauri/src/fetcher/mount.rs:1-25` and `src-tauri/src/fetcher/signing.rs:13-19` use literate "1. ... 2. ..." narratives. Replace with one-paragraph prose or kill entirely; the code structure already shows the steps.
7. **Section banners**: `src-tauri/src/mcp/mod.rs:72-76` uses `// ----------------- Parameter / output schemas ...` ASCII rules. Humans usually let `///` doc comments on the types speak for themselves.

## 4. Overall AI signal: moderate

If you grade by *function bodies*, the code looks human-driven. The architectural decisions are coherent (one ProgressSink trait shared by CLI and desktop, RRF tuning calibrated against a real golden set, schema-versioned manifest, embedder-id compatibility gate). No obvious "AI helper" sprawl: I did not find unnecessary wrappers, dead "for future use" parameters, or speculative trait abstractions.

If you grade by *prose*, the AI signal is loud. Em dashes, exhaustive docstrings, "we" voice, Phase markers, and the literate-step lists in module headers are the primary smell. None of these affect correctness, but together they make the codebase read as "Claude Code transcript" rather than "developer's working repo."

A focused 1-2 hour pass on points 1-4 above (em-dashes, Phase markers, trim `//!` headers, kill the royal we) would shift the read substantially. The bones underneath are fine.
