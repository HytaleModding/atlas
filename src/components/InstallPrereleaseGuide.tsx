import { useEffect, useState } from "react";
import { ImageOff, X } from "lucide-react";

// Screenshots live in `public/guide/` so Vite serves them verbatim, that
// way missing files just show a placeholder instead of blowing up the
// build. Drop `install-prerelease-step-{1,2}.png` in there when ready.
const STEP_1_SRC = "/guide/install-prerelease-step-2.png";
const STEP_2_SRC = "/guide/install-prerelease-step-1.png";

/**
 * Two-step walkthrough shown when the user has no pre-release install.
 * Both screenshots render side by side at equal size with a one-line
 * caption under each. Closes on Esc or backdrop click for parity with
 * the first-run modal.
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
        className="flex max-h-full w-full max-w-[min(1920px,90vw)] flex-col overflow-hidden rounded-lg border border-border-subtle bg-bg-surface shadow-2xl"
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
          <div className="grid grid-cols-2 gap-5">
            <Step
              number={1}
              caption="Click the gear next to Play."
              image={STEP_1_SRC}
              alt="Hytale launcher with the gear icon highlighted"
            />
            <Step
              number={2}
              caption="Open the Patchline dropdown and pick Pre-release."
              image={STEP_2_SRC}
              alt="Hytale launcher settings showing the Patchline dropdown set to Pre-release"
            />
          </div>
          <p className="text-xs text-fg-muted">
            Once Hytale finishes installing the pre-release, come back here
            and click <em>Pick folder</em> — or just relaunch Atlas and it
            will detect it automatically.
          </p>
        </div>
      </div>
    </div>
  );
}

function Step({
  number,
  caption,
  image,
  alt,
}: {
  number: number;
  caption: string;
  image: string;
  alt: string;
}) {
  const [broken, setBroken] = useState(false);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex h-[min(800px,60vh)] w-full items-center justify-center overflow-hidden rounded-md border border-border-subtle bg-bg-base">
        {broken ? (
          <div className="flex flex-col items-center justify-center gap-1 px-2 text-center text-[10px] text-fg-muted">
            <ImageOff size={14} strokeWidth={1.75} />
            <span className="break-all font-mono">{image}</span>
          </div>
        ) : (
          <img
            src={image}
            alt={alt}
            onError={() => setBroken(true)}
            className="max-h-full max-w-full object-contain"
          />
        )}
      </div>
      <div className="flex items-start gap-2">
        <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent-primary text-[11px] font-semibold text-accent-primary-fg">
          {number}
        </span>
        <p className="text-xs leading-relaxed text-fg-secondary">{caption}</p>
      </div>
    </div>
  );
}
