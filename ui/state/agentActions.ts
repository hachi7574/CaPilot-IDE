import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, RestoredSession, createBufferedChannel } from "./store";
import { notify } from "./notify";
import {
  detectShellFlavor,
  isShellRuntime,
  shellCd,
  shellCdAndRun,
} from "./shellPath";
import { t } from "../i18n";

const DEFAULT_RUNTIME = "claude";

const DEFAULT_PROJECT = "default";

/** Spawn a brand-new agent session and register it in the store. */
export async function spawnAgent(
  project?: string,
  runtime: string = DEFAULT_RUNTIME,
  opts?: { addTab?: boolean }
): Promise<string> {
  const s = useStore.getState();
  const { channel, flush } = createBufferedChannel();
  const proj = project ?? DEFAULT_PROJECT;
  // A git-cloned / local-folder project carries its own on-disk root (the
  // store's `projectRoots` map). Pass it through so the agent's cwd lives under
  // that root instead of ~/CaPilot/workspaces/<name>.
  const projectRoot = s.projectRoots[proj];
  // Model pinning: only pass a model id that actually exists in the target
  // runtime's catalog. claude spawns pin the chosen Claude model; dsh spawns
  // pin the chosen DeepSeek model via the per-session `--patch` overlay (its
  // model cannot be changed live — `/model` would fork the session); pi spawns
  // pin the chosen provider-qualified model id so the session boots on the
  // user's selection (live Ctrl+L switching still works after boot). A stale
  // selection from another runtime falls back to the target's own default
  // rather than leaking a provider-specific id across runtimes.
  const targetRuntime = s.runtimes.find((r) => r.id === runtime);
  const pinnedModel =
    runtime === DEFAULT_RUNTIME || runtime === "dsh" || runtime === "pi"
      ? targetRuntime?.models?.some((m) => m.id === s.selectedModel)
        ? s.selectedModel
        : null
      : null;
  let info: AgentInfo;
  try {
    info = (await invoke("agent_spawn", {
      runtime,
      project: proj,
      projectRoot: projectRoot ?? null,
      resumeKey: null,
      model: pinnedModel,
      speed: s.speed,
      mode: s.permissionMode,
      onData: channel,
    })) as AgentInfo;
  } catch (e) {
    // Spawn failures used to be swallowed by caller `.catch(console.error)` —
    // surface the reason (e.g. a dsh pre-flight diagnostic) instead of a
    // silently dead terminal. Re-throw so callers keep their own handling.
    notify(t("agentActions.spawnFailed"), typeof e === "string" ? e : String(e));
    throw e;
  }
  flush(info.id);
  s.addAgent({ ...info, project: proj }, channel);
  const tab = {
    id: info.id,
    type: "agent" as const,
    agentId: info.id,
    title: info.title || runtime,
  };
  // Canvas spawn passes `addTab: false` so CanvasPanel stays mounted. Still
  // register a silent tab — otherwise the tab strip has nothing to show when
  // switching back to terminal view, and the view slider is stuck.
  if (opts?.addTab === false) s.addTabSilent(tab);
  else s.addTab(tab);
  return info.id;
}

function shellRuntimeOrder(): string[] {
  return typeof navigator !== "undefined" &&
    ((navigator.platform || "").toLowerCase().includes("win") ||
      (navigator.userAgent || "").toLowerCase().includes("windows"))
    ? ["powershell", "cmd", "shell", "bash-rc"]
    : ["shell", "bash-rc"];
}

/** Available shells in preference order (always at least the first preference). */
function preferredShellRuntimes(): string[] {
  const available = new Set(
    useStore
      .getState()
      .runtimes.filter((r) => r.available)
      .map((r) => r.id)
  );
  const order = shellRuntimeOrder();
  const hits = order.filter((id) => available.has(id));
  return hits.length ? hits : [order[0]];
}

function spawnErrorRaw(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error && e.message) return e.message;
  if (e && typeof e === "object" && "message" in e) {
    const m = (e as { message: unknown }).message;
    if (typeof m === "string" && m) return m;
  }
  return String(e);
}

function spawnErrorText(e: unknown): string {
  const raw = spawnErrorRaw(e);
  if (/pty error/i.test(raw) || /daemon error/i.test(raw)) {
    return t("agentActions.ptyError");
  }
  return raw;
}

function isCapacityOrNameError(e: unknown): boolean {
  const raw = spawnErrorRaw(e);
  return /会话数已达上限|CapacityReached|Invalid project name|Project name cannot be empty/i.test(
    raw
  );
}

function sameDir(a: string | null, b: string): boolean {
  if (!a) return false;
  const n = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  return n(a) === n(b);
}

/** Spawn a terminal from a new-terminal template (project "+" / tab-bar "+"
 *  picker): shell → OS default terminal, claude → claude code. Custom
 *  quick-start commands run in the shell after it reaches its prompt. */
