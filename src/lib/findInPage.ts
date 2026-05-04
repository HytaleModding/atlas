/** Find-in-page utilities for the file viewer.
 *
 *  Walks the rendered DOM under a container, wraps every text-node
 *  occurrence of `query` in a `<mark class="atlas-find">` span, and
 *  returns the resulting mark elements so the caller can cycle through
 *  them with next/prev + scrollIntoView.
 *
 *  Why DOM walking instead of e.g. a regex over the raw markdown:
 *    - Works uniformly for source-code (line spans), markdown (rich
 *      tree), and javadoc text: anything React already rendered.
 *    - Doesn't require knowing how the renderer structured the DOM;
 *      we just look at text nodes.
 *    - Highlights survive React re-renders gracefully because we
 *      always clear-then-rewrap on every `query`/content change.
 *
 *  Caveats:
 *    - Wrapping mutates the DOM, so React thinks the tree is dirty.
 *      We never run inside a React render, only useEffect after a
 *      paint, and we always clear our marks before the parent
 *      component re-mounts the children.
 *    - Matches are case-insensitive and split across text nodes are
 *      missed (e.g. "fooBar" split by a `<strong>` boundary won't
 *      match). Acceptable: the common case is "find a word inside a
 *      paragraph" and that works.
 */

const MARK_CLASS = "atlas-find";
const MIN_QUERY_LEN = 2;

/** Strip every `<mark class="atlas-find">` we previously added inside
 *  `root`, replacing each with its text content and re-merging adjacent
 *  text nodes via `Node.normalize()`. Safe to call when no marks exist. */
export function clearFindMarks(root: HTMLElement): void {
  const marks = root.querySelectorAll<HTMLElement>(`mark.${MARK_CLASS}`);
  if (marks.length === 0) return;
  for (const mark of marks) {
    const parent = mark.parentNode;
    if (!parent) continue;
    parent.replaceChild(
      document.createTextNode(mark.textContent ?? ""),
      mark,
    );
  }
  // Coalesce any adjacent text nodes our replacements created, keeps
  // the next walk's nodeValue chunks the same shape they were before.
  root.normalize();
}

/** Walk every text node under `root` and wrap each case-insensitive
 *  occurrence of `query` in a `<mark>`. Returns the marks in document
 *  order so the caller can step through them with index arithmetic.
 *
 *  Skips queries shorter than [`MIN_QUERY_LEN`] to avoid the "highlight
 *  every letter" disaster while the user is typing. Returns `[]` for
 *  empty/short queries.
 */
export function applyFindMarks(
  root: HTMLElement,
  query: string,
): HTMLElement[] {
  if (query.length < MIN_QUERY_LEN) return [];
  const lowerQuery = query.toLowerCase();

  // Collect text nodes first; we mutate while walking otherwise.
  const walker = document.createTreeWalker(
    root,
    NodeFilter.SHOW_TEXT,
    {
      acceptNode: (node) => {
        // Don't double-wrap: text already inside one of our marks
        // shouldn't be re-walked. Belt-and-braces, clearFindMarks
        // should have stripped them, but a stale render could leak.
        const parent = node.parentElement;
        if (parent?.classList.contains(MARK_CLASS)) {
          return NodeFilter.FILTER_REJECT;
        }
        return NodeFilter.FILTER_ACCEPT;
      },
    },
  );
  const targets: Text[] = [];
  let n: Node | null;
  while ((n = walker.nextNode())) targets.push(n as Text);

  const out: HTMLElement[] = [];
  for (const node of targets) {
    const text = node.nodeValue ?? "";
    if (text.length === 0) continue;
    const lower = text.toLowerCase();
    const indices: number[] = [];
    let i = 0;
    while ((i = lower.indexOf(lowerQuery, i)) !== -1) {
      indices.push(i);
      i += lowerQuery.length;
    }
    if (indices.length === 0) continue;

    const frag = document.createDocumentFragment();
    let cursor = 0;
    for (const idx of indices) {
      if (idx > cursor) {
        frag.appendChild(document.createTextNode(text.slice(cursor, idx)));
      }
      const mark = document.createElement("mark");
      mark.className = MARK_CLASS;
      mark.textContent = text.slice(idx, idx + lowerQuery.length);
      frag.appendChild(mark);
      out.push(mark);
      cursor = idx + lowerQuery.length;
    }
    if (cursor < text.length) {
      frag.appendChild(document.createTextNode(text.slice(cursor)));
    }
    node.parentNode?.replaceChild(frag, node);
  }
  return out;
}

/** Set the `data-active` attribute on each mark and scroll the active
 *  one into view. Pass `behavior: "instant"` on the first apply so the
 *  viewer doesn't animate from top-of-page; "smooth" when stepping
 *  next/prev so the user can track the motion. */
export function setActiveMark(
  marks: HTMLElement[],
  index: number,
  behavior: ScrollBehavior = "smooth",
): void {
  for (let i = 0; i < marks.length; i++) {
    marks[i].dataset.active = i === index ? "true" : "false";
  }
  const target = marks[index];
  if (target) {
    target.scrollIntoView({ block: "center", behavior });
  }
}
