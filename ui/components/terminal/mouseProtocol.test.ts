import assert from "node:assert/strict";
import test from "node:test";
import {
  canForwardSgrMouse,
  isMouseTuiRuntime,
  sgrWheelReport,
} from "./mouseProtocol.ts";

test("OpenCode keeps SGR mouse forwarding after a resident PTY is reattached", () => {
  assert.equal(canForwardSgrMouse("opencode", false), true);
});

test("other runtimes retain their existing mouse behavior", () => {
  assert.equal(canForwardSgrMouse("claude", false), false);
  assert.equal(canForwardSgrMouse("claude", true), true);
  assert.equal(canForwardSgrMouse("codex", true), false);
  assert.equal(canForwardSgrMouse("bash", true), false);
  assert.equal(isMouseTuiRuntime(undefined), false);
});

test("wheel reports preserve direction and terminal coordinates", () => {
  assert.equal(sgrWheelReport(-1, 40, 5), "\x1b[<64;40;5M");
  assert.equal(sgrWheelReport(1, 12, 22), "\x1b[<65;12;22M");
});
