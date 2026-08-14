/** Runtimes whose alternate-screen UI consumes positioned mouse reports. */
const MOUSE_TUI_RUNTIMES = new Set(["claude", "opencode"]);

export function isMouseTuiRuntime(runtime: string | undefined): boolean {
  return runtime !== undefined && MOUSE_TUI_RUNTIMES.has(runtime);
}

/**
 * Whether CaPilot may synthesize an SGR 1006 mouse report for this runtime.
 *
 * OpenCode is a special case: its PTY and alternate-screen TUI stay resident
 * while the frontend xterm can be recreated. A recreated xterm may attach after
 * the bounded PTY replay has already dropped OpenCode's initial `CSI ? 1006 h`,
 * even though the live TUI still expects SGR reports. Treat SGR mouse support as
 * part of the OpenCode adapter contract instead of relying on that startup frame.
 */
export function canForwardSgrMouse(
  runtime: string | undefined,
  observedSgr: boolean
): boolean {
  if (!isMouseTuiRuntime(runtime)) return false;
  return observedSgr || runtime === "opencode";
}

export function sgrWheelReport(
  deltaY: number,
  col: number,
  row: number
): string {
  const button = deltaY < 0 ? 64 : 65;
  return `\x1b[<${button};${col};${row}M`;
}
