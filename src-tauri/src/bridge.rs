//! GUI ↔ PTY bridge (§8 of docs/pty-daemon-brief.md).
//!
//! Every `agent_*` command talks to a single [`PtyBridge`] instead of the PTY
//! core directly. The bridge owns exactly one PTY owner:
//!
//! - **Daemon** — a [`DaemonClient`] to the user-level PTY daemon. The daemon
//!   owns the PTYs; the bridge registers each agent's frontend `Channel` and
//!   forwards the daemon's output events to it, and re-broadcasts the
//!   natural-exit/removal events the daemon already persisted.
//! - **InProcess** — the fallback [`PtyCore`], used only when no daemon can be
//!   started AND the instance lock proves no other owner exists (§8).
//! - **Unavailable** — a hard condition (§8): a live daemon we cannot talk to
//!   (stale socket, version mismatch, wedged). Falling back here would create a
//!   second PTY owner, so every command fails with a clear error instead.

use crate::agent_runtime::adapter::{AgentError, AgentInfo, AgentStatus};
use crate::agent_runtime::pty_core::{OnExit, OutputSink, PtyCore, SinkError, SinkResult};
use crate::daemon::bin::{daemon_base, APP_VERSION};
use crate::daemon::client::{ClientError, DaemonClient};
pub use crate::daemon::client::SyncEventsResult;
use crate::daemon::protocol::ClientEvent;
use crate::daemon::runtime::InstanceLock;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

/// How many times to probe for a freshly spawned (or just-wedged) daemon.
const DAEMON_CONNECT_RETRIES: u32 = 30;
/// Delay between daemon connect probes.
const DAEMON_CONNECT_DELAY: Duration = Duration::from_millis(200);
/// Poll interval of the daemon event-forwarding thread.
const EVENT_POLL: Duration = Duration::from_millis(500);

/// Payload re-emitted on `agent://exited` (§5.1). Mirrors the in-process
/// `build_on_exit` payload so the frontend sees the same event shape in both
/// modes. `event_seq` (journal sequence, Phase 4b) lets the frontend advance
/// its replay watermark so offline events are never double-applied.
#[derive(Clone, Serialize)]
struct AgentExited {
    id: String,
    exit_code: i32,
    event_seq: u64,
}

/// Payload re-emitted on `agent://removed`.
#[derive(Clone, Serialize)]
struct AgentRemoved {
    id: String,
    event_seq: u64,
}

/// Payload re-emitted on `agent://hook-status` (Phase 4). Mirrors the
/// daemon's `ClientEvent::HookStatus` so the frontend sees the same shape as
/// the in-process status-hook polling path. `event_seq` lets the frontend
/// dedupe replay vs live delivery of the same journaled transition.
#[derive(Clone, Serialize)]
struct AgentHookStatus {
    id: String,
    status: String,
    ts: i64,
    event_seq: u64,
}

/// The live PTY owner behind the bridge.
enum PtyOwner {
    Daemon(Arc<DaemonClient>),
    InProcess(
        Arc<PtyCore>,
        /// Held for the bridge's whole lifetime: while the fallback owns the
        /// PTYs, no daemon may start (§8). Never read — dropping releases it.
        #[allow(dead_code)]
        InstanceLock,
    ),
    /// Hard condition (§8): a live owner we cannot talk to. Never fall back.
    Unavailable(String),
}

/// Per-agent frontend state kept while the owner is a daemon.
struct AgentChannel {
    channel: Channel<Vec<u8>>,
    generation: u64,
}

/// The single PTY owner exposed to the GUI (see module docs).
pub struct PtyBridge {
    owner: PtyOwner,
    /// agent_id → frontend channel + generation (daemon mode only). In-process
    /// mode keeps no registry; the PTY core holds its own sink per agent.
    channels: Mutex<HashMap<String, AgentChannel>>,
    /// Serializes a daemon spawn/attach round-trip (request → channel
    /// registration) against the event thread's per-event handling, so output
    /// that arrives between the request and the registration can never be
    /// dropped for lack of a channel — it queues in the daemon's event channel
    /// and is processed only after the channel is live (§4.2 attach window).
    attach_lock: Mutex<()>,
    /// Per-agent highest output seq already forwarded to the frontend, keyed by
    /// `agent_id → (generation, seq)`. Guards against duplicate delivery when a
    /// WebView reload leaves the spawn-time daemon subscriber alive and a later
    /// attach adds a second subscriber on the same connection: events with
    /// `seq <= last` for the same generation are already delivered (via the
    /// checkpoint/replay or the earlier subscriber) and are skipped. A new
    /// generation (respawn) resets the sequence.
    last_seq: Mutex<HashMap<String, (u64, u64)>>,
    /// AppHandle used to re-emit daemon lifecycle events to the WebView.
    app: Mutex<Option<AppHandle>>,
    /// Highest lifecycle `event_seq` already forwarded to the frontend (Phase
    /// 4b). On startup the frontend calls `agent_sync_events(last_event_seq)` to
    /// pull everything that happened while it was offline; live events after
    /// that point update this watermark, so replay never double-delivers.
    last_event_seq: AtomicU64,
    /// Set on shutdown so the event-forwarding thread doesn't outlive the GUI.
    closed: AtomicBool,
}

