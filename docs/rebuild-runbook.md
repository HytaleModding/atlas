# Production rebuild runbook

End-to-end commands for cutting a fresh Atlas index artifact with all
four V1 corpora wired in: source, HM docs, Hypixel Javadocs (release +
prerelease), and assets.

This runbook assumes the operator already has:

- A decompiled Hytale server tree on disk (produced by the patcher)
- `git` on PATH (the indexer auto-clones the HM docs repo)
- Network access to `release.server.docs.hytale.com` and
  `prerelease.server.docs.hytale.com`
- An Ed25519 signing keypair (produce one with `atlas-build keygen` if
  you don't have one yet)

Paths below assume Windows. Substitute forward slashes / Unix paths as
appropriate.

---

## 0. Build the binary

```bash
cd D:/CodeProjects/Atlas/src-tauri
cargo build --release --bin atlas-build
```

Verify:

```bash
./target/release/atlas-build.exe --help
```

You should see the `index | pack | verify | search | hybrid-search |
keygen | summarize-test | eval` subcommand list.

---

## 1. Mirror Hypixel Javadocs

Production CI should mirror with `wget` rather than the in-process
fetcher (the fetcher exists for local dev only, and skips retries /
backoff). The cache root layout `atlas-build` expects is
`<cache-root>/javadocs/<host>/...` - keep the host segment so the
`source_type=hypixel_doc` `rel_path`s resolve at search-result-click
time:

```bash
mkdir -p D:/atlas-cache/javadocs
cd D:/atlas-cache/javadocs
wget --mirror --no-parent --convert-links \
     --quiet --reject "*.svg,*.png,*.gif" \
     https://release.server.docs.hytale.com/
wget --mirror --no-parent --convert-links \
     --quiet --reject "*.svg,*.png,*.gif" \
     https://prerelease.server.docs.hytale.com/
```

Verify each host tree contains `type-search-index.js` and at least
one class page:

```bash
ls D:/atlas-cache/javadocs/release.server.docs.hytale.com/type-search-index.js
ls D:/atlas-cache/javadocs/prerelease.server.docs.hytale.com/type-search-index.js
```

(Skip this whole step if the `--hypixel-docs` flag is omitted in step
3 - Javadoc ingestion is opt-in.)

---

## 2. Stage directory + signing key

```bash
mkdir -p D:/atlas-build/staging
mkdir -p D:/atlas-build/keys
./target/release/atlas-build.exe keygen \
    --out-private D:/atlas-build/keys/atlas-signing.pem \
    --out-public  D:/atlas-build/keys/atlas-pubkey.hex
```

Note the printed fingerprint. The pubkey `.hex` should be committed as
`src-tauri/signing/atlas-pubkey.hex` if this is the first key for a new
HM-controlled signing identity. The private `.pem` goes into CI
secrets - never commit it.

---

## 3. Index everything into staging

For a release build:

```bash
./target/release/atlas-build.exe index \
    --decompile     D:/CodeProjects/patcher/work/decompile/release \
    --staging       D:/atlas-build/staging \
    --slot          release \
    --hm-docs-fetch \
    --hypixel-docs  D:/atlas-cache/javadocs
```

For a pre-release build, swap the slot and decompile path. The
Javadoc cache holds both hosts side-by-side under `<host>/...`, so
the same `--hypixel-docs` value points at both:

```bash
./target/release/atlas-build.exe index \
    --decompile     D:/CodeProjects/patcher/work/decompile/pre-release \
    --staging       D:/atlas-build/staging \
    --slot          pre-release \
    --hm-docs-fetch \
    --hypixel-docs  D:/atlas-cache/javadocs
```

The shared cache root (embedder model + HM docs clone + Javadoc
mirror) defaults to `D:/atlas-cache` on Windows; override with the
`--cache-root <path>` flag or the `ATLAS_CACHE_ROOT` env var. The
desktop app reads the same env/default chain when resolving HM doc
and Javadoc files for the right-panel viewer, so keep the two in
sync.

`--hm-docs-fetch` shallow-clones
`https://github.com/HytaleModding/site` into
`<cache-root>/hm-docs/site/`, wiping any prior clone so re-runs
always pick up new commits. The fetched commit SHA is printed to the
build log so you can record what was indexed. Requires `git` on PATH.

Optional flags:

- `--summarize` - enable the LLM summarizer pass on source chunks.
  Reads `ANTHROPIC_API_KEY` from `.env` or env. Adds 30-60 minutes for a
  full corpus; cached so re-runs are free.
- `--hypixel-docs-fetch <host>` - skip step 1 and let `atlas-build`
  fetch the Javadocs in-process. Repeatable. **Local dev only.**
- `--hm-docs <path>` - point at a pre-cloned HM docs tree on disk
  instead of fetching. Useful for offline rebuilds; mutually exclusive
  with `--hm-docs-fetch`.

Verify the staging tree contains everything expected:

```bash
ls D:/atlas-build/staging/tantivy/        # segment files + atlas-meta.json
ls D:/atlas-build/staging/tantivy/symbols.sqlite
ls D:/atlas-build/staging/lance/          # lance manifest + data
ls D:/atlas-build/staging | grep -v decompile  # decompile must NOT exist
```

The decompile tree intentionally is not copied into staging - see
`docs/legal-spec/what-the-artifact-contains.md`.

Sanity-check each corpus is represented. The `search` subcommand
queries the keyword index directly; pick terms that should only match
one corpus:

```bash
# Source corpus
./target/release/atlas-build.exe search \
    --staging D:/atlas-build/staging --query "PageManager"

# HM docs corpus (markdown body of the HM docs site)
./target/release/atlas-build.exe search \
    --staging D:/atlas-build/staging --query "Hytale Modding"

# Hypixel Javadoc corpus (prose only the Javadocs would carry)
./target/release/atlas-build.exe search \
    --staging D:/atlas-build/staging --query "Removes the given player"
```

Each query should return at least one hit when its corpus is present.

---

## 4. Pack into a signed artifact

```bash
./target/release/atlas-build.exe pack \
    --staging              D:/atlas-build/staging \
    --out                  D:/atlas-build/atlas-index-release-89796e57b.tar.zst \
    --signing-key          D:/atlas-build/keys/atlas-signing.pem \
    --hytale-impl-version  2026.03.26-89796e57b \
    --hytale-patchline     release \
    --build-id             release-89796e57b
```

Pack prints:

- `build_id` - the catalog key clients use
- `sha256sums_sha256` - the digest of the digests file
- `signing_pubkey_fp` - must match what `keygen` printed in step 2

---

## 5. Verify the artifact

Verify uses the pubkey embedded in the binary by default. To verify
against a different pubkey (e.g. before committing it to the repo):

```bash
./target/release/atlas-build.exe verify \
    D:/atlas-build/atlas-index-release-89796e57b.tar.zst \
    --pubkey D:/atlas-build/keys/atlas-pubkey.hex
```

Output should end with:

```
layout ok (... payload files)
build_id           release-89796e57b
schema_version    <n>
signature ok      (fingerprint <hex>)
```

**Hard failures here block release.** A non-zero exit means clients
will refuse to mount the artifact.

---

## 6. End-to-end client smoke test

Spin up the desktop app pointed at a fresh data directory, fetch the
artifact, mount it, and run a query:

```bash
cd D:/CodeProjects/Atlas
npm run tauri dev
```

In the app:

1. Open the Index Catalog tab (or whatever the current build calls it).
2. Trigger a fetch / mount of the freshly-built artifact.
3. In the search page, run each of these and confirm hits in the
   correct corpus chip:
   - **Source** - search a known class name.
   - **Guides** - search a known HM doc title and a known Hypixel
     Javadoc class name. Both should appear under the Guides chip
     since `corpusToSourceTypes("guides") = ["hm_doc", "hypixel_doc"]`.
   - **All** - confirm hits surface across all three corpora.
4. Click a Hypixel Javadoc hit. The right panel should render the
   class description text without crashing on the missing
   `path → file on disk` mapping (Javadoc hits don't point at a
   readable source file the way decompile hits do).

If the right-panel render misbehaves on `hypixel_doc` hits, that's a
follow-up - open a separate task. The artifact itself is still good.

---

## Cheat sheet

| Step | Output                                      | Failure mode                  |
| ---- | ------------------------------------------- | ----------------------------- |
| 0    | `target/release/atlas-build.exe`            | compile errors → `cargo check` |
| 1    | mirrored Javadoc trees                      | network → retry / skip step   |
| 2    | `atlas-signing.pem` + `atlas-pubkey.hex`    | rerun keygen                  |
| 3    | `staging/tantivy`, `staging/lance`, sqlite  | OOM → reduce concurrency      |
| 4    | `*.tar.zst`                                 | unsigned → re-pass key path   |
| 5    | `signature ok` line                         | pubkey mismatch → recheck fp  |
| 6    | hits in all chips                           | missing chip rows → check `source_type` filter on commands.rs:`search` |

---

## What gets built into the artifact

Recap - the artifact tarball contains exactly:

- `manifest.json` + `manifest.json.sig`
- `tantivy/` (keyword index segments + `symbols.sqlite`)
- `lance/` (vector store)
- `SHA256SUMS`

It does NOT contain:

- `decompile/` (the decompiled Java source)
- Raw chunk text outside what `tantivy/` already encodes as inverted-index tokens
- Any JAR or JAR-extracted file

See `docs/legal-spec/what-the-artifact-contains.md` for the policy
behind that decision.
