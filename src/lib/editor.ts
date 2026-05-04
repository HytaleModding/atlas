import type { EditorProtocol } from "@/state/uiPrefsStore";

/** Build an external-editor URL for the configured protocol.
 *  vscode:  vscode://file/<path>:<line>
 *  idea:    idea://open?file=<path>&line=<line>
 *  none:    null (caller should hide the trigger entirely)
 *
 *  Returns null when the protocol is "none" or when `path` is empty.
 * .
 */
export function editorUrl(
  protocol: EditorProtocol,
  path: string,
  line: number | null,
): string | null {
  if (protocol === "none" || path.length === 0) return null;
  if (protocol === "vscode") {
    return `vscode://file/${path}${line ? `:${line}` : ""}`;
  }
  // JetBrains IDEs accept `idea://open?file=<path>&line=<line>`.
  const params = new URLSearchParams({ file: path });
  if (line !== null) params.set("line", String(line));
  return `idea://open?${params.toString()}`;
}

export function editorLabel(protocol: EditorProtocol): string {
  switch (protocol) {
    case "vscode":
      return "Open in VS Code";
    case "idea":
      return "Open in IntelliJ";
    case "none":
      return "External editor disabled";
  }
}
