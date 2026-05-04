# What the Atlas index artifact contains (and what it doesn't)

This document is the canonical answer to the question Hytale Modding admins
or Hypixel legal will ask about the central-hosted Atlas index: **does the
artifact ship Hytale source code in any form, decompiled or otherwise?**

The short answer is **no**. The longer answer follows.

---

## What ships inside `atlas-index-<build_id>.tar.zst`

| Path                  | What it is                                                                   | Contains source text? |
| --------------------- | ---------------------------------------------------------------------------- | --------------------- |
| `manifest.json`       | Build metadata: Hytale impl version, schema versions, fingerprint, timestamps | No |
| `manifest.json.sig`   | Ed25519 detached signature over `manifest.json`                              | No |
| `SHA256SUMS`          | Per-file hashes covered by the manifest                                      | No |
| `tantivy/**`          | Tantivy inverted index - token postings only (see below)                     | **No (tokens, not text)** |
| `lance/**`            | Lance vector store - 384-dim float embeddings + structured metadata          | **No (vectors, not text)** |
| `symbols.sqlite`      | Symbol catalogue: FQNs, signatures, modifiers, line numbers                  | **No (names + line numbers, no bodies)** |

### Tantivy: tokens, not text

The `content` field on every Tantivy document is **indexed-only, not stored**.
Before tokenization, every chunk has Java comments and string-literal contents
stripped. What lands on disk is an inverted index of identifiers, Java
keywords, and numeric literals - equivalent to the back-of-textbook index of
a programming book, not the chapters themselves.

You cannot reconstruct method bodies from a Tantivy posting list. You can
look up "which document contains the token `loadPage`," but you cannot
reverse the index to recover the surrounding code. This is a fundamental
property of inverted indexes; it is not a setting we can accidentally turn off.

Field-by-field schema lives in `docs/legal-spec/tantivy_documents.csv`.

### Lance: vectors, not text

Lance stores one row per code chunk with a `FixedSizeList<Float32, 384>`
embedding plus structured metadata fields (path, package, FQN, symbol name,
line numbers). The `content` column **was removed** from the schema before
the central-hosted pivot. There is no chunk text in the Lance store at all -
only the 384-float vector representation and the structural metadata that
points back at where the original source lived.

A 384-dim sentence-transformer embedding is a one-way projection. Recovering
the original text from a vector is an open research problem with no known
practical solution against modern embedding models for code.

Field-by-field schema lives in `docs/legal-spec/lance_chunks.csv`.

### symbols.sqlite: API surface, not implementation

The symbol catalogue contains class names, method names, parameter type
names, return type names, modifiers, and start/end line numbers. It does
**not** contain method bodies, field initializer expressions, or any
executable code. It is the same kind of information a Javadoc tool emits.

Field-by-field schemas live in `docs/legal-spec/sqlite_*.csv`.

---

## What does NOT ship inside the artifact

- **`decompile/`** - the decompiled Java source tree is intentionally absent
  from the artifact. This is the most important boundary: shipping the
  decompile would make the artifact a redistribution of Hytale source code,
  which is exactly the line we will not cross.
- **Raw chunk text** - the chunk strings the indexer feeds through the
  embedder are discarded after embedding. Nothing in the build pipeline
  persists them to the artifact.
- **JAR contents** - the Hytale server JAR is read by the central builder
  to produce the decompile, but neither the JAR nor any class file ships
  with the artifact.
- **Third-party assets** - `assets.zip` is parsed for indexable metadata
  (filenames, JSON keys); the binary asset bytes themselves are not
  included.

---

## How clients display source preview

When a user clicks a search hit in the desktop app, Atlas needs to show
them the surrounding code. Because the artifact has no source, the client
performs a **local** decompile: Vineflower runs against the JAR already
present on the user's Hytale install, producing a `<workspace>/decompile/`
tree on the user's own machine. Source preview reads from there.

This is the same operation users could perform themselves with any
decompiler against their installed JAR - Atlas just automates it. No
source ever travels over the network from HM's CI to the user's machine.

---

## The compliance argument in one paragraph

The artifact is an **information-theoretic lossy projection** of the
Hytale API surface - token postings, vector embeddings, structural
metadata - none of which can be inverted to recover the underlying
source. We ship enough information for a search engine to point you at
"the file that contains this code," but the user must already possess
the code (via their own legitimate Hytale install) for that pointer to
resolve to viewable text.

---

## Audit checklist for the central builder

The CI pipeline must, before publishing any artifact:

1. Confirm `decompile/` is **not** present in the staging directory
   passed to `atlas-build pack`.
2. Confirm the manifest's compound-version key matches the Hytale build
   the JAR was sourced from.
3. Confirm the artifact tarball passes `atlas-build verify` against the
   embedded HM pubkey.
4. Confirm no `*.java`, `*.class`, or JAR bytes appear in any payload
   path inside the tarball.

A failing check on any of the above is a hard publish-block.
