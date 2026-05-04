# Atlas — Modder Community Audit

**Audience perspective:** a Hytale modder browsing the Atlas repo for the
first time after hearing it just went open source.
**Auditor lens:** "would I install this, contribute to it, recommend it on
the HM Discord, or close the tab?"

---

## TL;DR

Atlas reads as a serious, ambitious, well-scoped tool from someone who
clearly does Hytale modding for real. The README is welcoming, the plan
is unusually thorough, and the legal posture around the central index is
mature. The biggest community-facing risks are (1) the contribution door
is closed, (2) the stack is Rust + JS in a Java-shaped community, and
(3) the documentation is pitched at a project-lead audience, not a
drive-by modder. None of those are fatal; all three are fixable before
the public launch.

---

## 1. Tech stack reactions

**Tauri 2 + Rust + React/TS.** The first reaction from a Hytale modder
will be "wait, why isn't this a plugin or a Java tool?" The codebase
never directly answers this question, and it should. Modders see Java +
Gradle every day; a 50 MB native installer that doesn't need a JDK is
actually a *win* for non-technical mod authors, but the README presents
the stack matter-of-factly and lets the question hang.

The choice itself is defensible and the plan defends it well internally
(Tauri bundle size, no JVM dep on the user's machine, native fs +
filesystem watching, MCP being trivial to host in Axum). A short
"Why Rust + Tauri instead of Java?" paragraph in the README would defuse
80% of the pushback.

**Tantivy / LanceDB / BGE-small / tree-sitter / Vineflower.** These are
all the obvious correct choices once you've decided on Rust, and a
modder who reads `Cargo.toml` will recognise that. Vineflower
specifically will earn trust because it's already the de-facto Hytale
modding decompiler. Pinning the Vineflower version inside the central
builder (per `decompile-determinism.md`) is exactly the right move and
the kind of detail that signals "the maintainer has actually shipped
software before."

**Absences worth noting:**
- *Electron:* nobody will complain. Most modders associate Electron
  with bloat.
- *Python:* the `--summarize` flag uses `ANTHROPIC_API_KEY` and there's
  a documented Python sidecar fallback for embeddings, which is fine.
- *Java:* see above. Address it directly in the README.

**Risk flag:** React 19 + Tailwind 4 + shadcn is bleeding-edge at the
time of writing. A casual contributor on an older Node will hit
unexpected breakage. Document the Node 20+ requirement loudly in
CONTRIBUTING when it opens up.

---

## 2. Architecture choices: smart vs. overengineered

**Smart (will earn respect):**
- *Central-hosted signed index.* Once a modder reads
  `what-the-artifact-contains.md` they will get it. The legal framing
  (no source bytes ship, only token postings + vectors + symbol names)
  is exactly the answer Hypixel and HM admins will demand, and having
  it pre-written makes Atlas look serious rather than naive.
- *Ed25519 signing from v1.* Free credibility. The "old clients can't
  mount artifacts signed by a rotated key" framing as a feature, not a
  bug, is the kind of detail engineers love.
- *MCP day one.* This is the right bet. Cursor and Claude Code users
  in the modding community will love it. It's also the unique-value
  story: "Atlas is the data substrate your AI tools stand on" is a
  cleaner pitch than "Atlas is another search bar."
- *Local-first, AI-agnostic, opt-in telemetry.* These three lines in
  the philosophy section will close the deal for the privacy-conscious
  half of the community.

**Will read as overengineered to a casual visitor:**
- *Three-binary workspace (`atlas-desktop`, `atlas-build`,
  `atlas-serve`).* Reasonable for the central-host pivot but only
  obvious if you've read the pivot doc. A passing reader sees three
  bins and assumes scope creep. The README never mentions this; the
  pivot doc is buried two clicks deep.
- *Compound version key (`hytale_impl_version` /
  `chunker_version` / `embedder_id` / `schema_version` /
  `min_client_version`).* Necessary, but the doc explanation
  ("schema version alone papers over real incompatibility dimensions")
  is great and worth surfacing. Without that paragraph this looks like
  someone enjoying themselves too much.
- *`SearchCatalog` keyed-by-ID generalisation pre-Phase-5.* Justified
  if you trust the Phase 5 plan; looks speculative if you don't. YAGNI
  alarm bells will ring for some readers.
- *Three-dimensional version contract for MCP.* Defensible, but the
  intended audience is agent authors who almost certainly don't exist
  in the Hytale community yet.

The pattern: the architecture is *correct* but *the rationale lives in
docs the casual reader won't open.* Move two or three of these
justifications into the README itself.

---

## 3. What modders will love

- **One search bar over decompile + HM docs + Hypixel javadocs +
  assets.** This is the headline feature and it's the right one. Every
  Hytale modder has lost a workday to "I know I saw this method
  somewhere."
- **The Update Tracker.** "Atlas tells you what your plugin is about
  to break against on next release" is *the* feature most active mod
  authors will ask for. Phase 5 framing as the MVP pitch moment is
  spot on.
