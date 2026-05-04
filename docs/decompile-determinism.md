# Decompile Determinism - Decision Doc

**Status:** locked-in for Phase 3
**Owner:** Atlas
**Last updated:** 2026-04-22

## Why this matters

Atlas ships the decompiled source tree *inside* the central-hosted index artifact
(Phase 3.D). Users search against that shipped source and - via the diff tracker
in Phase 5.C - resolve their own mod's API references against symbols lifted from
it. If two CI runs decompile the same JAR to two different trees, every downstream
invariant breaks:

- Artifact hashes in `SHA256SUMS` flap between builds with no real change.
- Line numbers in search hits (`start_line`, `end_line`, `preview_line`) drift.
- Diff-tracker "what changed between release X and pre-release Y" picks up
  noise from the decompiler instead of real API shifts.

Because the decompile is shipped (not recomputed on each client), we only have to
make the *central builder* deterministic - not every user's machine. That's a
softer bar than "fully reproducible across arbitrary installs", but we still want
an inspectable, stable output so:

- Two CI runs on the same JAR produce byte-identical trees.
- A local developer running `atlas-build` against the same JAR gets the same
  tree as CI (useful for debugging a release that looked wrong).
- If the decompile ever needs to change, we bump a version field and the change
  is visible, not silent.

## Pinned toolchain

These are the exact versions the `atlas-build` CLI (Phase 3.F) and CI workflow
will run under. Pinned in code - never read from the host environment.

### Vineflower

| Field         | Value |
|---------------|-------|
| Version       | `1.11.2` |
| JAR URL       | `https://github.com/Vineflower/vineflower/releases/download/1.11.2/vineflower-1.11.2.jar` |
| SHA256        | `e1e2415e7f78b34960402c4beddfc88e033d7842a23ecd132a8ec2eadd54f6bf` |
| Constants in  | `src-tauri/src/patcher/vineflower.rs:19-23` |

Matches the desktop client exactly - `VINEFLOWER_VERSION`, `VINEFLOWER_URL`,
`VINEFLOWER_SHA256`. The central builder reuses the same module, so the artifact
pipeline can only ever run a hash-verified copy of the pinned JAR.

### JDK (for running Vineflower)

| Field         | Value |
|---------------|-------|
| Distribution  | Eclipse Temurin |
| Major version | `21` |
| Full version  | `21.0.5+11` (latest LTS at time of spec; bump on CI pin) |
| Required on CI | `actions/setup-java@v4` with `distribution: temurin` + `java-version: '21'` |

**Why 21 (LTS) not 25?** Temurin 21 is universally available on GitHub Actions
runners (Ubuntu, Windows, macOS). Vineflower 1.11.2 requires Java 17+ and runs
cleanly on 21. JDK 25 is fine locally but adds churn risk on CI for no
determinism benefit - the bytecode Vineflower *reads* is the only thing that
matters for output stability, and the JDK Vineflower *runs under* doesn't affect
that as long as the major version is stable.

**Why pin the *full* version (`21.0.5+11`)?** Minor JVM releases have in the
past changed HashMap iteration order under `-XX:+UseCompactObjectHeaders` and
other opt-in flags. Vineflower's output is (as far as upstream documents) order-
independent, but we pin fully because the cost is zero and the upside is ruling
out a whole class of future surprises.

### Vineflower invocation

Flags exactly as shipped by `patcher/decompile.rs:37-50`. **Must not diverge**
between desktop client and central builder - the desktop client is the
ground truth. If the central builder ever needs a different flag, that's a
chunker-breaking change (see "version bumps" below).

```
java \
  -jar vineflower-1.11.2.jar \
  --decompile-generics=true \
  --hide-default-constructor=false \
  --remove-bridge=false \
  --ascii-strings=true \
  --use-lvt-names=true \
  --log-level=warn \
  -e=. \
  <source-dir> \
  <out-dir>
```

Working directory: `classes_dir` (same as desktop client).

Source-dir rule: `classes_dir/com/hypixel` if present, else `classes_dir` whole.
Preserved exactly from desktop client - keeps META-INF, signing metadata, and
other non-Hypixel classes out of the decompile. See `patcher/decompile.rs:70-77`.

### JVM flags

| Flag | Value | Why |
|------|-------|-----|
| `-Xmx` | `4g` on CI runners | Vineflower peaks ~2 GB on a typical Hytale server JAR; 4 GB headroom avoids OOM on small runners. |
| `-XX:+UseG1GC` | on (Temurin 21 default) | Stable GC behavior; rule out ZGC pauses skewing CI wall-time. |
| `-Dfile.encoding=UTF-8` | set | Vineflower writes strings directly to files; locale-dependent encoding would make output host-dependent. |
| Locale | `LANG=C.UTF-8` (Linux), `LC_ALL=C.UTF-8` | Sort order in generated code comments has bitten other decompilers in the past. Belt-and-braces. |

## Version bumps

Three version fields live in `IndexMetadata` (Phase 3.B) that change the
decompile surface. Bump when:

- `vineflower_version` - Vineflower jar version bump. Any change forces a full
  artifact rebuild; old artifacts stay mountable but the client flags them as
  "rebuilt on older decompiler" in the Index Catalog UX.
- `chunker_version` - chunker logic change (e.g., splitting methods differently).
  Independent of Vineflower; bumps do not require decompile re-run but invalidate
  the chunk/embedding cache.
- `schema_version` - artifact format change (e.g., new file in tarball). Bumped
  by the builder; the client's `min_client_version` gate refuses to mount
  artifacts from a newer format.

## Proof of determinism (acceptance criterion)

Before Phase 3.F's artifact builder lands, we commit a one-off script
(`scripts/verify-decompile-determinism.sh`) that:

1. Runs `atlas-build decompile` twice against the same input JAR in two fresh
   temp dirs.
2. `diff -r` the two output trees → must be empty.
3. Runs the same decompile on a Linux CI runner and a Windows local box → must
   also produce identical trees (may need line-ending normalization: Vineflower
   writes `\n`, so `.gitattributes` enforces `* text=auto eol=lf` in the source
   tarball).

The script becomes a CI check gating the `atlas-build` workflow.

## Known sources of non-determinism (watch list)

- **Filesystem walk order** during `pick_source`: `std::fs::read_dir` is not
  ordered on Linux. Vineflower reads paths from arg and walks them itself - check
  whether `-e=.` + positional source causes Vineflower to rely on OS walk order.
  If so, the builder must list source dirs explicitly in a sorted order.
- **Vineflower bytecode cache**: if we ever run Vineflower with a cache flag,
  cross-run reuse can produce subtly different output. Current invocation has
  no cache flag - do not add one.
- **Non-`ASCII` strings in source constants**: `--ascii-strings=true` escapes
  these, which is the deterministic path. Do not flip this flag.
- **Line separators**: Vineflower writes `\n`. CI on Windows would silently
  convert to `\r\n` without `.gitattributes`. Tarball extraction on the client
  preserves original bytes, so CI producing `\r\n` would mean Windows-built
  artifacts mount with mixed line endings. Normalize at build time, not
  client time.

## Revisit triggers

Revisit this doc when:
- Vineflower ships a new version and we consider upgrading.
- GitHub Actions deprecates Temurin 21 (not expected until 2031).
- We observe artifact hash flapping between two CI runs on the same JAR.
- We add a new decompile flag for a real reason (and then `chunker_version`
  or `vineflower_version` must also bump).
