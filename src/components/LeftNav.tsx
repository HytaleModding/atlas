import {
  Search,
  Library,
  FolderKanban,
  GitCompareArrows,
  ScrollText,
  Settings,
  Sparkles,
  Pin,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useBranchStore } from "@/state/branchStore";
import { useIndexStore } from "@/state/indexStore";
import { useFetchStore } from "@/state/fetchStore";
import { useNavStore, type PageId } from "@/state/navStore";
import { useFeedbackStore } from "@/state/feedbackStore";
import { usePinsStore } from "@/state/pinsStore";
import { useTabsStore } from "@/state/tabsStore";
import type { Pin as PinRow } from "@/lib/state";
import type { Slot } from "@/lib/patcher";
import type { SearchHit } from "@/lib/indexer";
import { BranchCard } from "./BranchCard";

type NavItem = {
  id: string;
  label: string;
  icon: React.ComponentType<{ size?: number; strokeWidth?: number }>;
  /** If set, clicking the row routes to this page. Omit for disabled rows. */
  page?: PageId;
  disabled?: boolean;
  phaseNote?: string;
};

/** Left-rail navigation per ui-spec.md § Navigation. */
const ITEMS: NavItem[] = [
  { id: "search", label: "Search", icon: Search, page: "search" },
  { id: "catalog", label: "Library", icon: Library, page: "catalog" },
  {
    id: "projects",
    label: "Projects",
    icon: FolderKanban,
    page: "catalog",
  },
  {
    id: "tracker",
    label: "Tracker",
    icon: GitCompareArrows,
    page: "diff",
  },
  {
    id: "logs",
    label: "Logs",
    icon: ScrollText,
    disabled: true,
    phaseNote: "Coming soon",
  },
];

const SETTINGS_ITEM: NavItem = {
  id: "settings",
  label: "Settings",
  icon: Settings,
  page: "settings",
};

export function LeftNav() {
  return (
    <nav
      className="flex shrink-0 flex-col border-r border-border-subtle bg-bg-surface"
      style={{ width: "var(--nav-width)" }}
    >
      <div className="flex h-14 items-center px-4 border-b border-border-subtle">
        <span className="font-display text-xl tracking-wide text-fg-primary">
          Atlas
        </span>
      </div>
      <BranchToggle />
      <div className="px-2 pb-2">
        <BranchCard />
      </div>
      <ul className="flex flex-col gap-0.5 p-2">
        {ITEMS.map((item) => (
          <NavRow key={item.id} item={item} />
        ))}
      </ul>
      <PinnedSection />
      <div className="flex-1" />
      <ul className="flex flex-col gap-0.5 p-2 border-t border-border-subtle">
        <FeedbackRow />
        <NavRow item={SETTINGS_ITEM} />
      </ul>
    </nav>
  );
}

/** Sits just above the Settings row. Opens the feedback modal in
 *  Search Tuning mode by default since that's the higher-leverage path
 *  (every report becomes a labeled tuning case we can act on). */
function FeedbackRow() {
  const show = useFeedbackStore((s) => s.show);
  return (
    <li>
      <button
        type="button"
        onClick={() => show("tuning")}
        title="Send a bug report or flag a search result that ranked badly"
        className={cn(
          "flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm",
          "text-fg-secondary transition-colors duration-150",
          "hover:bg-bg-elevated hover:text-fg-primary",
        )}
      >
        <Sparkles size={16} strokeWidth={1.75} />
        <span>Help us improve Atlas</span>
      </button>
    </li>
  );
}

/**
 * Two-pill toggle at the top of the nav. Flipping it rewrites
 * `active_branch` in config so the choice persists across restarts.
 * The version line beneath mirrors whichever slot is live: either the
 * `Implementation-Version` from the JAR manifest or a configure-me hint.
 *
 * Always visible. A side without data yet renders greyed/disabled so
 * the user knows the slot exists but can't be selected until they pull
 * its data.
 */
function BranchToggle() {
  const active = useBranchStore((s) => s.active);
  const setActive = useBranchStore((s) => s.set);
  const overview = useIndexStore((s) => s.overview);
  const indexActiveSlot = useIndexStore((s) => s.activeSlot);
  const mounted = useFetchStore((s) => s.mounted);

  // Builds that arrived via "Update reference data" only show up in
  // `useFetchStore.mounted`, not the local-indexer overview, so we
  // count both sources.
  const mountedRelease = mounted.some(
    (m) => m.manifest.hytale_patchline === "release",
  );
  const mountedPreRelease = mounted.some(
    (m) => m.manifest.hytale_patchline === "pre-release",
  );

  const releaseHasData =
    !!overview?.release?.ready ||
    indexActiveSlot === "release" ||
    mountedRelease;
  const preReleaseHasData =
    !!overview?.pre_release?.ready ||
    indexActiveSlot === "pre-release" ||
    mountedPreRelease;

  return (
    <div className="px-3 pt-3 pb-2">
      <div className="flex rounded-md bg-bg-base p-0.5 text-xs">
        <BranchPill
          label="Release"
          selected={active === "release"}
          hasData={releaseHasData}
          onClick={() => void setActive("release")}
        />
        <BranchPill
          label="Pre-release"
          selected={active === "pre-release"}
          hasData={preReleaseHasData}
          onClick={() => void setActive("pre-release")}
        />
      </div>
    </div>
  );
}

