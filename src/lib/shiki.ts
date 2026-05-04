import { createHighlighter, type BundledLanguage, type Highlighter } from "shiki";

/** Shared shiki singleton. SourceCode.tsx and MarkdownView's code blocks
 *  both need a highlighter; loading the WASM and grammars twice would
 *  waste several megabytes. */

const SUPPORTED_LANGS: BundledLanguage[] = [
  "java",
  "json",
  "javascript",
  "typescript",
  "tsx",
  "kotlin",
  "rust",
  "python",
  "bash",
  "yaml",
  "toml",
  "xml",
  "html",
  "css",
  "scala",
  "groovy",
  "markdown",
];

const THEME = "github-dark-default";

let cached: Highlighter | null = null;
let cachedPromise: Promise<Highlighter> | null = null;

export async function ensureHighlighter(): Promise<Highlighter> {
  if (cached) return cached;
  if (!cachedPromise) {
    cachedPromise = createHighlighter({
      themes: [THEME],
      langs: SUPPORTED_LANGS,
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
  const langId = SUPPORTED_LANGS.includes(lang as BundledLanguage)
    ? (lang as BundledLanguage)
    : "java";
  return h.codeToHtml(code, { lang: langId, theme: THEME });
}

export const SHIKI_THEME = THEME;
