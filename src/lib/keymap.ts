import { useEffect } from "react";
import { useNavStore, type PageId } from "@/state/navStore";

/** Where a key binding is allowed to fire.
 *
 *  - `global`   : always (even if the user is typing in an input)
 *  - `app`      : everywhere except text inputs / contenteditable
 *  - `search-page` : only when the Search page is active
 *  - `viewer`   : only when the file viewer (RightPanel) is mounted
 *
 *  Scopes compose with the optional `when` predicate so callers can
 *  layer additional conditions (e.g. a hit is selected). */
export type KeymapScope = "global" | "app" | "search-page" | "viewer";

export type KeyBinding = {
  /** KeyboardEvent.key value. Use lowercase for letters; symbolic keys
   *  like "ArrowLeft" / "Escape" / "F3" / "?" are passed through. */
  key: string;
  ctrl?: boolean;
  meta?: boolean;
  shift?: boolean;
  alt?: boolean;
  scope: KeymapScope;
  /** Extra condition. Return false to skip this binding even when the
   *  scope matches (e.g. "only when a hit is selected"). */
  when?: () => boolean;
  handler: (e: KeyboardEvent) => void;
  /** Whether to call preventDefault when the binding fires. Default true. */
  preventDefault?: boolean;
};

function isEditableTarget(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  if (t.isContentEditable) return true;
  const tag = t.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return false;
}

function scopeAllows(scope: KeymapScope, page: PageId, target: EventTarget | null): boolean {
  if (scope === "global") return true;
  if (isEditableTarget(target)) return false;
  if (scope === "app") return true;
  if (scope === "search-page") return page === "search";
  if (scope === "viewer") return true;
  return false;
}

function modifiersMatch(b: KeyBinding, e: KeyboardEvent): boolean {
  // Treat "undefined" as "must be false" so a binding without ctrl set
  // doesn't fire on Ctrl+key.
  if (!!b.ctrl !== e.ctrlKey) return false;
  if (!!b.meta !== e.metaKey) return false;
  if (!!b.shift !== e.shiftKey) return false;
  if (!!b.alt !== e.altKey) return false;
  return true;
}

/** Register a set of key bindings for the lifetime of the calling
 *  component. Cleans up on unmount. Re-registers when `deps` changes
 *  so handlers can capture fresh closures.
 *
 *  Bindings are checked in order; the first match wins. Pass higher-
 *  priority bindings first.
 *
 *  Why not a global registry: keymap behavior is component-driven
 *  (search-page bindings only exist while the search page is mounted;
 *  viewer bindings only while the viewer is open). A registry would
 *  re-implement React's lifecycle for no benefit.
 */
export function useKeymap(bindings: KeyBinding[], deps: ReadonlyArray<unknown>): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const page = useNavStore.getState().page;
      for (const b of bindings) {
        if (b.key !== e.key && b.key.toLowerCase() !== e.key.toLowerCase()) continue;
        if (!modifiersMatch(b, e)) continue;
        if (!scopeAllows(b.scope, page, e.target)) continue;
        if (b.when && !b.when()) continue;
        if (b.preventDefault !== false) e.preventDefault();
        b.handler(e);
        return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}
