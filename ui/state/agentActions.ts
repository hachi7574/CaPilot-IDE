import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, RestoredSession, createBufferedChannel } from "./store";
import { notify } from "./notify";

const DEFAULT_RUNTIME = "claude";

const DEFAULT_PROJECT = "default";

/** Spawn a brand-new agent session and register it in the store. */
export async function spawnAgent(
  project?: string,
  runtime: string = DEFAULT_RUNTIME
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
    notify("终端启动失败", typeof e === "string" ? e : String(e));
    throw e;
  }
  flush(info.id);
  s.addAgent({ ...info, project: proj }, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || runtime,
  });
  return info.id;
}

/** True for plain interactive shells (OS shell / bash) — not agent CLIs. */
function isShellRuntime(runtime: string): boolean {
  return (
    runtime === "shell" ||
    runtime === "bash" ||
    runtime === "bash-rc" ||
    runtime.startsWith("bash")
  );
}

/** Spawn a terminal from a new-terminal template (project "+" / tab-bar "+"
 *  picker): shell → OS default terminal, claude → claude code. Custom
 *  quick-start commands run in the shell after it reaches its prompt. */
export async function spawnTerminal(
  project: string,
  template: { runtime: string; command: string }
): Promise<string> {
  const id = await spawnAgent(project, template.runtime);
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

/** Spawn the OS default shell (or bash fallback) whose cwd is an arbitrary
 *  directory (e.g. a folder picked in the file tree). `command` (optional) runs
 *  after the shell reaches its prompt. Grouped under `project` in the sidebar. */
export async function spawnBashAt(
  project: string,
  dir: string,
  command?: string
): Promise<string> {
  const s = useStore.getState();
  const { channel, flush } = createBufferedChannel();
  // Prefer the OS shell; fall back to bash-rc when shell isn't registered yet
  // (very old daemon) so file-tree "在此打开终端" still works.
  const runtime =
    s.runtimes.find((r) => r.id === "shell")?.available === false
      ? "bash-rc"
      : "shell";
  let info: AgentInfo;
  try {
    info = (await invoke("agent_spawn", {
      runtime,
      project,
      projectRoot: dir,
      resumeKey: null,
      model: null,
      speed: "auto",
      mode: "ask",
      onData: channel,
    })) as AgentInfo;
  } catch (e) {
    notify("终端启动失败", typeof e === "string" ? e : String(e));
    throw e;
  }
  flush(info.id);
  s.addAgent({ ...info, project }, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || (runtime === "shell" ? "终端" : "bash"),
  });
  if (command) {
    // Wait for the shell prompt, then send the command (raw:false appends \r).
    await new Promise((r) => setTimeout(r, 400));
    // The launch command is user-intended work — end the spawn wake so its
    // output reads as 运行中 rather than being dismissed as boot noise.
    useStore.getState().markAgentActive(info.id);
    await invoke("agent_write", { id: info.id, data: command, raw: false }).catch(
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
 * Send a prompt to an agent with the Composer's exact send semantics, shared by
 * the Composer and the todo-tag drop targets:
 *   1. ensure a live PTY channel (resume restored/dormant sessions);
 *   2. give a freshly-booted TUI time to attach its input loop before injecting
 *      the message (see `waitForTui` / the resumed-detection comment in the
 *      Composer);
 *   3. stamp the submission (tab bar reads 运行中) and clear the
 *      unviewed-completion flag;
 *   4. write the message — codex gets the text+Enter keystroke burst its TUI
 *      needs to treat the input as a submitted prompt, other runtimes a plain
 *      write (raw:false appends the Enter).
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
  const runtime = s.agents.get(agentId)?.runtime;
  if (runtime === "codex") {
    // Codex's TUI detects a text+Enter burst as pasted input. When both are
    // delivered in one PTY write, the trailing CR may remain in the editor
    // instead of submitting the prompt. Send the keystrokes separately,
    // matching what happens when a user types in the terminal directly.
    await invoke("agent_write", { id: agentId, data: text, raw: true });
    await new Promise((r) => setTimeout(r, 30));
    await invoke("agent_write", { id: agentId, data: "\r", raw: true });
  } else {
    await invoke("agent_write", { id: agentId, data: text });
  }
}

/**
 * Assign a todo tag to an agent and send its text as a prompt. The tag leaves
 * 待分配 (becomes invisible `assigned`), linked to the session so it auto-lands
 * in 待处理 when the session's turn ends. For an ended/dormant session the
 * standard reopen flow runs first (drop dead channel + flag resume + open the
 * terminal), then the prompt is injected once the resumed channel is live.
 */
export async function assignTodoAndSend(tagId: string, agentId: string): Promise<void> {
  const st = useStore.getState();
  const tag = st.todos.find((t) => t.id === tagId);
  if (!tag) return;
  const agent = st.agents.get(agentId);
  const sessionName = agent?.title ?? null;
  // The tag keeps its creation-time scope (project) — see `assignTodoToAgent`.
  st.assignTodoToAgent(tagId, agentId, sessionName);

  // Live session (channel attached, not ended) → straight in.
  if (!st.agentChannels.has(agentId) || agent?.status === "done") {
    // Ended/dormant: reopen like the sidebar "已结束" click — force a fresh
    // terminal mount that resumes — then wait for the resumed channel before
    // sending, so `sendPromptToAgent`'s ensureAgentChannel can't double-resume.
    st.dropAgentChannel(agentId);
    if (st.tabs.some((t) => t.id === agentId)) st.closeTab(agentId);
    st.requestResume(agentId);
    if (!st.tabs.find((t) => t.id === agentId)) {
      st.addTab({
        id: agentId,
        type: "agent",
        agentId,
        title: sessionName ?? `agent-${agentId.slice(0, 6)}`,
      });
    }
    st.setActiveTab(agentId);
    for (let i = 0; i < 30; i++) {
      if (useStore.getState().agentChannels.has(agentId)) break;
      await new Promise((r) => setTimeout(r, 100));
    }
  }
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
