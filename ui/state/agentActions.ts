import { invoke } from "@tauri-apps/api/core";
import { useStore, AgentInfo, createBufferedChannel } from "./store";

const DEFAULT_RUNTIME = "claude";
const DEFAULT_PROJECT = "default";

/** Spawn a brand-new agent session and register it in the store. */
export async function spawnAgent(
  role: "master" | "worker" | "standalone"
): Promise<string> {
  const s = useStore.getState();
  const { channel, flush } = createBufferedChannel();
  const info = (await invoke("agent_spawn", {
    runtime: DEFAULT_RUNTIME,
    role,
    project: DEFAULT_PROJECT,
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

/** Close an agent: kill PTY, remove session, close tabs. */
export async function closeAgent(agentId: string): Promise<void> {
  const s = useStore.getState();
  try {
    await invoke("agent_kill", { id: agentId });
  } catch {
    // ignore
  }
  s.closeTab(agentId);
  s.removeAgent(agentId);
}
