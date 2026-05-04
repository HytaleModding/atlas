import {
  Search,
  Library,
  FolderKanban,
  GitCompareArrows,
  ScrollText,
  Settings,
  Sparkles,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useBranchStore } from "@/state/branchStore";
import { useIndexStore } from "@/state/indexStore";
import { useNavStore, type PageId } from "@/state/navStore";
import { useFeedbackStore } from "@/state/feedbackStore";
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
    disabled: true,
    phaseNote: "Coming soon",
  },
  {
    id: "tracker",
    label: "Tracker",
    icon: GitCompareArrows,
    disabled: true,
    phaseNote: "Coming soon",
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
      <ul className="flex flex-1 flex-col gap-0.5 p-2">
        {ITEMS.map((item) => (
          <NavRow key={item.id} item={item} />
        ))}
      </ul>
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
 * Hidden entirely when only one slot has data loaded - showing a
 * toggle that has nothing to flip to is just visual noise. As soon as
 * the second slot becomes ready (or starts loading) the toggle
 * reappears.
 */
function BranchToggle() {
  const active = useBranchStore((s) => s.active);
  const setActive = useBranchStore((s) => s.set);
  const overview = useIndexStore((s) => s.overview);
  const indexActiveSlot = useIndexStore((s) => s.activeSlot);

  const releaseHasData =
    !!overview?.release?.ready || indexActiveSlot === "release";
  const preReleaseHasData =
    !!overview?.pre_release?.ready || indexActiveSlot === "pre-release";

  if (!(releaseHasData && preReleaseHasData)) return null;

  return (
    <div className="px-3 pt-3 pb-2">
      <div className="flex rounded-md bg-bg-base p-0.5 text-xs">
        <BranchPill
          label="Release"
          selected={active === "release"}
          onClick={() => void setActive("release")}
        />
        <BranchPill
          label="Pre-release"
          selected={active === "pre-release"}
          onClick={() => void setActive("pre-release")}
        />
      </div>
    </div>
  );
}

function BranchPill({
  label,
  selected,
  onClick,
}: {
  label: string;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex-1 rounded px-2 py-1 transition-colors",
        selected
          ? "bg-bg-elevated text-fg-primary shadow-sm"
          : "text-fg-secondary hover:text-fg-primary",
      )}
    >
      {label}
    </button>
  );
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
