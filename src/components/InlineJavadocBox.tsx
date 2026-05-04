import { ExternalLink } from "lucide-react";

/** Light-tint card that drops above a class or method declaration inside
 *  the source viewer. fuses Javadoc + source into a single
 *  view rather than showing them as separate panes / separate hits.
 *
 *  When `methodName` is null this renders the class-level box (header
 *  shows the FQN). When set, it documents that specific method overload.
 */
export function InlineJavadocBox({
  classFqn,
  methodName,
  signature,
  body,
  deprecated,
  onOpenFull,
}: {
  classFqn: string;
  methodName?: string | null;
  signature?: string | null;
  body: string;
  deprecated?: boolean;
  onOpenFull?: () => void;
}) {
  const className = classFqn.split(".").pop() ?? classFqn;
  const heading = methodName
    ? `${className}.${methodName}${signature ?? ""}`
    : className;
  return (
    <div
      className="my-2 rounded-md border-l-2 px-3 py-2 text-[12px] leading-5"
      style={{
        borderLeftColor: "var(--section-javadocs)",
        background:
          "color-mix(in srgb, var(--section-javadocs) 8%, transparent)",
      }}
    >
      <div className="mb-1 flex items-baseline justify-between gap-2">
        <span
          className="truncate font-mono text-[11px] font-medium"
          style={{ color: "var(--section-javadocs)" }}
          title={methodName ? `${classFqn}#${methodName}` : classFqn}
        >
          Javadoc · {heading}
          {deprecated && " (deprecated)"}
        </span>
        {onOpenFull && (
          <button
            type="button"
            onClick={onOpenFull}
            className="inline-flex shrink-0 items-center gap-1 text-[11px] text-fg-muted hover:text-fg-secondary"
            title="Open full Javadoc page"
          >
            Open full
            <ExternalLink size={11} strokeWidth={1.75} />
          </button>
        )}
      </div>
      <div className="whitespace-pre-wrap break-words text-fg-secondary">
        {body}
      </div>
    </div>
  );
}
