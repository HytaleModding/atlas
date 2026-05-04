import { useEffect, useState } from "react";
import { ImageOff, X } from "lucide-react";

// Screenshots live in `public/guide/` so Vite serves them verbatim, that
// way missing files just show a placeholder instead of blowing up the
// build. Drop `install-prerelease-step-{1,2}.png` in there when ready.
const STEP_1_SRC = "/guide/install-prerelease-step-1.png";
const STEP_2_SRC = "/guide/install-prerelease-step-2.png";

/**
 * Two-step walkthrough shown when the user has no pre-release install.
 * Content is static by design. Hytale's launcher UI rarely moves, and
 * the screenshots are shipped in the bundle. Closes on Esc or backdrop
 * click for parity with the first-run modal.
 */
export function InstallPrereleaseGuide({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-8"
      onClick={onClose}
    >
      <div
        className="flex max-h-full w-full max-w-2xl flex-col overflow-hidden rounded-lg border border-border-subtle bg-bg-surface shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-center justify-between border-b border-border-subtle px-5 py-3">
          <h2 className="font-display text-base text-fg-primary">
            Install the Hytale pre-release
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md p-1 text-fg-secondary hover:bg-bg-elevated hover:text-fg-primary"
            aria-label="Close"
          >
            <X size={16} strokeWidth={1.75} />
          </button>
        </header>

        <div className="flex flex-col gap-5 overflow-auto p-5">
          <Step
            number={1}
            title="Open launcher settings"
            body="In the Hytale launcher, click the gear icon in the top-right corner."
            image={STEP_1_SRC}
            alt="Hytale launcher with the gear icon highlighted"
          />
          <Step
            number={2}
            title="Switch the Patchline to Pre-release"
            body="Open the Patchline dropdown and choose Pre-release. The launcher will download the pre-release install alongside your release one."
            image={STEP_2_SRC}
            alt="Hytale launcher settings showing the Patchline dropdown set to Pre-release"
          />
          <p className="text-xs text-fg-muted">
            Once Hytale finishes installing the pre-release, return to Atlas
            and pick the new install folder with <em>Pick folder manually</em>,
            or Atlas will detect it automatically on next startup.
          </p>
        </div>
      </div>
    </div>
  );
}

function Step({
  number,
  title,
  body,
  image,
  alt,
}: {
  number: number;
  title: string;
  body: string;
  image: string;
  alt: string;
}) {
  const [broken, setBroken] = useState(false);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-accent-primary text-[11px] font-semibold text-accent-primary-fg">
          {number}
        </span>
        <h3 className="text-sm font-medium text-fg-primary">{title}</h3>
      </div>
      <p className="text-xs text-fg-muted">{body}</p>
      {broken ? (
        <div className="flex h-40 w-full items-center justify-center gap-2 rounded-md border border-dashed border-border-subtle text-xs text-fg-muted">
          <ImageOff size={14} strokeWidth={1.75} />
          <span className="font-mono">{image}</span>
        </div>
      ) : (
        <img
          src={image}
          alt={alt}
          onError={() => setBroken(true)}
          className="w-full rounded-md border border-border-subtle"
        />
      )}
    </div>
  );
}
