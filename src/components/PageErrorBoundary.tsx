import { Component, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null };

/** Surfaces React render errors from a page so the user (and any
 *  attached devtools) sees them instead of a blank white screen. The
 *  message and stack are rendered verbatim - this is a development /
 *  diagnostic affordance, not a final UX. */
export class PageErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    // eslint-disable-next-line no-console
    console.error("Page render error:", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex flex-1 flex-col gap-3 overflow-auto p-6 font-mono text-xs">
          <p className="text-sm font-semibold text-destructive">
            This page hit a render error.
          </p>
          <pre className="whitespace-pre-wrap rounded border border-destructive/40 bg-destructive/10 p-3 text-destructive">
            {this.state.error.message}
          </pre>
          {this.state.error.stack && (
            <pre className="whitespace-pre-wrap rounded border border-border-subtle bg-bg-surface p-3 text-fg-muted">
              {this.state.error.stack}
            </pre>
          )}
          <button
            type="button"
            onClick={() => this.setState({ error: null })}
            className="self-start rounded border border-border-subtle px-3 py-1.5 text-fg-secondary hover:bg-bg-elevated"
          >
            Dismiss
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