impl PtyBridge {
    /// Start the bridge: prefer an already-running daemon, then spawn one,
    /// then fall back in-process — each step gated by the instance lock (§8).
    pub fn start() -> Arc<Self> {
        let base = daemon_base();

        // 1. Already-running daemon.
        if let Some(client) = Self::try_connect(&base, false) {
            return Arc::new(Self::daemon(client));
        }

        // 2. Is another daemon's instance lock live? Probe it so a wedged /
        //    incompatible daemon is never silently bypassed (§8).
        match InstanceLock::try_acquire(&base) {
            Ok(Some(lock)) => drop(lock), // lock free — the daemon/fallback re-takes it
            Ok(None) => {
                if let Some(client) = Self::try_connect(&base, true) {
                    return Arc::new(Self::daemon(client));
                }
                return Arc::new(Self::broken(
                    "daemon instance lock is held but the daemon is unreachable or incompatible",
                ));
            }
            Err(e) => return Arc::new(Self::broken(format!("instance lock unavailable: {e}"))),
        }

        // 3. Lock free: spawn a daemon and wait for it.
        if let Err(e) = Self::spawn_daemon_process() {
            log::warn!("daemon spawn failed: {e}");
        }
        if let Some(client) = Self::try_connect(&base, true) {
            return Arc::new(Self::daemon(client));
        }

        // 4. The daemon never came up. Re-take the lock and fall back
        //    in-process (Phase 2: the GUI owns the PTYs again).
        let lock = match InstanceLock::try_acquire(&base) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                return Arc::new(Self::broken(
                    "daemon failed to start and another owner took the instance lock",
                ));
            }
            Err(e) => return Arc::new(Self::broken(format!("fallback instance lock: {e}"))),
        };
        Arc::new(Self::in_process(lock))
    }

    /// Try to connect to a daemon. With `retry`, loops while the daemon is
    /// "not running" (socket/token not created yet — a freshly spawned daemon).
    /// A hard handshake failure (version mismatch, bad token, protocol error)
    /// aborts immediately: the daemon is alive but incompatible, and retrying
    /// will not change that (§8 — never fall back under a live owner).
    fn try_connect(base: &Path, retry: bool) -> Option<Arc<DaemonClient>> {
        let attempts = if retry { DAEMON_CONNECT_RETRIES } else { 1 };
        for _ in 0..attempts {
            match DaemonClient::connect(base, APP_VERSION) {
                Ok(client) => return Some(Arc::new(client)),
                Err(ClientError::NotRunning) => {}
                Err(e) => {
                    log::warn!("daemon handshake failed (not falling back): {e}");
                    return None;
                }
            }
            std::thread::sleep(DAEMON_CONNECT_DELAY);
        }
        None
    }

    /// Spawn the daemon as a detached child of this process (`current_exe()
    /// --daemon`). The child must outlive the GUI, so the handle is dropped
    /// without waiting or killing it. Phase 4 (§9.4): the daemon stays resident
    /// across GUI exits, so it is spawned in its own process group and its
    /// stderr is redirected to a log file — an orphaned daemon must not inherit
    /// a closed terminal (SIGTTOU/EPIPE on eprintln) or die with the GUI's
    /// session.
    fn spawn_daemon_process() -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        let log = std::env::temp_dir().join("capilot-daemon.log");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log_file);
        // New process group: the daemon survives Ctrl-C / SIGHUP to the GUI
        // process group, and the GUI's exit never waits on it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS: the daemon gets its
            // own process group and no console, so it survives the GUI's exit
            // and Ctrl-C and never inherits the GUI's console.
            cmd.creation_flags(0x0000_0200 | 0x0000_0008);
        }
        let child = cmd.spawn()?;
        // Detached child — dropping Child doesn't wait or kill.
        drop(child);
        Ok(())
    }

    fn daemon(client: Arc<DaemonClient>) -> Self {
        Self {
            owner: PtyOwner::Daemon(client),
            channels: Mutex::new(HashMap::new()),
            attach_lock: Mutex::new(()),
            last_seq: Mutex::new(HashMap::new()),
            last_event_seq: AtomicU64::new(0),
            app: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    fn in_process(lock: InstanceLock) -> Self {
        Self {
            owner: PtyOwner::InProcess(Arc::new(PtyCore::new()), lock),
            channels: Mutex::new(HashMap::new()),
            attach_lock: Mutex::new(()),
            last_seq: Mutex::new(HashMap::new()),
            last_event_seq: AtomicU64::new(0),
            app: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    fn broken(reason: impl Into<String>) -> Self {
        Self {
            owner: PtyOwner::Unavailable(reason.into()),
            channels: Mutex::new(HashMap::new()),
            attach_lock: Mutex::new(()),
            last_seq: Mutex::new(HashMap::new()),
            last_event_seq: AtomicU64::new(0),
            app: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    /// Give the bridge the AppHandle used to re-emit daemon lifecycle events
    /// to the WebView. Called from `.setup()` before `start_event_loop`.
    pub fn attach_app(&self, app: AppHandle) {
        *self.app.lock().unwrap_or_else(|p| p.into_inner()) = Some(app);
    }

    /// Human-readable ownership mode for diagnostics.
    pub fn mode(&self) -> &'static str {
        match &self.owner {
            PtyOwner::Daemon(_) => "daemon",
            PtyOwner::InProcess(..) => "in_process",
            PtyOwner::Unavailable(_) => "unavailable",
        }
    }

    /// Start the thread that drains daemon events and forwards them to the
    /// frontend. No-op in non-daemon modes.
    pub fn start_event_loop(self: &Arc<Self>) {
        let PtyOwner::Daemon(client) = &self.owner else {
            return;
        };
        let client = client.clone();
        let bridge = self.clone();
        std::thread::Builder::new()
            .name("capilot-daemon-events".into())
            .spawn(move || bridge.drain_events(client))
            .expect("spawn daemon event thread");
    }

    fn drain_events(self: &Arc<Self>, client: Arc<DaemonClient>) {
        while !self.closed.load(Ordering::Acquire) {
            match client.recv_event_timeout(EVENT_POLL) {
                Ok(event) => self.handle_event(event),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    log::error!(
                        "daemon connection lost; live sessions continue in the daemon (bridge offline)"
                    );
                    break;
                }
            }
        }
    }

    /// Dispatch one daemon event to the frontend (split out for testing).
    ///
    /// Every event takes `attach_lock` so it can never interleave with a daemon
    /// spawn/attach round-trip: an event that arrives during that window queues
    /// and then flows through the freshly-registered channel (§4.2), instead of
    /// being dropped for lack of one.
    fn handle_event(&self, event: ClientEvent) {
        let _g = self.attach_lock.lock().unwrap_or_else(|p| p.into_inner());
        match event {
            ClientEvent::Output {
                agent_id,
                generation,
                seq,
                data,
            } => {
                // Skip already-delivered output. Two daemon subscribers on one
                // connection (a WebView reload leaves the spawn-time subscriber
                // alive, then an attach adds a second) would otherwise replay
                // the same bytes; a fresh terminal got them via the checkpoint/
                // replay. A new generation resets the sequence (§5).
                {
                    let mut last = self.last_seq.lock().unwrap_or_else(|p| p.into_inner());
                    let entry = last.entry(agent_id.clone()).or_insert((generation, 0));
                    if entry.0 == generation {
                        if seq <= entry.1 {
                            return;
                        }
                    }
                    entry.0 = generation;
                    entry.1 = seq;
                }
                let channel = self
                    .channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&agent_id)
                    .map(|c| c.channel.clone());
                if let Some(channel) = channel {
                    if channel.send(data).is_err() {
                        // Frontend channel gone — drop the subscription (§4.3).
                        // The PTY keeps running in the daemon.
                        self.channels
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .remove(&agent_id);
                    }
                }
            }
            ClientEvent::Exited {
                agent_id,
                exit_code,
                event_seq,
                ..
            } => {
                self.channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&agent_id);
                self.last_seq
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&agent_id);
                self.last_event_seq.fetch_max(event_seq, Ordering::AcqRel);
                self.emit(
                    "agent://exited",
                    AgentExited {
                        id: agent_id,
                        exit_code,
                        event_seq,
                    },
                );
            }
            ClientEvent::Removed {
                agent_id,
                event_seq,
                ..
            } => {
                self.channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&agent_id);
                self.last_seq
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&agent_id);
                self.last_event_seq.fetch_max(event_seq, Ordering::AcqRel);
                self.emit(
                    "agent://removed",
                    AgentRemoved {
                        id: agent_id,
                        event_seq,
                    },
                );
            }
            ClientEvent::HookStatus {
                agent_id,
                status,
                ts,
                event_seq,
            } => {
                self.last_event_seq
                    .fetch_max(event_seq, Ordering::AcqRel);
                self.emit(
                    "agent://hook-status",
                    AgentHookStatus {
                        id: agent_id,
                        status,
                        ts,
                        event_seq,
                    },
                );
            }
        }
    }

    fn emit<T: Serialize + Clone>(&self, event: &str, payload: T) {
        let app = self.app.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(app) = app {
            let _ = app.emit(event, payload);
        }
    }

    /// Spawn a PTY session. `on_data` receives the session's PTY bytes.
    /// `on_exit` is used only in in-process mode — in daemon mode the daemon
    /// persists the natural-exit outcome and pushes `ClientEvent::Exited`,
    /// which the event loop re-broadcasts as `agent://exited`.
    pub fn spawn(
        &self,
        agent_id: &str,
        program: &str,
        args: &[String],
        cwd: &PathBuf,
        rows: u16,
        cols: u16,
        env: &[(String, String)],
        on_data: Channel<Vec<u8>>,
        on_exit: Option<OnExit>,
    ) -> Result<AgentInfo, AgentError> {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => {
                let sink = Arc::new(ChannelSink {
                    agent_id: agent_id.to_string(),
                    channel: on_data,
                    pty: pty.clone(),
                });
                pty.spawn(
                    agent_id.to_string(),
                    program,
                    args,
                    cwd,
                    rows,
                    cols,
                    sink,
                    on_exit,
                    env,
                )
            }
            PtyOwner::Daemon(client) => {
                // Hold the attach lock across the request + channel registration
                // so the event thread cannot drop the first output events before
                // the channel exists (§4.2 attach window). A fresh spawn's
                // terminal wants everything.
                let _g = self.attach_lock.lock().unwrap_or_else(|p| p.into_inner());
                let (pid, generation) = match client.spawn(agent_id, program, args, cwd, env, rows, cols)
                {
                    Ok(v) => v,
                    Err(ClientError::Request { code, .. }) if code == "capacity" => {
                        // Preserve the structured cap so `build_and_spawn` shows
                        // the same friendly Chinese message as in-process mode.
                        return Err(AgentError::CapacityReached {
                            limit: crate::agent_runtime::pty_core::MAX_LIVE_SESSIONS,
                        });
                    }
                    Err(e) => return Err(AgentError::PtyError(e.to_string())),
                };
                self.channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        agent_id.to_string(),
                        AgentChannel {
                            channel: on_data,
                            generation,
                        },
                    );
                // A fresh spawn restarts the daemon hub's sequence at 1; record
                // the new generation so a stale entry from a previous incarnation
                // of this agent_id can't suppress the first bytes.
                self.last_seq
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(agent_id.to_string(), (generation, 0));
                Ok(AgentInfo {
                    id: agent_id.to_string(),
                    workspace_id: None,
                    project: None,
                    runtime: String::new(),
                    status: AgentStatus::Running,
                    title: String::new(),
                    cwd: cwd.to_path_buf(),
                    pid: Some(pid),
                    mode: String::new(),
                    speed: String::new(),
                    model: None,
                    last_usage: None,
                })
            }
            PtyOwner::Unavailable(reason) => Err(AgentError::PtyError(reason.clone())),
        }
    }

    /// Re-attach to a live daemon session (§4.2/§6.3). Uses the daemon's list as
    /// the liveness authority: a session the daemon has reaped is gone regardless
    /// of GUI state and yields `AgentNotFound` (the caller falls through to
    /// respawn). Sends the checkpoint + gap replay through `on_data`, registers
    /// the channel, and forwards only live `seq > snapshot_seq` output.
    /// In-process mode has nothing to attach to — always `AgentNotFound`, so a
    /// restored session respawns there.
    pub fn attach(
        &self,
        agent_id: &str,
        rows: u16,
        cols: u16,
        on_data: Channel<Vec<u8>>,
    ) -> Result<AgentInfo, AgentError> {
        match &self.owner {
            PtyOwner::InProcess(..) => Err(AgentError::AgentNotFound(agent_id.to_string())),
            PtyOwner::Daemon(client) => {
                // Hold the attach lock across the whole round-trip so the
                // checkpoint + replay are applied before any live event is
                // forwarded (§4.2 attach window).
                let _g = self.attach_lock.lock().unwrap_or_else(|p| p.into_inner());
                // The daemon's list is the authoritative liveness view (§6.3).
                let summary = client
                    .list()
                    .map_err(|e| AgentError::PtyError(e.to_string()))?
                    .into_iter()
                    .find(|s| s.agent_id == agent_id)
                    .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
                let result = client
                    .attach(agent_id, summary.generation, rows, cols, None)
                    .map_err(|e| match e {
                        ClientError::Request { code, .. }
                            if code == "not_found" || code == "stale_generation" =>
                        {
                            AgentError::AgentNotFound(agent_id.to_string())
                        }
                        other => AgentError::PtyError(other.to_string()),
                    })?;
                // A fresh terminal: render the checkpoint, then the gap.
                if let Some(ckpt) = &result.checkpoint {
                    if on_data.send(ckpt.clone()).is_err() {
                        return Err(AgentError::PtyError(format!(
                            "attach channel closed for {agent_id}"
                        )));
                    }
                }
                if !result.replay.is_empty() {
                    if on_data.send(result.replay.clone()).is_err() {
                        return Err(AgentError::PtyError(format!(
                            "attach channel closed for {agent_id}"
                        )));
                    }
                }
                self.channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        agent_id.to_string(),
                        AgentChannel {
                            channel: on_data,
                            generation: summary.generation,
                        },
                    );
                // Forward only live output past the snapshot; anything ≤
                // snapshot_seq was already delivered via checkpoint/replay and
                // must not be re-sent (§11 no-loss/no-dup).
                self.last_seq
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        agent_id.to_string(),
                        (summary.generation, result.snapshot_seq),
                    );
                Ok(AgentInfo {
                    id: agent_id.to_string(),
                    workspace_id: None,
                    project: None,
                    runtime: String::new(),
                    status: AgentStatus::Running,
                    title: String::new(),
                    cwd: PathBuf::new(),
                    pid: Some(summary.pid),
                    mode: String::new(),
                    speed: String::new(),
                    model: None,
                    last_usage: None,
                })
            }
            PtyOwner::Unavailable(reason) => Err(AgentError::PtyError(reason.clone())),
        }
    }

    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<(), AgentError> {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty.write(agent_id, data),
            PtyOwner::Daemon(client) => {
                let generation = self
                    .generation(agent_id)
                    .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
                let text = std::str::from_utf8(data)
                    .map_err(|e| AgentError::PtyError(format!("terminal input is not UTF-8: {e}")))?;
                client
                    .write(agent_id, generation, text)
                    .map_err(|e| AgentError::PtyError(e.to_string()))
            }
            PtyOwner::Unavailable(reason) => Err(AgentError::PtyError(reason.clone())),
        }
    }

    pub fn resize(&self, agent_id: &str, rows: u16, cols: u16) -> Result<(), AgentError> {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty.resize(agent_id, rows, cols),
            PtyOwner::Daemon(client) => {
                let generation = self
                    .generation(agent_id)
                    .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
                client
                    .resize(agent_id, generation, rows, cols)
                    .map_err(|e| AgentError::PtyError(e.to_string()))
            }
            PtyOwner::Unavailable(reason) => Err(AgentError::PtyError(reason.clone())),
        }
    }

    pub fn kill(&self, agent_id: &str) -> Result<(), AgentError> {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty.kill(agent_id),
            PtyOwner::Daemon(client) => {
                let generation = self.generation(agent_id);
                let result = client
                    .kill(agent_id, generation)
                    .map_err(|e| AgentError::PtyError(e.to_string()));
                // Drop the frontend channel + generation either way: a reaped
                // agent has no more output to forward, and a stale entry would
                // misroute a later respawn's events.
                self.channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(agent_id);
                self.last_seq
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(agent_id);
                result
            }
            PtyOwner::Unavailable(reason) => Err(AgentError::PtyError(reason.clone())),
        }
    }

    /// Snapshot of live agent PIDs with their incarnation generation —
    /// `(agent_id, generation, pid)`. Used by the resource monitor (§10) to
    /// sample whole process trees per agent; the generation lets it invalidate
    /// history when a respawned process takes over the same agent slot (old
    /// samples must not be mixed into the new incarnation's curve).
    pub fn pids(&self) -> Vec<(String, u64, u32)> {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty
                .pids()
                .into_iter()
                .map(|(id, pid)| (id.clone(), pty.generation(&id).unwrap_or(0), pid))
                .collect(),
            PtyOwner::Daemon(client) => client
                .list()
                .map(|sessions| {
                    sessions
                        .into_iter()
                        .map(|s| (s.agent_id, s.generation, s.pid))
                        .collect()
                })
                .unwrap_or_default(),
            PtyOwner::Unavailable(_) => Vec::new(),
        }
    }

    /// Kill every live PTY.
    pub fn kill_all(&self) {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty.kill_all(),
            // Explicit full shutdown (kept for callers that genuinely want the
            // daemon gone). The GUI's normal exit uses `detach` instead (§9.4).
            PtyOwner::Daemon(client) => {
                self.closed.store(true, Ordering::Release);
                let _ = client.shutdown();
            }
            PtyOwner::Unavailable(_) => {}
        }
    }

    /// GUI-exit semantics (Phase 4, §9.4): release the GUI's interest in every
    /// session so the daemon — and each agent's PTY — keeps running.
    ///
    /// - daemon mode → `Detach` (release leases + subscriptions; the daemon and
    ///   sessions survive, and the next GUI launch re-attaches to the same
    ///   `(daemon_instance_id, agent_id, generation, pid)`);
    /// - in-process fallback → the GUI owns the PTYs, so it must kill them
    ///   before exiting (there is no daemon to keep them alive);
    /// - unavailable → nothing to do.
    pub fn detach(&self) {
        match &self.owner {
            PtyOwner::InProcess(pty, _) => pty.kill_all(),
            PtyOwner::Daemon(client) => {
                self.closed.store(true, Ordering::Release);
                let _ = client.detach();
            }
            PtyOwner::Unavailable(_) => {}
        }
    }

    /// Offline lifecycle replay (Phase 4b, §6.2): pull every journaled event
    /// with `seq > last_seq` from the daemon. The frontend calls this once after
    /// registering its live listeners, passing the highest `event_seq` it has
    /// already applied — so natural exits / removals / hook-status transitions
    /// that happened while the GUI was offline are applied exactly once, in
    /// journal order. In-process fallback has no journal (its PTYs die with the
    /// GUI), so it reports nothing to replay.
    pub fn sync_events(&self, last_seq: u64) -> SyncEventsResult {
        match &self.owner {
            PtyOwner::Daemon(client) => match client.sync_events(last_seq) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("agent_sync_events: {e}");
                    SyncEventsResult {
                        last_seq,
                        events: Vec::new(),
                    }
                }
            },
            _ => SyncEventsResult {
                last_seq,
                events: Vec::new(),
            },
        }
    }

    fn generation(&self, agent_id: &str) -> Option<u64> {
        self.channels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(agent_id)
            .map(|c| c.generation)
    }
}

