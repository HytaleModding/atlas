# Atlas — User Experience Audit (Search V1)

**Scope:** the search experience only. Project tracking, the pre-release pipeline, and the log parser are out of scope by request. Audit is based on the actual UI source (SearchPage, RightPanel, SiblingPane, FindOverlay, SettingsPage, AppShell, LeftNav, StatusBar, KeyboardCheatsheet, FirstRunModal) plus README and plan.md.

## 1. The five-minute test

A new modder downloads Atlas, double-clicks the installer, opens it. What happens in the first five minutes?

**The good:** A welcome modal appears, prefilled with a detected Hytale path. They click Continue. Atlas auto-decompiles and indexes in the background. The empty SearchPage tells them, in plain language, "Atlas doesn't have Hytale release data yet. Atlas will download Hytale's source code, modding guides, and API docs automatically. Hang tight, this is on the way." A progress bar with friendly phase labels (Looking up Hytale data, Downloading, Setting up) ticks forward. When it finishes, the search input lights up. They type `PageManager`, see ranked results in a few hundred ms with corpus stripes down the left edge, and clicking opens a syntax-highlighted source view with a Javadoc box inlined right above the class declaration. The IdlePrompt suggests three real symbols (`PageManager`, `getComponent`, `ItemStack`) so they have something concrete to try.

**The flag:** the most powerful thing in the product (semantic / natural-language search) is not on the surface anywhere. The placeholder says "Search Hytale source…" and the IdlePrompt examples are all symbol names. A modder who reads the README sees "natural language queries bridge intent to symbol name for an undocumented API," but the UI never says it. They will type symbols, get good results, and conclude this is "fast IDE search," not "ask it what you want."

**Quit-likely moments:**
- They press Skip on the first-run modal. The status bar now says "Hytale: not configured." There is no obvious recovery hint on the empty SearchPage beyond a "Set up Hytale install" button. That button does work, but the failure state needs to feel like one click, not "where do I go now."
- The download is large and time-to-first-search is non-trivial. The progress UI is fine, but the user does not know if "Setting up" means 30 seconds or 30 minutes. No ETA, no byte rate, just bytes and percent.

**Delight moments that will land:**
- The married hit (source + Javadoc fused into one row with a split-color stripe).
- The inline Javadoc box dropping in above class and method declarations. This single feature is the most "oh, that's clever" thing in the app and it lands without any user effort.
- `?` cheatsheet covers j/k, Alt+arrow viewer history, 1–4 corpus chips, Ctrl+F, F3.

## 2. Promise vs. delivery

The README promises: one search bar over decompiled source, modding guides, asset packs, project source. Auto-update of app and data. No terminal, no Java, no manual decompile.

What ships in V1 search:
- Source: yes
- HM Guides: yes (with rendered MDX, TOC, "View on hytalemodding.dev" link)
- Hypixel Javadocs: yes, and notably *better* than the README implied because they're fused into source view
- Assets: chip is `disabled: true` with a "Coming soon" tooltip
- User project source: not present in any UI surface

The README's framing ("one unified workspace…asset pack inspection…") sets an expectation that Assets is a first-class corpus on day one. The chip being grayed-out is the right call (it's honest), but anyone who read the README will notice. The fix is one line of README copy, not a feature change. Everything else delivers or over-delivers.

## 3. Search quality from a user lens

**Typing latency.** The 180ms debounce plus stale-response cancellation feels right. The store preserves selection on the same file across keystrokes, which is the difference between "the viewer flickers while I type" and "the viewer feels stable."

**Ranking surprises.** The split-lane layout (code lane on top, HM Guides pinned to the bottom 25%) is a real opinion and it is the right one. Guides are conversational and would otherwise drown in code hits, or get lost when buried beneath them. The `#1` `#2` `#3` rank chips with the top-1 in the accent color give the user a visual gut-check on whether the model agrees with them. The `via docs` chip, when a source row was matched through Javadoc prose, is honest in a way most search UIs are not. Most products would silently re-rank and call it "AI." Atlas tells you why it surfaced this row. That maps directly to the "Honest, never magic" philosophy in plan.md.

**Married hits.** Splicing Javadoc and source into a single row with a two-tone stripe down the left edge is the headline feature of the search experience and it works. The fact that the Javadoc-only row is rewritten into a source row when a sibling exists (the `findSourceSiblings` substitution in SearchPage) is invisible to the user and exactly the right call.

**Inline Javadoc in source view.** This is the single best UX decision in the product. A modder reading a decompiled class sees the Javadoc dropped directly above each method, tinted with the Javadocs corpus color, with a "Javadoc inline / off" toggle pill if they want pure source. This is what every Hypixel modder wishes IntelliJ did out of the box.

**Keyboard ergonomics.** j/k/Enter/Esc/1–4/?/Alt+arrow/Ctrl+F/F3 is a complete keyboard story. The cheatsheet is reachable via `?` and the search input regains focus on Esc. The one missing primitive is `/` to focus search (vim/GitHub-style); the user has to click or rely on Esc-from-elsewhere.

