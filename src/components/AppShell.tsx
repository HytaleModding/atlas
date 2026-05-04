import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { Toaster, toast } from "sonner";
import { LeftNav } from "./LeftNav";
import { StatusBar } from "./StatusBar";
import { RightPanel } from "./RightPanel";
import { FirstRunModal } from "./FirstRunModal";
import { FeedbackModal } from "./FeedbackModal";
import { PageErrorBoundary } from "./PageErrorBoundary";
import { SearchPage } from "@/pages/SearchPage";
import { IndexCatalog } from "@/pages/IndexCatalog";
import { SettingsPage } from "@/pages/SettingsPage";
import { DiffReportPage } from "@/pages/DiffReport";
import { useConfigStore } from "@/state/configStore";
import { useBranchStore } from "@/state/branchStore";
import { useOverviewStore } from "@/state/overviewStore";
import { usePatcherStore } from "@/state/patcherStore";
import { useIndexStore } from "@/state/indexStore";
import { useFetchStore } from "@/state/fetchStore";
import { useProjectStore } from "@/state/projectStore";
import { useNavStore } from "@/state/navStore";
import { usePinsStore } from "@/state/pinsStore";
import { needsFirstRun } from "@/lib/config";
import type { Slot } from "@/lib/patcher";

/**
 * The three-region desktop shell defined in docs/ui-spec.md § Layout.
 *
 *  +-----+------------------+--------------+
 *  | Nav |  Main content    | Right panel  |
 *  |     |  (active page)   | (file viewer)|
 *  +-----+------------------+--------------+
 *  |             Status bar                |
 *  +---------------------------------------+
 *
 * The first-run modal overlays everything until the user picks a Hytale
 * install or explicitly skips.
 */
