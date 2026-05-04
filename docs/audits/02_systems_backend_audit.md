# Atlas Backend / Systems Audit

**Auditor perspective:** senior backend / systems engineer
**Scope:** `src-tauri/src/` Rust backend, `src/` React/TS frontend, build manifests
**Stance:** honest assessment, not a rubber stamp

## Executive summary

This is a high-quality codebase for a pre-alpha desktop tool. The shape is coherent: one Tauri binary, one shared multi-thread Tokio runtime, an Axum router that the MCP service hangs off, a search catalog wired to both Tauri commands and MCP so IPC and HTTP traffic land on the same cached readers. The plan and the code agree on what is being built and why.

What I would push back on, in order of consequence:

1. A handful of files (`indexer/mod.rs` 1626 LOC, `bin/atlas_build.rs` 1514, `chunker.rs` 1298, `commands.rs` 997) are pushing past the size where one person can hold them in their head. Splitting is overdue.
2. `SearchCatalog` is a long-lived `Arc` whose `OpenedIndex` cache is keyed by `IndexId` but only ever populated through a `Slot`-flavoured ensure path. The fetcher path mutates `<indexes_root>/{tantivy,lance}/<slot>/` underneath cached readers, then the callback calls `invalidate(slot)`. There is a real but narrow race window between mount-rewrite and invalidate.
3. The reranker is fully plumbed but disabled at the Tauri call site (`commands.rs:521-524`). The justification is sound, but the dead-but-wired state is the kind of scaffolding that quietly rots.
4. Frontend↔backend type contract is hand-mirrored. It works today because there is one author. It will drift the moment that stops.

Nothing here is load-bearing-broken. The criticisms are real but proportionate.

## 1. Architecture sanity

### What works

The module split is honest: `indexer/`, `embedder/`, `lance/`, `search/`, `mcp/`, `fetcher/`, `patcher/`, `reranker/`. Each owns a bounded responsibility. Trait surfaces (`Embedder`, `Reranker`, `ProgressSink`, `ExtractProgress`) are minimal and object-safe, exactly as documented in the plan's "fastembed-rs is not load-bearing" stance.

