import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, createBufferedChannel } from "./store";

const DEFAULT_RUNTIME = "claude";

/**
 * Project name for the pinned Master group (never a real workspace project).
 * Standalone terminals spawned via the master group's "＋ 新建终端" belong here.
 */
export const MASTER_PROJECT = "master";

/**
 * Fallback project for un-scoped spawns (no project passed) — they land in the
 * Master group, so a stray empty "default" project never appears.
 */
const DEFAULT_PROJECT = MASTER_PROJECT;

/** Spawn a brand-new agent session and register it in the store. */
export async function spawnAgent(
  role: "master" | "worker" | "standalone",
  project?: string,
  runtime: string = DEFAULT_RUNTIME
): Promise<string> {
  const s = useStore.getState();
  const { channel, flush } = createBufferedChannel();
  const proj = project ?? DEFAULT_PROJECT;
  // A git-cloned / local-folder project carries its own on-disk root (the
  // store's `projectRoots` map). Pass it through so the agent's cwd lives under
  // that root instead of ~/CaPilot/workspaces/<name>. The pinned Master group
  // never gets a custom root (its spawns use the workspace layout).
  const projectRoot =
    proj === MASTER_PROJECT ? undefined : s.projectRoots[proj];
  const info = (await invoke("agent_spawn", {
    runtime,
    role,
    project: proj,
    projectRoot: projectRoot ?? null,
    resumeKey: null,
    model: s.selectedModel,
    speed: s.speed,
    mode: s.permissionMode,
    onData: channel,
  })) as AgentInfo;
  flush(info.id);
  s.addAgent(info, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || `${runtime}@${role}`,
  });
  if (role === "master") s.setMasterAgentId(info.id);
  return info.id;
}

/** Spawn a terminal from a new-terminal template (project "+" / tab-bar "+"
 *  picker): bash → plain shell, claude → claude code. Custom quick-start
 *  commands run in a bash terminal after the shell reaches its prompt. */
export async function spawnTerminal(
  project: string,
  template: { runtime: string; command: string },
  role: "master" | "worker" | "standalone" = "standalone"
): Promise<string> {
  const id = await spawnAgent(role, project, template.runtime);
  if (template.command && template.runtime.startsWith("bash")) {
    // Wait for the shell prompt, then send the command (raw:false appends \r).
    await new Promise((r) => setTimeout(r, 400));
    await invoke("agent_write", { id, data: template.command, raw: false }).catch(
      () => {}
    );
  }
  return id;
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
  const wasMaster = s.masterAgentId === agentId;
  s.closeTab(agentId);
  s.removeAgent(agentId);
  // Closing the master leaves its pinned "master" placeholder tab behind, which
  // would otherwise linger in the tab bar as a dead entry.
  if (wasMaster && s.tabs.some((t) => t.id === "master")) {
    s.closeTab("master");
  }
  try {
    // sessions_delete kills the PTY, removes the agent dir + DB session row,
    // and unregisters the worker (Bug 7).
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