**Recall surprises that will bite.** The vector store has a distance threshold, the BM25/vector blend uses RRF, and the limit defaults to 10 with a "Show more" pill capped at 100. The user never sees any of that, which is correct. But the "Show more" pill at the bottom of a long file group is easy to miss. A user who sees ten results, none of which are what they want, will conclude "Atlas doesn't know about this" rather than "I should click Show more."

## 4. Friction points

- **Branch toggle.** The left nav shows a Release / Pre-release pill toggle at the very top of the rail. For a brand-new user with only release data, this is a confusing piece of vocabulary right above their search bar. It should default-collapse or move into Hytale Data until both branches are populated.
- **"Hytale Data" page.** The IndexCatalog nav item exists alongside Search. A normal user does not need to think about indexes. Renaming it to something like "Library" or hiding it behind a small icon would push less-load onto a first-time user. The current label is a half-step toward jargon.
- **Debug toggle on the search bar.** A "Debug" button sits next to the corpus chips by default. It is disabled until searchable, but its presence in the primary chrome alongside [All] [Source] [Guides] [Javadocs] [Assets] is jargon that does not belong there. It belongs in Settings → Developer.
- **The placeholder copy is too narrow.** "Search Hytale source…" undersells the product. A user reading that won't think to type "how do I send a message to a player." Recommend "Search Hytale source, guides, and Javadocs…" or a rotating placeholder that includes one natural-language example.
- **No-results state.** Currently shows "No matches in {corpus}" plus three "Try X" pills. Solid. Could go further: if the corpus is "all" and zero hits, suggest the model may not have indexed yet, or offer a "did you mean" with the closest indexed symbol.
- **Loading flash on result switch.** When the user clicks between hits, the right panel shows "Loading…" centered briefly before content lands. Pre-fetching content for adjacent hits in the result list would make j/k navigation feel instant.

## 5. Praise-worthiness

Would a modder recommend this to another modder after one afternoon? Yes, but for a specific reason: the inline Javadoc fusion. That is the thing they will screenshot and Discord-paste. "Look, you just open a class and the docs are right there above each method." Everything else is competent and polished, but that one feature is the wedge.

Is this "in my permanent workflow"? Almost. The thing keeping it at "neat tool" instead of "indispensable" is the lack of any deep-link / share story. A modder reading source in Atlas cannot copy a URL and paste it in Discord with "look at this method." They can copy the FQN, copy the method signature, or open in editor, and that is enough for solo work but not for community work. Atlas is currently a personal tool, not a sharing tool.

## 6. Missing affordances (specific wins)

- **`/` focuses search.** Universal across GitHub, Discord, Slack. Currently Esc does it, which is half-right.
- **`atlas://` deep-link copy.** "Copy link to this hit" header button. Pastes back into a fresh Atlas instance and jumps straight to file + line. Optional v1.5: `atlas-web://` once Phase 8 lands.
- **Surface the "Show more" pill better.** When `hits.length === limit`, render a faint "+ N more results hidden" line directly beneath the last hit, not just the pill. Users miss the pill.
- **Rotate the IdlePrompt examples between symbols and natural language.** "Try `PageManager`, `ItemStack`, or `how do I show a message to a player`." This is the single highest-leverage copy change in the product.
- **Recent files list in viewer.** The viewer has Alt+← / Alt+→ history. A small dropdown listing the last 10 files in viewer history would give users a visible sense of where they've been.
- **"Open both" on a married hit.** Shift-click on a married row could open source in the main viewer and Javadoc in the SiblingPane simultaneously, instead of forcing the user to swap.
- **Persistent "indexing in background" badge.** The StatusBar carries this, but it's small and gray. A first-time user mid-index will type a query, see disabled state, and not connect "Setting up search" in the empty area to the gray text in the bottom strip. A toast on completion ("Atlas is ready, your release data is searchable") would close the loop.
- **Rename "Hytale Data" to "Library"** or hide it under a gear icon. "Data" is jargon-adjacent.
- **Move the Debug toggle out of primary chrome.** Settings → Developer is the right home, with a keyboard shortcut for power users.
- **First-run skip recovery.** If the user clicked Skip, the empty-SearchPage button "Set up Hytale install" should be more prominent and the headline should read "Hytale isn't connected yet" instead of "Atlas doesn't have Hytale release data yet" (which sounds like a wait, not a fix).
- **README copy fix.** Either downgrade Assets in the README pillar list to "coming soon" or ship the assets corpus before launch. Current text creates a small but real expectation gap.

## Bottom line

The search surface is in unusually good shape for a pre-alpha product. The hard UX decisions (corpus stripes, married hits, inline Javadoc, split lane for guides, honest debug breakdowns, keyboard-first nav) are the right ones, and they're already implemented. The gaps are almost entirely about *discoverability of what's already there*: the natural-language story is hidden, the Show more pill is buried, the branch toggle is confusing for a first-time user, and there is no shareable link out of a hit. Fix those and Atlas crosses from "demo well" to "modders quietly install it and stop alt-tabbing."
