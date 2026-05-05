import {
  createHighlighterCore,
  type HighlighterCore,
} from "shiki/core";
import { createOnigurumaEngine } from "shiki/engine/oniguruma";
// `shiki/wasm` re-exports `@shikijs/engine-oniguruma/wasm-inlined`, which
// embeds onig.wasm as base64 in the JS bundle. The default `createHighlighter`
// from "shiki" tries to fetch the WASM at runtime via `new URL(...,
// import.meta.url)`, which fails inside a Tauri production bundle (the
// `tauri://` asset protocol can't resolve the relative URL). Inlining the
// WASM ships everything inside the JS bundle Vite already emits, so syntax
// highlighting works identically in `tauri dev` and the packaged app.
import wasm from "shiki/wasm";

/** Shared shiki singleton. SourceCode.tsx and MarkdownView's code blocks
 *  both need a highlighter; loading the WASM and grammars twice would
 *  waste several megabytes. */

// Dynamic-import each grammar/theme so Vite code-splits them and only the
// ones we actually use end up in the bundle. The keys here drive the
// public `langForPath` mapping below.
const LANG_LOADERS = {
  java: () => import("@shikijs/langs/java"),
  json: () => import("@shikijs/langs/json"),
  javascript: () => import("@shikijs/langs/javascript"),
  typescript: () => import("@shikijs/langs/typescript"),
  tsx: () => import("@shikijs/langs/tsx"),
  kotlin: () => import("@shikijs/langs/kotlin"),
  rust: () => import("@shikijs/langs/rust"),
  python: () => import("@shikijs/langs/python"),
  bash: () => import("@shikijs/langs/bash"),
  yaml: () => import("@shikijs/langs/yaml"),
  toml: () => import("@shikijs/langs/toml"),
  xml: () => import("@shikijs/langs/xml"),
  html: () => import("@shikijs/langs/html"),
  css: () => import("@shikijs/langs/css"),
  scala: () => import("@shikijs/langs/scala"),
  groovy: () => import("@shikijs/langs/groovy"),
  markdown: () => import("@shikijs/langs/markdown"),
} as const;

export type SupportedLang = keyof typeof LANG_LOADERS;

const SUPPORTED_LANGS = Object.keys(LANG_LOADERS) as SupportedLang[];

const THEME = "github-dark-default";

let cached: HighlighterCore | null = null;
let cachedPromise: Promise<HighlighterCore> | null = null;

export async function ensureHighlighter(): Promise<HighlighterCore> {
  if (cached) return cached;
  if (!cachedPromise) {
    cachedPromise = createHighlighterCore({
      themes: [import("@shikijs/themes/github-dark-default")],
      langs: SUPPORTED_LANGS.map((id) => LANG_LOADERS[id]()),
      engine: createOnigurumaEngine(wasm),
    }).then((h) => {
      cached = h;
      return h;
    });
  }
  return cachedPromise;
}

/** Render `code` to a self-contained HTML string with shiki. The result
 *  is `<pre class="shiki ...">…</pre>` and is safe to inject because the
 *  input came from local content we trust. */
export async function highlightToHtml(
  code: string,
  lang: string,
): Promise<string> {
  const h = await ensureHighlighter();
  const langId = (SUPPORTED_LANGS as string[]).includes(lang)
    ? (lang as SupportedLang)
    : "java";
  return h.codeToHtml(code, { lang: langId, theme: THEME });
}

export const SHIKI_THEME = THEME;
export { SUPPORTED_LANGS };