/// In-process fallback sink (§8): forwards PTY bytes to the frontend Tauri
/// Channel. A dead frontend Channel terminates the session — the pre-daemon
/// reader's "channel gone ⇒ kill" behavior, kept OUT of `pty_core` and owned by
/// the GUI bridge (§2.2). In daemon mode this adapter is replaced by the
/// bridge's channel registry + the daemon-side subscriber policy (§4.3).
struct ChannelSink {
    agent_id: String,
    channel: Channel<Vec<u8>>,
    pty: Arc<PtyCore>,
}

impl OutputSink for ChannelSink {
    fn send(&self, data: Vec<u8>) -> SinkResult {
        if self.channel.send(data).is_err() {
            // The WebView is gone (or saturated past the channel's limit).
            // Fallback policy: kill the session exactly as the old reader did.
            let _ = self.pty.kill(&self.agent_id);
            Err(SinkError::Closed)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::{DaemonConfig, DaemonServer};
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;
    use tauri::ipc::InvokeResponseBody;

    static BRIDGE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_base() -> PathBuf {
        let n = BRIDGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "capilot-bridge-test-{}-{n}",
            std::process::id()
        ))
    }

    /// A Tauri Channel that captures the bytes the bridge forwards, so tests
    /// assert on the same path the WebView receives.
    fn test_channel() -> (Channel<Vec<u8>>, std::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let channel = Channel::new(move |body: InvokeResponseBody| {
            if let InvokeResponseBody::Json(s) = body {
                // `Vec<u8>` is serialized as a JSON array over the IPC channel.
                if let Ok(bytes) = serde_json::from_str::<Vec<u8>>(&s) {
                    let _ = tx.send(bytes);
                }
            }
            Ok(())
        });
        (channel, rx)
    }

