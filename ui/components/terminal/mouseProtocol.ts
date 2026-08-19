/** Runtimes whose alternate-screen UI consumes positioned mouse reports. */
const MOUSE_TUI_RUNTIMES = new Set(["claude", "opencode"]);

export function isMouseTuiRuntime(runtime: string | undefined): boolean {
  return runtime !== undefined && MOUSE_TUI_RUNTIMES.has(runtime);
}

/**
 * Whether CaPilot may synthesize an SGR 1006 mouse report for this runtime.
 *
 * Claude / OpenCode keep a live alternate-screen TUI. Their xterm can still be
 * recreated (split-view remount, first restore). A recreated xterm may attach
 * after the bounded PTY replay has already dropped the initial `CSI ? 1006 h`,
 * even though the live TUI still expects SGR reports. Treat SGR mouse support
 * as part of the adapter contract instead of relying on that startup frame.
 */
export function canForwardSgrMouse(
  runtime: string | undefined,
  _observedSgr: boolean
): boolean {
  return isMouseTuiRuntime(runtime);
}

export function sgrWheelReport(
  deltaY: number,
  col: number,
  row: number
): string {
  const button = deltaY < 0 ? 64 : 65;
  return `\x1b[<${button};${col};${row}M`;
}