- **Dual-branch patcher.** Every modder maintains a release and a
  pre-release plugin and they all hate it. Atlas making this routine
  is a quiet but huge selling point.
- **Apache 2.0.** Permissive, plugin-author-friendly, pre-empts the
  "can I fork this for my Discord bot" question.
- **Honest framing in the philosophy section.** "Honest, never magic"
  and "we describe what the tool does accurately so users trust it"
  will land especially well with the engineer-leaning half of the
  community who are tired of AI marketing.
- **No bundled AI brand.** Modders who don't use AI tools won't feel
  excluded.

---

## 4. Suspicious or red-flag-ish

- **"Pre-alpha" + "Plan locked, implementation not yet started" in the
  README, but Phase 3 is clearly already underway** (rebuild runbook,
  legal spec, fully-populated `indexer/` and `fetcher/` modules,
  `atlas-build` bin target). The mismatch reads as either "stale
  README" or "vaporware that's actually code." Update the README's
  status line to reflect reality before the public launch.
- **No screenshots, no demo gif, no asciinema.** A modder with
  ten seconds to spend will leave. The UI spec describes a beautiful
  app; show one screenshot.
- **Hardcoded paths in `plan.md`** (`D:\Users\matth\AppData\...`).
  Anyone reading the plan will notice these are one user's machine.
  Replace with `%APPDATA%` / `~/.config` placeholders before public
  release.
- **README mentions "your-org/atlas" placeholder in the git clone
  URL.** Cosmetic but signals the repo isn't quite ready.
- **"HytaleModding has agreed to host" is presented as fact in
  internal docs but not in the README.** When the public sees the
  central-index architecture without this context, "trust the central
  builder" can read as "trust some random person's server." Be
  explicit in the README about who runs the pipeline.
- **The audits directory exists but is empty (other than this file).**
  If this remains empty at launch it looks abandoned. Either populate
  it or remove it.
- **`scraper` dep + a comment about HytaleModding scraping with
  rate-limit and robots.txt.** Pivot away from this and toward the
  documented "git-clone the public HM docs repo" path; the latter is
  already implemented (`indexer/hm_docs.rs`). Leaving stale scraper
  language in `plan.md` will spark needless drama.

---

## 5. Contribution friction

The current state is "not yet open to outside contributions" with a
note to open issues only. That's fine for pre-launch but creates two
specific problems for the moment Atlas goes public:

- **No documented extension points.** A modder thinking "I want to add
  a chunker for asset packs" or "I want to wire in a new doc source"
  has no map. The pivot doc names the right files (`indexer/chunker.rs`,
  `sources/hm_docs.rs`, the `source_type` discriminator) but a
  potential contributor shouldn't have to read three internal design
  docs to find them. A short `docs/extending.md` with "to add a new
  corpus, implement X trait, register in Y, your hits flow through Z"
  would unlock community contribution overnight.
- **CONTRIBUTING.md is a placeholder.** When the repo flips public,
  this needs a real first version even if it just says "PRs welcome
  for these specific areas, not these other ones, ping in issues
  first."

The codebase itself is structured well for contribution (clean
module boundaries, traits like `Embedder` that document their
fallbacks, a tests directory). The friction is *informational*, not
*architectural*.

---

## 6. License, governance, branding

- **Apache 2.0** is the right choice. No friction there.
- **Branding** explicitly avoids Hytale-owned assets (palette is
  "Hytale-inspired," not Hytale-supplied) and the plan flags the
  trademark/asset question as deferred-pending-permission. This is
  exactly the posture the HM admins will want to see.
- **Governance is undecided** (DCO vs CLA, single vs team
  maintainer). That's fine for now, but state *that it's undecided*
  in CONTRIBUTING rather than leaving the section blank — silent
  ambiguity worries potential contributors.
- **Repo name and "your-org" placeholder** suggest the GitHub
  transfer to a final org hasn't happened yet. Land that before any
  community announcement.
- **No code of conduct.** Standard expectation in 2026. Add one of
  the boilerplate ones (Contributor Covenant) before launch.

---

## What to fix before going public

The smallest set of changes that would dramatically improve the
first-impression experience for a Hytale modder:

1. README: add a "Why Rust + Tauri, not Java?" paragraph and one
   screenshot.
2. README: update status from "implementation not yet started" to
   reflect actual Phase 3 progress.
3. README: name HM as the central-build host explicitly.
4. Add `docs/extending.md` with the four extension points (new
   corpus, new chunker, new MCP tool, new viewer panel).
5. Replace hardcoded user paths in `plan.md` with placeholders.
6. Real CONTRIBUTING.md, even a short one, with the governance
   "still deciding" line.
7. Add `CODE_OF_CONDUCT.md`.
8. Either populate or remove `docs/audits/` (this file
   notwithstanding) and `eval-staging/`.

None of these touch architecture. All of them are first-impression
fixes. The actual product is in good shape.