The runtime model is right for Tauri: one shared Tokio runtime (`lib.rs:32-52`), the Axum server and indexer jobs hang off the same handle, `RuntimeHandle` in managed state lets Tauri commands `rt.spawn(...)` work that needs to outlive the IPC call. The `OnceLock<Arc<Runtime>>` is unusual (most Tauri apps just let Tauri's runtime do the work) but the comment explains it: cancellation and lifetime control beat what you'd get if you let Tauri own everything.

`SearchCatalog` opening readers lazily and caching them in an `Arc<OpenedIndex>` keyed by `IndexId` is the right shape. The `ReloadPolicy::OnCommitWithDelay` (`indexer/mod.rs:1213`) is the correct choice for Tantivy: avoids reopening the searcher per query, picks up commits without manual intervention.

### Leaky abstractions and scaffolding

- `IndexId` (`indexer/mod.rs:1093-1118`) is introduced as a future generalization for fetched artifacts and user projects, but every call site (`ensure`, `invalidate`, `find_sibling`, `class_javadoc`, `search_chunks`) routes through `Slot`. The `_id` variants are mostly unused. This is honest preparatory work, not dead code, but if Phase 3.G doesn't ship soon the abstraction goes stale.
- `parking_lot_like` mod (`indexer/mod.rs:1610-1626`) is a local `std::sync::Mutex` shim to avoid pulling in `parking_lot`. Fine, but the comment "tiny Mutex shim so we don't have to add `parking_lot`" reads like the author flinched at one dep then built the same primitive. Just add `parking_lot` or use `std::sync` directly without the wrapper.
- `mounted_ids()` is `#[allow(dead_code)]` on `SearchCatalog`. Either it's about to be wired to the Index Catalog UX or it's premature.
- The reranker (`reranker/`) is fully implemented, has a `RERANK_WINDOW`, has its own `bge_reranker.rs`, and is bypassed at every entry point. `commands.rs:521-524` explicitly nulls it out with a comment about cold-cache hangs and parity-not-win golden eval. That's a defensible call, but two trait impls and ~175 LOC of reranker glue are sitting unused in a pre-alpha. Worth pruning to a stub or deleting until eval data justifies bringing it back.
- `add_corpus` (`indexer/mod.rs:239-456`) handles only `SourceType::HmDoc` and bails on every other variant. The doc-comment (`only `hm_doc` is supported today`) is honest. Acceptable until the next corpus needs incremental rebuild, but the function shape would benefit from the bail being moved to the dispatcher level so the body isn't a single match arm wrapped in a 200-line function.

### File-size violations of one's own rules

`indexer/mod.rs` at 1626 LOC, `bin/atlas_build.rs` at 1514, `chunker.rs` at 1298, `hypixel_docs.rs` at 1160, `commands.rs` at 997 are all comfortably above the "split when it stops fitting in your head" threshold. `indexer/mod.rs` in particular mixes the headless `run` orchestrator, the `add_corpus` orchestrator, two distinct blocking indexer bodies, the chunk-flush primitive, the `SearchHit` / `IndexId` / `SearchCatalog` types, and the `parking_lot_like` shim. It would split cleanly into `indexer/{run.rs, add_corpus.rs, flush.rs, catalog.rs, types.rs}`.

## 2. System load and resource handling

The hot path is well thought through.

**Embedder lifetime.** `SharedEmbedder` (`embedder/mod.rs:48-71`) is a `Mutex<Option<Arc<dyn Embedder>>>` that initializes once and hands out clones forever. No per-search load, no torn state from concurrent first-init. The lazy-init policy (only load when a Lance store actually exists, `commands.rs:501-511`) keeps memory at rest minimal.

**Indexing throughput.** Batch size 256 chunks per embed-and-write (`indexer/mod.rs:463`), Tantivy writer pinned to 2 threads with a 128MB heap (`mod.rs:50, 535`), explicit comment justifying the thread cap (Windows file thrash, "killed writer" error). The `block_on` from inside `spawn_blocking` to call async LanceDB methods (`mod.rs:993`) is the standard idiom for bridging a sync indexer thread into an async store. The `dev.package."*" opt-level = 3` profile override (`Cargo.toml:144`) is the pragmatic answer to slow debug builds dominated by ML deps.

**Mmap discipline.** Tantivy and Lance both mmap their segments. The catalog cache holds `Arc<OpenedIndex>` so segments stay open as long as anyone is searching. The blow-away-and-recreate rebuild path (`mod.rs:516-521`) deletes `index_dir`, which on Windows would fail if a reader still has files mapped. The `invalidate(slot)` callback after a successful indexer run drops the cache entry, but if a search races a rebuild the rebuild may transiently fail. Phase 1 contract is "rebuild from scratch", so this is acceptable; under real concurrent load the rebuild path needs a barrier.

**Footguns.**
- `dir_size_bytes` in `commands.rs:986-997` walks the entire mounted dir on every `index_catalog()` Tauri call. Each call is O(files). For mounted artifacts of tens of thousands of small Tantivy segments, this will start to bite. Should be cached on the mounted entry's `manifest`.
- `read_source` in `commands.rs:775-798` re-reads the full file from disk every time the user clicks a hit. No LRU. Fine for now (decompiled files are small, OS pagecache covers it). Worth a 16-entry LRU when the viewer becomes hot.
- The fetcher download throttles IPC emit to every 128 KiB (`fetcher/mod.rs:163-164`). Sensible. The extract progress sink (`mount.rs` ExtractProgress) does not throttle; `tar` extraction can fire tens of thousands of `report(current, total)` callbacks. Each one does a `tauri::Emitter::emit` into the IPC bus. Worth a cadence guard there too.

## 3. Code quality

**Error handling.** `anyhow::Result<T>` end-to-end on the indexer/search side, with `Context` adornment at boundary points (`with_context(|| format!("opening index at {}", ...))`). Tauri commands convert to `Result<T, String>`. This is the idiomatic split. There is little defensive `Result` posturing; failures genuinely propagate. The handful of swallowed errors (`tracing::warn!` then continue) are the right ones: a single unreadable file should not fail the whole index, and the warn includes path + err so you can diagnose.

**Idiomatic Rust.** No fighting the borrow checker. The `texts_for_task` + `loaded_for_task` move-into-spawn-blocking dance in `search/hybrid.rs:179-193` is explicit about why the pattern is needed (`'static` requirement, references would force a lifetime hack). `ChunkRow<'a>` with borrowed strings throughout the flush path avoids per-batch allocation. The few `.clone()` calls on `Arc` are unavoidable and labelled.

**Observability.** `tracing` everywhere. `tracing-subscriber` with `EnvFilter::try_from_default_env()` configured at startup. No `println!` debugging in the hot paths. Per-event spans on the indexer would be a nice upgrade, but the current `info`/`warn` levels on every meaningful transition are enough.

**Tests.** Unit tests live next to the code (`tests/` modules at the bottom of each file). They test the things that matter: weight selection in `pick_weights`, RRF promotion semantics in `rrf_blend`, schema dim invariants in `lance::tests`, batch round-trip, embedder trait object-safety. The expensive integration-flavored tests (real BGE model load, throughput probe) are gated behind `#[ignore]` with explicit documentation of how to run them. This is the right test-pyramid for this domain.

What is NOT tested:
- `SearchCatalog::find_sibling` and `class_javadoc` Javadoc-pairing logic. Both have tricky branching (Pass 1 exact match, Pass 2 name-only fallback, dedup) and zero coverage.
- The fetcher mount path (`verify_and_mount`). There is `verify_and_mount_with_pubkey` exposed for tests, which is the right hook, but no test in this audit's view uses it. The plan's "stress test, 5 rebuilds" gate would catch most of this; a unit test would be cheaper.

## 4. Concurrency

**Lock discipline.** `SharedIndexerStatus`, `SharedFetchStatus`, `SharedStatus` (patcher), and `SharedEmbedder` all wrap `Arc<Mutex<T>>` with tight critical sections (set / snapshot / is_busy). `expect("... poisoned")` on every `lock()` (`indexer/status.rs:70, 75, 80`, `embedder/mod.rs:62`). A poisoned mutex would crash the relevant subsystem rather than producing inconsistent state, which is the right call.

**No deadlock risk** in what I read. No nested locks, no async-across-lock, the `OnceLock` for the runtime is set-once-then-read-only. The `SearchCatalog` lock (`indexer/mod.rs:1199, 1223`) is acquired-checked-released, then re-acquired for the insert. There's a benign double-open under contention: two threads both miss the cache, both open the index, both try to insert, the second `insert` wins and the first's `OpenedIndex` is dropped. Cheap, correct, and the comment is silent about it. Worth a `dashmap::entry().or_try_insert_with` or a pattern that does the work under the lock when index opens become more expensive.

**Race I would actually flag.** `fetcher::mount::wire_legacy_slot` rewrites `<indexes_root>/tantivy/<slot>/` and `<indexes_root>/lance/<slot>/` on the blocking thread. Then control returns to the spawn closure, which calls `catalog.invalidate(slot)`. Any search that landed between the rewrite and the invalidate sees stale segments under a fresh dir. Tantivy's mmap should keep the prior version alive for that searcher (segments are immutable on disk), so the search returns prior-build results, not corruption. Still: there is no version barrier here. A monotonic catalog generation counter or a `RwLock<HashMap<IndexId, Arc<OpenedIndex>>>` with the rewrite holding the write lock would close it.

**Tauri State `Send`-ness.** `commands::search` (`commands.rs:484-488`) explicitly clones Arcs out of `State<'_, Arc<...>>` *before* the first await, with a comment explaining why. This is the correct pattern; Tauri State guards aren't `Send`. Anyone copying this style needs to know that, and the comment teaches it.

## 5. Backend↔frontend contract

The Rust `SearchHit` (`indexer/mod.rs:1031-1064`) and the TS `SearchHit` (`src/lib/indexer.ts:55-86`) are hand-mirrored. They match today. They will drift unless either side adds a guard. No `ts-rs`, no `specta`, no schema-export-from-rust. For an MCP-first project this is a real risk: rmcp's `schemars` derive auto-publishes the MCP tool schemas, so the MCP surface is self-documenting, but the Tauri IPC surface is not.

`tauri::Emitter::emit` events (`index:phase`, `index:progress`, `index:done`, `index:error`, `fetch:phase`, `fetch:progress`, `fetch:done`, `fetch:error`, `decompile:*`) are stringly-typed on both sides. Acceptable for now, painful at scale. Worth a const-string registry on each side at minimum.

The MCP surface is well-shaped: `SearchOutput.partial: bool` honestly tells agents when vector search fell back to keyword-only, error taxonomy is published in `docs/mcp-contract.md` and mapped through `contract_error()` (`mcp/mod.rs:583-594`). The build_id resolution (`resolve_slot`) is pragmatic but has a TODO baked in: `Some(other) if other.starts_with("release") => Ok(Slot::Release)` is a string-prefix kludge that needs to become a real artifact catalog lookup before Phase 3.G ships in earnest.

## 6. Load-bearing vs cosmetic

**Actually load-bearing:**
- `SearchCatalog` cache invalidation. Already correct for the current rebuild model; will need real synchronization when fetched artifacts can be live-replaced.
- Embedder dim invariant (`EMBEDDING_DIM = 384` enforced at write and at query time, `lance/mod.rs:123-131`, `embedder/bge_small.rs:66-73`). Crucial; the comment "fail loud" is the right instinct.
- The `ReloadPolicy::OnCommitWithDelay` and the `delete_term` + add-document sequencing in `add_corpus_blocking` (`indexer/mod.rs:366-369`). Tantivy's opstamp-ordered delete is subtle; the comment makes it clear someone has read the docs.
- Fetcher's `.ok` marker convention. The "only directories with .ok matter" rule (`fetcher/mount.rs:24-25`) is the correctness anchor for crash recovery.
- Path-traversal guard in `indexer::join_safe` (`indexer/mod.rs:1542-1559`). This is the only thing standing between an MCP `get_source({path: "../../../etc/passwd"})` call and an exfil. It's strict-enough.

**Cosmetic:**
- The `parking_lot_like` shim.
- `mounted_ids()` being `#[allow(dead_code)]`.
- The reranker being plumbed but disabled.
- Hand-mirrored TS types.
- File-size bloat in `indexer/mod.rs` etc. Refactoring is value-add, not bug-fix.

## Verdict

This is a genuinely well-engineered pre-alpha. The concurrency discipline, error-handling honesty, and observability are all at the level you'd want from a shipping product. The criticisms I listed are real, but they are the criticisms of a codebase that already got the hard parts right and now needs grooming, not the criticisms of a system that's about to fall over.

Two concrete next moves I would prioritize:
1. Split `indexer/mod.rs` and `chunker.rs`. They are the files that will receive the most edits in Phase 4 and 5; splitting now is cheap, splitting under deadline pressure is not.
2. Decide on the reranker. Either delete the trait and impls or schedule the background-init work that lets it be turned on. Carrying disabled-but-wired infrastructure forward is the kind of debt that compounds.

Beyond those, the `SearchCatalog`-vs-fetcher race window and the IPC type drift are the next things to handle before you have real users.