export async function spawnTerminal(
  project: string,
  template: { runtime: string; command: string },
  opts?: { addTab?: boolean }
): Promise<string> {
  const id = await spawnAgent(project, template.runtime, opts);
  if (template.command && isShellRuntime(template.runtime)) {
    // Wait for the shell prompt, then send the command (raw:false appends \r).
    await new Promise((r) => setTimeout(r, 400));
    // The template command is user-intended work, not boot output — end the
    // spawn wake so its output reads as 运行中 in the tab bar.
    useStore.getState().markAgentActive(id);
    await invoke("agent_write", { id, data: template.command, raw: false }).catch(
      () => {}
    );
  }
  return id;
}

function titleForShell(runtime: string): string {
  if (runtime === "powershell") return "PowerShell";
  if (runtime === "cmd") return "CMD";
  if (runtime === "bash-rc" || runtime.startsWith("bash")) return "bash";
  return t("agentActions.terminal");
}

function flavorForRuntime(runtime: string) {
  const info = useStore.getState().runtimes.find((r) => r.id === runtime);
  return detectShellFlavor(runtime, info?.name);
}

async function spawnShellOnce(
  project: string,
  runtime: string,
  projectRoot: string | null
): Promise<{
  info: AgentInfo;
  channel: ReturnType<typeof createBufferedChannel>["channel"];
  flush: (id: string) => void;
}> {
  const { channel, flush } = createBufferedChannel();
  const info = (await invoke("agent_spawn", {
    runtime,
    project,
    projectRoot,
    resumeKey: null,
    model: null,
    speed: "auto",
    mode: "ask",
    onData: channel,
  })) as AgentInfo;
  return { info, channel, flush };
}

/** Spawn a plain shell whose cwd is an arbitrary directory (e.g. a folder
 *  picked in the file tree). `command` (optional) runs after the shell reaches
 *  its prompt. Grouped under `project` in the sidebar.
 *
 *  File-tree folders sometimes fail as a PTY cwd (permissions, daemon wrap,
 *  ConPTY). Retry other shells, then spawn at the project root and `cd`. */
export async function spawnBashAt(
  project: string,
  dir: string,
  command?: string
): Promise<string> {
  const runtimes = preferredShellRuntimes();
  let lastError: unknown;
  let spawned: {
    info: AgentInfo;
    channel: ReturnType<typeof createBufferedChannel>["channel"];
    flush: (id: string) => void;
    runtime: string;
    needCd: boolean;
  } | null = null;

  for (const runtime of runtimes) {
    try {
      const r = await spawnShellOnce(project, runtime, dir);
      spawned = { ...r, runtime, needCd: false };
      break;
    } catch (e) {
      lastError = e;
      if (isCapacityOrNameError(e)) break;
    }
  }

  // Folder cwd can fail (PTY/permissions/canonicalize). Spawn at the project
  // root — or the per-agent workspace if the folder *is* the project root —
  // then cd into the requested directory.
  if (!spawned && lastError && !isCapacityOrNameError(lastError)) {
    const stored = useStore.getState().projectRoots[project] ?? null;
    const fallbackRoot = sameDir(stored, dir) ? null : stored;
    for (const runtime of runtimes) {
      try {
        const r = await spawnShellOnce(project, runtime, fallbackRoot);
        spawned = { ...r, runtime, needCd: true };
        break;
      } catch (e) {
        lastError = e;
        if (isCapacityOrNameError(e)) break;
      }
    }
  }

  if (!spawned) {
    const msg = spawnErrorText(lastError);
    notify(t("agentActions.spawnFailed"), msg);
    throw new Error(msg);
  }

  const { info, channel, flush, runtime, needCd } = spawned;
  const s = useStore.getState();
  flush(info.id);
  s.addAgent({ ...info, project }, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || titleForShell(runtime),
  });

  const flavor = flavorForRuntime(runtime);
  const followup = needCd
    ? command
      ? shellCdAndRun(dir, command, flavor)
      : shellCd(dir, flavor)
    : command ?? null;
  if (followup) {
    await new Promise((r) => setTimeout(r, 400));
    useStore.getState().markAgentActive(info.id);
    await invoke("agent_write", { id: info.id, data: followup, raw: false }).catch(
      () => {}
    );
  }
  return info.id;
}

/** Ensure the target agent has a live PTY channel (resume restored sessions).
 *  Returns true if a resume was required (caller may want to delay input). */
export async function ensureAgentChannel(agentId: string): Promise<boolean> {
  const s = useStore.getState();
  if (s.agentChannels.has(agentId)) return false;
  const { channel, flush } = createBufferedChannel();
  const info = (await invoke("agent_resume", {
    id: agentId,
    onData: channel,
  })) as AgentInfo;
  flush(info.id);
  s.addAgent(info, channel);
  return true;
}

