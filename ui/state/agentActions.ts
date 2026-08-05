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
  project?: string
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
    runtime: DEFAULT_RUNTIME,
    role,
    project: proj,
    projectRoot: projectRoot ?? null,
    resumeKey: null,
    model: s.selectedModel,
    speed: s.speed,
    onData: channel,
  })) as AgentInfo;
  flush(info.id);
  s.addAgent(info, channel);
  s.addTab({
    id: info.id,
    type: "agent",
    agentId: info.id,
    title: info.title || `${DEFAULT_RUNTIME}@${role}`,
  });
  if (role === "master") s.setMasterAgentId(info.id);
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
  s.closeTab(agentId);
  s.removeAgent(agentId);
}