function BranchPill({
  label,
  selected,
  hasData,
  onClick,
}: {
  label: string;
  selected: boolean;
  hasData: boolean;
  onClick: () => void;
}) {
  // We deliberately keep the pill clickable even when its slot has no
  // data yet: switching to it routes BranchCard into its
  // "Not configured" / install-and-prepare flow, which is how the user
  // sets the slot up in the first place.
  return (
    <button
      type="button"
      onClick={onClick}
      title={!hasData ? `No ${label} data yet - click to set up` : undefined}
      className={cn(
        "flex-1 rounded px-2 py-1 transition-colors",
        selected
          ? "bg-bg-elevated text-fg-primary shadow-sm"
          : hasData
            ? "text-fg-secondary hover:text-fg-primary"
            : "text-fg-muted opacity-60 hover:opacity-100 hover:text-fg-secondary",
      )}
    >
      {label}
    </button>
  );
}

/** "Pinned" section under the main nav. Lists every file pin sorted
 *  newest-first. Hidden entirely until at least one pin exists so the
 *  rail stays clean for fresh installs. Clicking a row opens the pinned
 *  file in the viewer; the small × removes the pin. Query and symbol
 *  pins are ignored here for now - those surface inside the search
 *  input dropdown (planned next). */
function PinnedSection() {
  const pins = usePinsStore((s) => s.pins);
  const remove = usePinsStore((s) => s.remove);
  const openTab = useTabsStore((s) => s.openTab);
  const setPage = useNavStore((s) => s.setPage);

  const filePins = pins.filter((p) => p.kind === "file");
  if (filePins.length === 0) return null;

  function openPin(p: PinRow) {
    // The pin only knows the file's path + the slot it belonged to.
    // Build a minimal SearchHit so the viewer can render it; the
    // backend's read_source command only needs slot, path, source_type.
    // preview_line stays null so the viewer scrolls to the top.
    const slot = pinSlot(p);
    const sourceType = guessSourceType(p.target);
    const hit: SearchHit = {
      slot,
      source_type: sourceType,
      path: p.target,
      fqn: "",
      package: "",
      filename: filenameOf(p.target),
      score: 0,
      line_count: 0,
      preview_line: null,
      preview: null,
      chunk_kind: "",
      symbol_name: "",
      start_line: null,
      end_line: null,
      debug: null,
    };
    openTab(hit);
    setPage("search");
  }

  return (
    <div className="flex max-h-64 flex-col border-t border-border-subtle">
      <div className="flex items-center gap-2 px-3 pt-3 pb-1 text-[11px] uppercase tracking-wide text-fg-muted">
        <Pin size={11} strokeWidth={1.75} />
        <span>Pinned</span>
      </div>
      <ul className="flex flex-col gap-0.5 overflow-y-auto px-2 pb-2">
        {filePins.map((p) => (
          <PinnedRow
            key={p.id}
            pin={p}
            onOpen={() => openPin(p)}
            onRemove={() => void remove(p.id)}
          />
        ))}
      </ul>
    </div>
  );
}

function PinnedRow({
  pin,
  onOpen,
  onRemove,
}: {
  pin: PinRow;
  onOpen: () => void;
  onRemove: () => void;
}) {
  const label = pin.label ?? filenameOf(pin.target);
  return (
    <li className="group flex items-center">
      <button
        type="button"
        onClick={onOpen}
        title={pin.target}
        className={cn(
          "flex min-w-0 flex-1 items-center gap-2 rounded-md px-3 py-1.5 text-left text-xs",
          "text-fg-secondary transition-colors duration-150",
          "hover:bg-bg-elevated hover:text-fg-primary",
        )}
      >
        <span className="truncate">{label}</span>
      </button>
      <button
        type="button"
        onClick={onRemove}
        title="Unpin"
        aria-label="Unpin"
        className={cn(
          "shrink-0 rounded-sm p-1 text-fg-muted opacity-0 transition-opacity",
          "group-hover:opacity-100 hover:bg-bg-elevated hover:text-fg-primary",
        )}
      >
        <X size={12} strokeWidth={1.75} />
      </button>
    </li>
  );
}

/** Best-effort recovery of the slot the pin was made under. State stores
 *  it as plain text so we cross-check it against the two known values
 *  and fall back to "release" - safer than failing the open. */
function pinSlot(p: PinRow): Slot {
  return p.build_id === "pre-release" ? "pre-release" : "release";
}

/** Pick a source_type from the file extension. Markdown gets routed to
 *  the prose renderer; everything else falls through to the source code
 *  viewer (which works for `.java` and unknown extensions alike). */
function guessSourceType(path: string): string {
  if (path.endsWith(".md")) return "hm_doc";
  return "source";
}

function filenameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function NavRow({ item }: { item: NavItem }) {
  const Icon = item.icon;
  const page = useNavStore((s) => s.page);
  const setPage = useNavStore((s) => s.setPage);
  const active = item.page !== undefined && item.page === page;
  return (
    <li>
      <button
        type="button"
        disabled={item.disabled}
        title={item.phaseNote ?? item.label}
        onClick={() => {
          if (item.page) setPage(item.page);
        }}
        className={cn(
          "flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm",
          "transition-colors duration-150",
          active && "bg-bg-elevated text-accent-primary",
          !active && !item.disabled && "text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary",
          item.disabled && "cursor-not-allowed text-fg-muted opacity-40",
        )}
      >
        <Icon size={16} strokeWidth={1.75} />
        <span>{item.label}</span>
      </button>
    </li>
  );
}