/**
 * TUIs that treat a text+Enter burst as pasted input. When both land in one
 * PTY write, the trailing CR stays in the editor as a literal newline instead
 * of submitting. Send the prompt and Enter as two writes, matching a user
 * typing in the terminal. Codex was the original case; CodeBuddy Code (v1
 * generic CLI) does the same.
 */
const SPLIT_ENTER_RUNTIMES = new Set(["codex", "codebuddy"]);

/** CodeBuddy's editor treats a short text→CR burst as a paste, so the CR lands
 *  as a literal newline. Wait out paste-mode; 150ms was still too short. */
const SPLIT_ENTER_GAP_MS: Record<string, number> = {
  codex: 30,
  codebuddy: 400,
};

async function writePromptToPty(
  agentId: string,
  text: string,
  runtime?: string
): Promise<void> {
  // Trailing CRs in the payload become extra editor newlines before submit.
  const payload = text.replace(/[\r\n]+$/, "");
  if (runtime && SPLIT_ENTER_RUNTIMES.has(runtime)) {
    await invoke("agent_write", { id: agentId, data: payload, raw: true });
    await new Promise((r) => setTimeout(r, SPLIT_ENTER_GAP_MS[runtime] ?? 30));
    await invoke("agent_write", { id: agentId, data: "\r", raw: true });
    return;
  }
  await invoke("agent_write", { id: agentId, data: payload });
}

/**
 * Send a prompt to an agent with the Composer's exact send semantics, shared by
 * the Composer and the todo-tag drop targets:
 *   1. ensure a live PTY channel (resume restored/dormant sessions);
 *   2. give a freshly-booted TUI time to attach its input loop before injecting
 *      the message (see `waitForTui` / the resumed-detection comment in the
 *      Composer);
 *   3. stamp the submission (tab bar reads 运行中) and clear the
 *      unviewed-completion flag;
 *   4. write the message — split-enter runtimes (codex, codebuddy) get text
 *      then Enter as two PTY writes; other runtimes a plain write (raw:false
 *      appends the Enter).
 */
export async function sendPromptToAgent(
  agentId: string,
  text: string,
  opts?: { waitForTui?: boolean }
): Promise<void> {
  const s = useStore.getState();
  const resumed = await ensureAgentChannel(agentId);
  if ((opts?.waitForTui) || resumed) {
    await new Promise((r) => setTimeout(r, 800));
  }
  s.markAgentSubmitted(agentId);
  s.setAgentUnread(agentId, false);
  s.trackPromptAsTodo(agentId, text);
  await writePromptToPty(agentId, text, s.agents.get(agentId)?.runtime);
}

/**
 * Assign a todo tag to an agent and send its text as a prompt. The tag leaves
 * 待分配 (becomes invisible `assigned`), linked to the session so it auto-lands
 * in 待处理 when the session's turn ends. Does not switch the active tab —
 * dropping onto a sidebar session from the canvas (or any other view) must
 * keep that view. A missing/ended channel is resumed in-place by
 * `sendPromptToAgent`.
 */
export async function assignTodoAndSend(tagId: string, agentId: string): Promise<void> {
  const st = useStore.getState();
  const tag = st.todos.find((t) => t.id === tagId);
  if (!tag) return;
  const agent = st.agents.get(agentId);
  const sessionName = agent?.title ?? null;
  // The tag keeps its creation-time scope (project) — see `assignTodoToAgent`.
  st.assignTodoToAgent(tagId, agentId, sessionName);

  // Inject the prompt without stealing the current view (canvas / editor /
  // another terminal stay put). `sendPromptToAgent` resumes a missing channel
  // in-place via `agent_resume` — no tab switch required.
  await sendPromptToAgent(agentId, tag.text).catch(() => {});
}


/** Rename a terminal: the backend (`agent_rename`) persists the new title to the
 *  DB row + `.agent-meta.json`; then we update the live store (agent record +
 *  tab title) so the tab bar and sidebar labels move together. Throws on
 *  validation/backend errors — callers surface the message. */
export async function renameAgent(agentId: string, title: string): Promise<void> {
  const updated = await invoke<RestoredSession>("agent_rename", {
    id: agentId,
    title,
  });
  useStore.getState().updateAgentTitle(agentId, updated.title);
}

/** Close an agent: kill PTY, remove session row (so it won't resurrect), close tabs. */
export async function closeAgent(agentId: string): Promise<void> {
  const s = useStore.getState();
  // Close the UI first (tab + sidebar row) so the terminal disappears from the
  // main content right away even if the backend kill is slow. The kill then
  // runs after; sessions_delete also drops the DB row so nothing resurrects.
  s.closeTab(agentId);
  s.removeAgent(agentId);
  try {
    // sessions_delete kills the PTY and removes the agent dir + DB session row.
    await invoke("sessions_delete", { id: agentId });
  } catch {
    // Fall back to a plain kill so the terminal still closes even if session
    // cleanup failed.
    try {
      await invoke("agent_kill", { id: agentId });
    } catch {
      // ignore
    }
  }
}
