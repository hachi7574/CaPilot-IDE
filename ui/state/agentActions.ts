import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, createBufferedChannel } from "./store";

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
  const info = (await invoke("agent_spawn", {
    runtime,
    project: proj,
    projectRoot: projectRoot ?? null,
    resumeKey: null,
    // The composer currently exposes Claude's model list. Do not leak that
    // provider-specific selection into another runtime (for example Codex),
    // which should use its own configured default model.
    model: runtime === DEFAULT_RUNTIME ? s.selectedModel : null,
    speed: s.speed,
    mode: s.permissionMode,
    onData: channel,
  })) as AgentInfo;
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

/** Spawn a terminal from a new-terminal template (project "+" / tab-bar "+"
 *  picker): bash → plain shell, claude → claude code. Custom quick-start
 *  commands run in a bash terminal after the shell reaches its prompt. */
export async function spawnTerminal(
  project: string,
  template: { runtime: string; command: string }
): Promise<string> {
  const id = await spawnAgent(project, template.runtime);
  if (template.command && template.runtime.startsWith("bash")) {
    // Wait for the shell prompt, then send the command (raw:false appends \r).
    await new Promise((r) => setTimeout(r, 400));
    await invoke("agent_write", { id, data: template.command, raw: false }).catch(
      () => {}
    );
  }
  return id;
}

/** Spawn a bash terminal whose cwd is an arbitrary directory (e.g. a folder
 *  picked in the file tree). `command` (optional) runs after the shell reaches
 *  its prompt. The session is grouped under `project` for the sidebar. */
export async function spawnBashAt(
  project: string,
  dir: string,
  command?: string
): Promise<string> {
  const s = useStore.getState();
  const { channel, flush } = createBufferedChannel();
  const info = (await invoke("agent_spawn", {
    runtime: "bash-rc",
    project,
    projectRoot: dir,
    resumeKey: null,
    model: null,
    speed: "auto",
    mode: "ask",
    onData: channel,
  })) as AgentInfo;
  flush(info.id);
  s.addAgent({ ...info, project }, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || "bash",
  });
  if (command) {
    // Wait for the shell prompt, then send the command (raw:false appends \r).
    await new Promise((r) => setTimeout(r, 400));
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