    #[test]
    fn in_process_spawn_write_kill_roundtrip() {
        let base = tmp_base();
        let lock = InstanceLock::try_acquire(&base).unwrap().unwrap();
        let bridge = PtyBridge::in_process(lock);
        let (channel, rx) = test_channel();

        let info = bridge
            .spawn(
                "a1",
                "/bin/sh",
                &["-c".into(), "echo __READY__; while read x; do echo \"got:$x\"; done".into()],
                &std::env::temp_dir(),
                24,
                80,
                &[],
                channel,
                None,
            )
            .unwrap();
        assert!(info.pid.is_some());

        // Output reaches the frontend channel (accumulate: PTY bytes can split
        // across reader chunks, exactly as the WebView sees them).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut collected: Vec<u8> = Vec::new();
        let mut ready = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx.recv_timeout(Duration::from_millis(100)) {
                collected.extend_from_slice(&data);
                if collected.windows(9).any(|w| w == b"__READY__") {
                    ready = true;
                    break;
                }
            }
        }
        assert!(ready, "in-process output not forwarded to the channel");

        // Writes round-trip through the PTY.
        bridge.write("a1", b"hello\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut echoed = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx.recv_timeout(Duration::from_millis(100)) {
                collected.extend_from_slice(&data);
                if collected.windows(9).any(|w| w == b"got:hello") {
                    echoed = true;
                    break;
                }
            }
        }
        assert!(echoed, "write was not echoed back");

        assert!(bridge.pids().iter().any(|(id, _, _)| id == "a1"));
        bridge.kill("a1").unwrap();
        // `write` after kill → not found (the in-process core reaped the entry).
        let err = bridge.write("a1", b"x\n").unwrap_err();
        assert!(matches!(err, AgentError::AgentNotFound(_)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn daemon_mode_routes_output_and_reemits_exit() {
        let base = tmp_base();
        let server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let thread = {
            let s = server.clone();
            std::thread::spawn(move || s.run())
        };

        let client = Arc::new(DaemonClient::connect(&base, APP_VERSION).unwrap());
        let bridge = Arc::new(PtyBridge::daemon(client.clone()));
        bridge.start_event_loop();
        let (channel, rx) = test_channel();

        let info = bridge
            .spawn(
                "b1",
                "/bin/sh",
                &["-c".into(), "echo __OK__; sleep 2".into()],
                &std::env::temp_dir(),
                24,
                80,
                &[],
                channel,
                None,
            )
            .unwrap();
        assert!(info.pid.is_some());

        // The daemon's real output events flow to the bridge's channel via the
        // event thread (end-to-end through the socket).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx.recv_timeout(Duration::from_millis(100)) {
                if data.windows(6).any(|w| w == b"__OK__") {
                    saw = true;
                    break;
                }
            }
        }
        assert!(saw, "daemon output not routed to the bridge channel");

        // Synthetic lifecycle events exercise the removal bookkeeping the event
        // thread performs (without needing a live WebView to emit to).
        bridge.handle_event(ClientEvent::Exited {
            agent_id: "b1".into(),
            generation: 1,
            exit_code: 0,
            event_seq: 1,
        });
        assert!(
            bridge.channels.lock().unwrap().get("b1").is_none(),
            "exited agent must be dropped from the channel registry"
        );

        // List reflects the live session; then close the daemon.
        let live = client.list().unwrap();
        assert!(live.iter().any(|s| s.agent_id == "b1"));
        client.shutdown().unwrap();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn daemon_attach_roundtrip_checkpoint_and_deduped_live() {
        let base = tmp_base();
        let server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let thread = {
            let s = server.clone();
            std::thread::spawn(move || s.run())
        };

        let client = Arc::new(DaemonClient::connect(&base, APP_VERSION).unwrap());
        let bridge = Arc::new(PtyBridge::daemon(client.clone()));
        bridge.start_event_loop();

        let (ch1, rx1) = test_channel();
        let info = bridge
            .spawn(
                "a1",
                "/bin/sh",
                &["-c".into(), "echo __OK__; while read x; do echo \"got:$x\"; done".into()],
                &std::env::temp_dir(),
                24,
                80,
                &[],
                ch1,
                None,
            )
            .unwrap();
        assert!(info.pid.is_some());

        // Banner reaches the spawn channel.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx1.recv_timeout(Duration::from_millis(100)) {
                if data.windows(6).any(|w| w == b"__OK__") {
                    saw = true;
                    break;
                }
            }
        }
        assert!(saw, "banner not received on the spawn channel");

        // A second client attaches: the first bytes it gets are the checkpoint,
        // which reconstructs the banner screen.
        let (ch2, rx2) = test_channel();
        let attached = bridge.attach("a1", 24, 80, ch2).unwrap();
        assert_eq!(attached.id, "a1");
        assert!(attached.pid.is_some());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut collected: Vec<u8> = Vec::new();
        let mut ckpt_seen = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx2.recv_timeout(Duration::from_millis(100)) {
                collected.extend_from_slice(&data);
                if collected.windows(6).any(|w| w == b"__OK__") {
                    ckpt_seen = true;
                    break;
                }
            }
        }
        assert!(ckpt_seen, "attach channel did not receive a checkpoint");

        // Live output flows to the attach channel exactly once — the two daemon
        // subscribers on the same connection (spawn + attach) both push every
        // chunk, and the bridge's per-generation seq dedupe must collapse them.
        bridge.write("a1", b"ping\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut echo_seen = false;
        while Instant::now() < deadline {
            if let Ok(data) = rx2.recv_timeout(Duration::from_millis(100)) {
                collected.extend_from_slice(&data);
                if collected.windows(8).any(|w| w == b"got:ping") {
                    echo_seen = true;
                }
            }
        }
        assert!(echo_seen, "attach channel did not receive the live echo");
        let count = collected.windows(8).filter(|w| *w == b"got:ping").count();
        assert_eq!(count, 1, "duplicate delivery of live output: {collected:?}");

        bridge.kill("a1").unwrap();
        client.shutdown().unwrap();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn attach_not_found_in_daemon_and_in_process() {
        let base = tmp_base();
        let server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let thread = {
            let s = server.clone();
            std::thread::spawn(move || s.run())
        };

        let client = Arc::new(DaemonClient::connect(&base, APP_VERSION).unwrap());
        let bridge = Arc::new(PtyBridge::daemon(client.clone()));
        bridge.start_event_loop();

        let (channel, _rx) = test_channel();
        // Unknown agent → AgentNotFound so agent_resume respawns (§6.3).
        let err = bridge.attach("ghost", 24, 80, channel).unwrap_err();
        assert!(matches!(err, AgentError::AgentNotFound(_)), "{err:?}");

        client.shutdown().unwrap();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);

        // In-process mode has nothing to attach to → AgentNotFound.
        let base2 = tmp_base();
        let lock = InstanceLock::try_acquire(&base2).unwrap().unwrap();
        let ip_bridge = PtyBridge::in_process(lock);
        let (channel, _rx) = test_channel();
        let err = ip_bridge.attach("x", 24, 80, channel).unwrap_err();
        assert!(matches!(err, AgentError::AgentNotFound(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&base2);
    }

    /// `PtyBridge::sync_events` (§6.2): daemon mode replays the journaled exit
    /// past the caller's watermark; in-process fallback has no journal and
    /// reports nothing.
    #[test]
    fn sync_events_replays_daemon_journal_and_is_empty_in_process() {
        let base = tmp_base();
        let server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let thread = {
            let s = server.clone();
            std::thread::spawn(move || s.run())
        };

        let client = Arc::new(DaemonClient::connect(&base, APP_VERSION).unwrap());
        let bridge = Arc::new(PtyBridge::daemon(client.clone()));
        // NOTE: no start_event_loop — the bridge's event thread would consume
        // the client's lifecycle events before the test can observe them. The
        // replay path under test (`sync_events`) doesn't need the event loop.

        // A natural exit lands in the daemon journal.
        let (_pid, _g) = client
            .spawn(
                "fast",
                "/bin/sh",
                &["-c".into(), "exit 7".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited_seq = None;
        while Instant::now() < deadline {
            match client.recv_event_timeout(Duration::from_millis(500)) {
                Ok(ClientEvent::Output { .. }) => continue,
                Ok(ClientEvent::Exited {
                    exit_code,
                    event_seq,
                    ..
                }) => {
                    assert_eq!(exit_code, 7);
                    exited_seq = Some(event_seq);
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        let exited_seq = exited_seq.expect("Exited not received");

        // Bridge replay from 0 delivers the exit; from the watermark delivers
        // nothing.
        let r = bridge.sync_events(0);
        assert_eq!(r.events.len(), 1, "{:?}", r.events);
        assert_eq!(r.events[0].kind, "exited");
        assert_eq!(r.events[0].exit_code, Some(7));
        assert!(r.last_seq >= exited_seq);
        assert!(bridge.sync_events(exited_seq).events.is_empty());

        client.shutdown().unwrap();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);

        // In-process fallback: no daemon journal → nothing to replay.
        let base2 = tmp_base();
        let lock = InstanceLock::try_acquire(&base2).unwrap().unwrap();
        let ip_bridge = PtyBridge::in_process(lock);
        let r = ip_bridge.sync_events(0);
        assert!(r.events.is_empty());
        assert_eq!(r.last_seq, 0);
        let _ = std::fs::remove_dir_all(&base2);
    }
}
