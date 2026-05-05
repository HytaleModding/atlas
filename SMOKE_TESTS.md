# Smoke tests

Run this checklist on the platform installers from the latest
`build-app` workflow run BEFORE tagging a release. If anything fails,
fix it on `main` and let `build-app` produce a fresh installer. Only
tag `v*` once every flow below works on a clean machine.

The point of this list is to catch the failure modes that have actually
happened in shipped builds, not to test every feature. Add a row when
you ship a regression that this list would have caught.

## How to get the installers

1. Open the latest successful `build-app` run on GitHub Actions.
2. Scroll to the **Artifacts** section at the bottom of the run page.
3. Download the artifact for your OS:
   - Windows: `atlas-windows-x64`
   - macOS: `atlas-macos-aarch64`
   - Linux: `atlas-linux-amd64`
4. Unzip. You'll get the OS-native installers (`.exe` + `.msi` on
   Windows, `.dmg` + `.app.tar.gz` on macOS, `.AppImage` + `.deb`
   on Linux).

## Before each smoke test

For a true clean-machine test, wipe Atlas's data directory first so
the first-run wizard fires:

- Windows: `%APPDATA%\horizon\Atlas\`
- macOS: `~/Library/Application Support/dev.horizon.atlas/`
- Linux: `~/.local/share/dev.horizon.atlas/`

If you only want to test the upgrade path, leave the data dir alone
and just install on top.

## 1. Fresh install + first run

- [ ] Installer runs to completion without admin prompts beyond the
      OS-standard one
- [ ] App launches and the window opens at 1280x800
- [ ] First-run wizard appears (install path picker)
- [ ] "Browse" button opens a native folder picker
- [ ] Picking the real Hytale install folder shows a green checkmark
      ("Hytale install detected.")
- [ ] Picking a folder that is NOT a Hytale install shows the red
      error line with a reason
- [ ] "Continue" is disabled until a valid path is picked
- [ ] After "Continue", the wizard closes AND the release card in the
      top-left no longer says "Choose install" (this was the v0.1.3 bug)

## 2. Decompile the release JAR

- [ ] Release card shows a "Decompile" or similar action when source
      is missing
- [ ] Decompile runs through to completion (progress bar reaches 100%)
- [ ] After decompile finishes, source files are browsable from the
      tree on the left

## 3. Search returns results AND highlights code

- [ ] Type a query that should hit Java source (e.g. `Inventory`)
- [ ] Results list appears within a couple of seconds
- [ ] Clicking a hit opens the source file in the right pane
- [ ] **Source code is syntax-highlighted** (keywords coloured, not
      flat white text). This is the Shiki WASM regression that hit
      v0.1.4 — if you see plain text here, the bundle didn't ship the
      WASM correctly.
- [ ] The hit line is visibly highlighted with the accent colour band

## 4. Inline Javadocs render

- [ ] Open a file you KNOW has Javadocs in the live Hypixel docs
      (e.g. `InventoryComponent`, `WorldComponent`, anything under
      `com/hypixel/hytale/server/core/`)
- [ ] Above method declarations, an inline Javadoc card appears
      with the doc text rendered (not raw HTML)
- [ ] No "system cannot find the path specified (os error 3)" error
      bar at the top of the source pane. If you see that, the index
      artifact is missing its `javadocs/` payload — most likely the
      central `build-index` workflow failed silently. Check the
      `build-index` Actions run for the slot in question before
      tagging.

## 5. Markdown / HM docs render

- [ ] Search for an HM-doc page (e.g. `quickstart`, `mantle`)
- [ ] Clicking a hit renders the markdown with formatting (headings,
      code blocks, lists)
- [ ] Code blocks inside markdown are syntax-highlighted

## 6. Settings + dev tools

- [ ] Settings opens via the gear icon
- [ ] Hytale install path can be re-selected from Settings without
      relaunching
- [ ] No infra jargon visible in any user-facing text (no "artifact",
      "manifest", "slot", "fingerprint", "build_id", ".tar.zst")

## When a smoke test fails

1. Don't tag.
2. Note which step failed and on which OS.
3. Fix on `main`.
4. Wait for the next `build-app` run to finish.
5. Re-run from step 1 of this checklist on the new artifact.
6. Only tag `v<next>` once the checklist is green on at least the
   primary OS (Windows). macOS and Linux can fail post-tag and be
   fixed in a patch, but the Windows installer is what most modders
   will hit first, so it must be green before tagging.
