# Atlas - UI Spec (Phase 1 Minimum)

**Status:** initial - covers Phase 1 scope only (App Shell, Search Page,
File Viewer, First-Run Modal). Later phases extend this doc.
**Visual direction:** clean dev-tool UI with a Hytale-inspired palette
and typography accent. No Hytale-owned assets. See
[plan.md Open Decisions](plan.md#open-decisions) for licensing status.

---

## Design Principles

1. **Information density over whitespace.** This is a dev tool. Modders
   scan hundreds of results per session. Tight spacing, small-but-
   readable type.
2. **Keyboard-first.** Every common action has a shortcut. Mouse is the
   fallback, not the primary.
3. **Honest visuals.** Loading states show progress, not spinners.
   Empty states say what's missing. Errors say what went wrong.
   Nothing pretends to be smarter than it is.
4. **Theme-swappable from day one.** All colors via CSS variables.
   Typography scale via variables. Adding a second theme later is a
   styling job, not a refactor.
5. **Dark mode default.** Light mode respects system preference.
   Neither is an afterthought.

---

## Layout

Three-region desktop shell. Window minimum: 1024 × 720.

```
+-----+----------------------------------+----------------------+
|     |                                  |                      |
| Nav |   Main content                   |   Right panel        |
|     |   (active page)                  |   (file viewer)      |
|     |                                  |   collapsible        |
| 220 |                                  |   default 40% width  |
| px  |                                  |                      |
|     |                                  |                      |
|     |                                  |                      |
|     |                                  |                      |
+-----+----------------------------------+----------------------+
```

- **Left nav:** fixed 220px. Collapsible to 56px icon rail.
- **Main content:** flex-grow. Scrolls internally.
- **Right panel:** resizable (200-800px). Closed by default. Opens when
  a file is selected for viewing. `Esc` closes it.

Panels are not tabs. There is only ever one "active page" in the main
content area. Navigation is via the left rail.

---

## Navigation

Left rail, top to bottom:

| Item | Icon (lucide) | Status in Phase 1 |
|---|---|---|
| Search | `search` | **Active** |
| Projects | `folder-kanban` | Ghost (disabled, visible) |
| Tracker | `git-compare-arrows` | Ghost |
| Logs | `scroll-text` | Ghost |
| *(spacer)* | | |
| Settings | `settings` | Active (Hytale path only) |

Ghost items render at 40% opacity with a tooltip "Coming in Phase N."
Keeps the final shape visible from day one so users know what's
planned.

---

## Visual Language

### Color palette (starting values; tune for WCAG AA contrast)

All values as CSS custom properties. Dark mode defaults listed.

```css
--bg-base:        #0f1419;  /* app background */
--bg-surface:     #1a1f26;  /* cards, nav, panels */
--bg-elevated:    #242a33;  /* hover, active, dialog */
--border-subtle:  #2a3038;
--border-strong:  #3a414b;

--fg-primary:     #e8e6e3;  /* warm off-white, parchment-toned */
--fg-secondary:   #b4b0a8;
--fg-muted:       #7a776f;

--accent-primary: #d4a048;  /* warm amber, Hytale-inspired gold */
--accent-secondary: #4a9aa6; /* muted teal */
--accent-primary-fg: #1a1510; /* text on accent backgrounds */

--destructive:    #e06060;
--warning:        #e0a040;
--success:        #7aa967;

--corpus-source:  #4a9aa6;  /* teal */
--corpus-guides:  #d4a048;  /* amber */
--corpus-assets:  #a87acb;  /* soft violet */
--corpus-mydocs:  #7aa967;  /* muted green */
```

Accent is used sparingly: active nav item, primary buttons, focus
rings, selected result. Most of the UI is neutral grays.

### Typography

- **UI text:** Inter, system-ui fallback. Weights 400 / 500 / 600.
- **Display (Atlas wordmark + H1 headers only):** Cinzel or similar
  serif-with-character. Everywhere else uses Inter.
- **Code / file paths / source content:** JetBrains Mono, fallback
  `Menlo, Consolas, monospace`.

Scale (in rem; base 16px):

| Token | Size | Usage |
|---|---|---|
| `text-xs` | 0.75 | Badges, metadata |
| `text-sm` | 0.875 | Body text, result rows |
| `text-base` | 1.0 | Search input, primary copy |
| `text-lg` | 1.125 | Section headers |
| `text-xl` | 1.25 | Page titles |
| `text-2xl` | 1.5 | Display / wordmark |

### Spacing

4px grid. Common values: 4, 8, 12, 16, 24, 32, 48. No arbitrary
spacing. Tailwind defaults apply.

### Component library

**shadcn/ui** installed into the project (not imported as dependency).
Primitives from Radix. Icons from lucide-react. Theming via
`next-themes`. Form handling via `react-hook-form` + `zod`. Toasts via
shadcn's Sonner integration.

---

## Primary Screens

### 1. Application Shell

**Elements:**

- Left nav (see Navigation section)
- Top bar: **empty in Phase 1.** Reserved for global command palette
  (`Cmd/Ctrl+K`) in Phase 2+.
- Main content area
- Right panel (collapsed by default)
- Status bar at bottom: 24px tall, shows indexer state ("Idle" /
  "Indexing 1,247 / 8,302 files" / "Error: see details"), connected
  Hytale version, and a small Atlas build tag.

**Window chrome:** native OS decorations in Phase 1. Custom Tauri
chrome deferred.

### 2. Search Page

**Layout (top to bottom in main content area):**

1. **Sticky header (80px tall):**
   - Search input: full width, `base` type size, monospace acceptable
     for query clarity. Placeholder: "Search Hytale source, docs,
     assets…"
   - Below input: corpus filter chip row `[All] [Source] [Guides]
     [Assets]`. Active chip has accent background. Single-select only
     in Phase 1 (multi-select in Phase 3).
   - Right side of header: mode indicator - small muted text showing
     current search mode ("hybrid" / "keyword" / "semantic"). Click
     to toggle in dev mode; hidden in production.

2. **Result list:**
   - Virtualized. Unbounded results; only ~30 rendered at a time.
   - Each row: 56px tall, full width.
     ```
     [icon] path/to/file.java                          [corpus]
            matched snippet with highlighted term            :line
     ```
   - File path: JetBrains Mono, `text-sm`, truncated at start
     (`…nwhals/plugin/Something.java`) when too long.
   - Snippet: Inter, `text-sm`, muted foreground, one line, match
     highlighted in accent color.
   - Corpus badge: `text-xs`, corpus color, aligned right.
   - Line number: `text-xs`, muted, after snippet.
   - Hover: `bg-elevated`. Selected: accent left border (3px) +
     `bg-elevated`. Selected row is preserved across keyboard nav.

3. **Empty states:**
   - No query: centered, "Search the Hytale codebase" + small muted
     text "Tip: try 'how do I send a message to a player'"
   - No results: centered, "No matches for '<query>'"
   - Error: centered, red text, error message, retry button

4. **Loading:**
   - Subtle progress bar at top of search input (2px), indeterminate
     during query. No modal spinner.

### 3. File Viewer (Right Panel)

**Opens when:** a search result is selected (Enter or click).

**Layout:**

- **Header (48px):**
  - File path, JetBrains Mono, `text-sm`, truncated-at-start
  - Close button (X, lucide `x`) at right, `Esc` keyboard shortcut
- **Body:**
  - Syntax-highlighted source. Language detected from file extension.
    Phase 1 supports: Java, JSON, Markdown, plain text.
  - Highlighter: Shiki or highlight.js with a dark theme matching
    Atlas's palette (pick one with muted, Hytale-ish tones).
  - Scrolls to matched line on open, with 3 lines of context above.
  - Matched line: subtle accent-colored left border (2px).
- **No editing.** Read-only. No copy button in Phase 1 (browser
  default text selection works).

**Resizing:** drag handle on left edge of panel. Width persisted to
SQLite between sessions.

### 4. First-Run Modal

**When:** app launches and no Hytale install path is configured.

**Behavior:** blocks the app. Cannot be dismissed without completing.

**Layout (single centered dialog, ~480px wide):**

- Title: "Welcome to Atlas"
- Body text (one paragraph):
  > Atlas needs to know where Hytale is installed on your machine.
  > We'll decompile the server JAR and index it so you can search.
- Path input: pre-filled with auto-detected default
  (`%APPDATA%\Hytale\install\release\package\game\latest\`).
- Browse button (folder-open icon) opens native directory picker.
- Validation: on blur, check the directory contains
  `Server/HytaleServer.jar`. Green check on valid, red X with
  message on invalid.
- Primary button: **"Continue"** (disabled until valid path). Saves
  to SQLite, closes modal, begins initial decompile/index.
- Secondary button: **"Skip for now"** - saves empty state, closes
  modal, app opens in a degraded state showing "No Hytale install
  configured" in the search area with a CTA to set one in Settings.

**Pre-release path:** Phase 1 sets only the release path via this
modal. The pre-release detection is automatic (same tree, different
`install/` subdir) but verification is a Phase 2 concern.

---

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Cmd/Ctrl + K` | Focus search input |
| `↑` / `↓` | Navigate search results |
| `Enter` | Open selected result in right panel |
| `Esc` | Close right panel, or clear focused input |
| `Cmd/Ctrl + ,` | Open Settings |
| `Cmd/Ctrl + B` | Toggle left nav collapse |
| `Cmd/Ctrl + /` | Toggle right panel |

Shown in a `Cmd/Ctrl + ?` cheat sheet modal (Phase 2 addition; not
required for Phase 1).

---

## Accessibility Baseline

- All interactive elements reachable via keyboard (Tab order, focus
  rings visible and accent-colored)
- Contrast ratios meet WCAG AA for body text (4.5:1)
- Semantic HTML: `<nav>`, `<main>`, `<aside>` regions present
- Screen reader labels on icon-only buttons (`aria-label`)
- No color-only information: corpus badges also carry text labels

Not in scope for Phase 1: full WCAG AAA, screen reader-optimized
flows, high-contrast theme. Revisit once content surfaces stabilize.

---

## Explicitly Out of Scope (Phase 1)

- Project dashboard UI (Phase 4)
- Update Tracker UI (Phase 5)
- Log viewer UI (Phase 6)
- MCP settings UI (Phase 7)
- Settings page beyond single-field Hytale path (grows with features)
- Command palette (Phase 2)
- Multi-select corpus filters (Phase 3)
- Per-result actions menu (copy path, show in file explorer, etc.)
- Animations beyond 150ms opacity fades
- Responsive layouts below 1024px width
- Custom window chrome
- Light-mode tuning beyond system-default inversion
- Onboarding tour / guided walkthrough

---

## Appendix - Design Token File

Create `src/styles/tokens.css` in Phase 1 with the CSS variables from
[Color Palette](#color-palette-starting-values-tune-for-wcag-aa-contrast)
above. All component styling references these variables via Tailwind
theme config. No component has hardcoded color values.

Dark-mode values are the defaults. Light-mode values are added as
`[data-theme='light']` overrides once the dark theme stabilizes
(Phase 2+).

---

*End of UI spec.*
