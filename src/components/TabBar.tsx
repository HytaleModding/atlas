import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useTabsStore, tabIdOf } from "@/state/tabsStore";
import type { SearchHit } from "@/lib/indexer";

/** Source section → CSS variable name. Mirrors RightPanel.sectionVar. */
function sectionVar(sourceType: string): string {
  switch (sourceType) {
    case "hm_doc":
      return "--section-guides";
    case "hypixel_doc":
      return "--section-javadocs";
    case "asset":
      return "--section-assets";
    case "source":
    case "":
    default:
      return "--section-source";
  }
}

function tabLabel(hit: SearchHit): string {
  if (hit.filename) return hit.filename;
  const tail = hit.fqn?.split(".").pop();
  if (tail) return tail;
  const segs = hit.path.split(/[\\/]/).filter(Boolean);
  return segs[segs.length - 1] ?? hit.path;
}

/** Top-of-RightPanel tab strip. Each opened search hit becomes a tab;
 *  clicking flips the active tab; the × closes it. */
export function TabBar() {
  const tabs = useTabsStore((s) => s.tabs);
  const activeId = useTabsStore((s) => s.activeId);
  const setActive = useTabsStore((s) => s.setActive);
  const closeTab = useTabsStore((s) => s.closeTab);

  if (tabs.length === 0) return null;

  return (
    <div
      role="tablist"
      className="flex shrink-0 items-stretch gap-px overflow-x-auto border-b border-border-subtle bg-bg-base"
      // 40px reserves 8px under the tab row so the horizontal scrollbar
      // (when many tabs are open) doesn't crush the tabs themselves.
      style={{ height: "40px" }}
    >
      {tabs.map((hit) => {
        const id = tabIdOf(hit);
        const active = id === activeId;
        const stripeColor = `var(${sectionVar(hit.source_type)})`;
        const label = tabLabel(hit);
        return (
          <div
            key={id}
            role="tab"
            aria-selected={active}
            className={cn(
              "group relative flex max-w-[220px] shrink-0 items-center gap-1.5 px-3",
              "border-r border-border-subtle text-xs",
              active
                ? "bg-bg-elevated font-medium text-fg-primary"
                : "bg-bg-base text-fg-secondary hover:bg-bg-surface",
            )}
          >
            {/* Top edge: accent bar on the active tab so the focused tab is
                obvious at a glance, not just a faint background tint. */}
            {active && (
              <span
                aria-hidden
                className="absolute left-0 right-0 top-0 h-0.5 bg-accent-primary"
              />
            )}
            <span
              aria-hidden
              className="absolute left-0 top-0 h-full w-0.5"
              style={{ background: stripeColor }}
            />
            <button
              type="button"
              onClick={() => setActive(id)}
              title={hit.path}
              className="min-w-0 flex-1 truncate text-left font-mono"
            >
              {label}
            </button>
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(id);
              }}
              aria-label="Close tab"
              className={cn(
                "shrink-0 rounded-sm p-0.5 text-fg-muted",
                "hover:bg-bg-elevated hover:text-fg-primary",
                active ? "" : "opacity-0 group-hover:opacity-100",
              )}
            >
              <X size={12} strokeWidth={1.75} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