export function AppShell() {
  const snapshot = useConfigStore((s) => s.snapshot);
  const loading = useConfigStore((s) => s.loading);
  const configError = useConfigStore((s) => s.error);
  const refreshConfig = useConfigStore((s) => s.refresh);
  const hydrateBranch = useBranchStore((s) => s.hydrate);
  const branchHydrated = useBranchStore((s) => s.hydrated);
  const refreshOverview = useOverviewStore((s) => s.refresh);
  const subscribePatcher = usePatcherStore((s) => s.subscribe);
  const subscribeIndex = useIndexStore((s) => s.subscribe);
  const subscribeFetch = useFetchStore((s) => s.subscribe);
  const subscribeProjects = useProjectStore((s) => s.subscribe);
  const refreshProjects = useProjectStore((s) => s.refresh);
  const refreshIndexOverview = useIndexStore((s) => s.refreshOverview);
  const startIndex = useIndexStore((s) => s.start);
  const refreshPins = usePinsStore((s) => s.refresh);
  const page = useNavStore((s) => s.page);
  const refreshCatalog = useFetchStore((s) => s.refreshCatalog);
  const checkForUpdates = useFetchStore((s) => s.checkForUpdates);
  const startFetch = useFetchStore((s) => s.start);

  // Boot: load config + pin snapshot once. Pins are independent of
  // config so they can hydrate in parallel.
  useEffect(() => {
    void refreshConfig();
    void refreshPins();
  }, [refreshConfig, refreshPins]);

  // Once config is in, hydrate branch state + pull the first overview.
  useEffect(() => {
    if (snapshot && !branchHydrated) {
      hydrateBranch();
      void refreshOverview();
      void refreshIndexOverview();
      void refreshCatalog();
    }
  }, [snapshot, branchHydrated, hydrateBranch, refreshOverview, refreshIndexOverview, refreshCatalog]);

  // After the catalog snapshot lands, ask the central repo what the
  // newest published builds are and pull anything we don't already have.
  // The user never has to click "Check for updates" — Atlas ships
  // pre-wired to the central index repo and just keeps current. We
  // skip pre-release auto-fetch when no pre-release Hytale install is
  // detected so users without that patchline enabled don't get data
  // they can't legally use; the BranchCard surfaces the install guide
  // for that case instead.
  const autoFetchRanRef = useRef(false);
  useEffect(() => {
    if (autoFetchRanRef.current) return;
    if (!snapshot) return;
    // Wait until the user has satisfied the entitlement gate (set
    // their Hytale install path at least once). Otherwise this fires
    // behind the FirstRunModal.
    if (needsFirstRun(snapshot)) return;
    autoFetchRanRef.current = true;
    void (async () => {
      // Refresh the local catalog first so the "already have it"
      // check against `mounted` doesn't race the boot-time refresh.
      await refreshCatalog();
      await checkForUpdates();
      const fetchState = useFetchStore.getState();
      const mountedNow = fetchState.mounted;
      const resolutions = fetchState.remoteResolutions;
      const hasPrereleaseInstall = !!snapshot.detected_prerelease_path;

      for (const slot of ["release", "pre-release"] as Slot[]) {
        if (slot === "pre-release" && !hasPrereleaseInstall) continue;
        const resolution = resolutions[slot];
        if (!resolution) continue;
        const alreadyMounted = mountedNow.some(
          (m) => m.build_id === resolution.build_id,
        );
        if (alreadyMounted) continue;
        try {
          await startFetch({
            buildId: resolution.build_id,
            url: resolution.url,
          });
        } catch (err) {
          // Surface via toast but keep going for the other slot; the
          // user can retry from IndexCatalog.
          toast.error(
            slot === "release"
              ? "Couldn't get Release data"
              : "Couldn't get Pre-release data",
            { description: String(err) },
          );
        }
      }
    })();
  }, [snapshot, checkForUpdates, startFetch, refreshCatalog]);

  // Subscribe to decompile + index events for the whole session. The
  // auto-kick listener runs *in addition* to the patcherStore subscriber
  // because we need the slot carried on the raw `decompile:done` payload;
  // the store zeroes `activeSlot` the moment the event lands.
  useEffect(() => {
    let unlistenPatcher: (() => void) | undefined;
    let unlistenIndex: (() => void) | undefined;
    let unlistenFetch: (() => void) | undefined;
    let unlistenProjects: (() => void) | undefined;
    let unlistenAutoKick: (() => void) | undefined;
    let unlistenIndexDoneToast: (() => void) | undefined;

    void (async () => {
      unlistenPatcher = await subscribePatcher();
      unlistenIndex = await subscribeIndex();
      unlistenFetch = await subscribeFetch();
      unlistenProjects = await subscribeProjects();
      void refreshProjects();

      unlistenAutoKick = await listen<{ slot: Slot; outputDir: string }>(
        "decompile:done",
        (event) => {
          const slot = event.payload.slot;
          void refreshOverview();

          const indexState = useIndexStore.getState();
          const summary =
            slot === "release"
              ? indexState.overview?.release
              : indexState.overview?.pre_release;
          const indexerBusy =
            indexState.status.kind === "phase" ||
            indexState.status.kind === "progress";
          if (!indexerBusy && (!summary || !summary.ready || summary.stale)) {
            void startIndex(slot);
          }
        },
      );

      // Toast when an index run finishes so users get explicit
      // confirmation the data is ready to search. The store also marks
      // the slot ready, but search doesn't pop the user back to the
      // page if they've wandered off, so the toast is the bridge.
      unlistenIndexDoneToast = await listen<{ slot: Slot; docs: number }>(
        "index:done",
        (event) => {
          const slotName =
            event.payload.slot === "release" ? "Release" : "Pre-release";
          toast.success(`${slotName} data ready to search`, {
            description: `${event.payload.docs.toLocaleString()} entries indexed.`,
          });
        },
      );
    })();

    return () => {
      unlistenPatcher?.();
      unlistenIndex?.();
      unlistenFetch?.();
      unlistenProjects?.();
      unlistenAutoKick?.();
      unlistenIndexDoneToast?.();
    };
  }, [
    subscribePatcher,
    subscribeIndex,
    subscribeFetch,
    subscribeProjects,
    refreshProjects,
    refreshOverview,
    startIndex,
  ]);

  const firstRun = snapshot !== null && needsFirstRun(snapshot);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-bg-base text-fg-primary">
      <div className="flex min-h-0 flex-1">
        <LeftNav />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          {loading ? (
            <div className="flex flex-1 items-center justify-center text-fg-muted">
              <span className="text-sm">Loading…</span>
            </div>
          ) : configError && !snapshot ? (
            <div className="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
              <p className="text-sm text-destructive">
                Couldn't load Atlas settings.
              </p>
              <p className="max-w-md font-mono text-xs text-fg-muted">
                {configError}
              </p>
              <button
                type="button"
                onClick={() => void refreshConfig()}
                className="rounded-md bg-accent-primary px-3 py-1.5 text-xs font-medium text-accent-primary-fg hover:brightness-110"
              >
                Retry
              </button>
            </div>
          ) : (
            <PageErrorBoundary key={page}>
              {page === "catalog" ? (
                <IndexCatalog />
              ) : page === "settings" ? (
                <SettingsPage />
              ) : page === "diff" ? (
                <DiffReportPage />
              ) : (
                <SearchPage />
              )}
            </PageErrorBoundary>
          )}
        </main>
        <RightPanel />
      </div>
      <StatusBar />
      {firstRun && <FirstRunModal />}
      <FeedbackModal />
      <Toaster
        theme="dark"
        position="top-right"
        duration={3000}
        toastOptions={{
          style: {
            background: "var(--bg-elevated)",
            border: "1px solid var(--border-subtle)",
            color: "var(--fg-primary)",
            fontSize: "12px",
          },
        }}
      />
    </div>
  );
}
