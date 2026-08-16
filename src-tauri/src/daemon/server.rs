//! The PTY daemon server (§3). A single process holds the instance lock, owns
//! `PtyCore` (plus a per-agent `AgentOutputHub`), serves the framed protocol on
//! a user-only Unix socket, and persists natural exits through `SessionStore` /
//! `LifecycleJournal`.
//!
//! Lifecycle rules enforced here:
//! - natural exit → `apply_natural_exit` + journal + `Exited`/`Removed` event to
//!   all connected clients (the GUI re-emits to the WebView);
//! - explicit `Kill` → pty_core suppresses the natural-exit callback, so the
//!   daemon removes the agent's hub itself;
//! - client disconnect → the client's output subscriber detaches via the hub's
//!   failing-subscriber rule; the PTY keeps running (§2.2);
//! - `Detach` (Phase 4) → release the client's input leases + subscriptions;
//!   the daemon and its sessions keep running (§9.4 — GUI exit detaches instead
//!   of shutting the daemon down);
//! - status-hook sidecar changes (`~/CaPilot/status/<id>.json`) are recorded in
//!   the journal and broadcast, so offline `working → idle` transitions are
//!   replayable when the GUI reconnects (§6.2/§9.4);
//! - `Shutdown` → stop accepting, `kill_all`, then return so the process exits.

use crate::agent_runtime::adapter::AgentError;
use crate::agent_runtime::pty_core::{OnExit, PtyCore, SinkError, SinkResult};
use crate::daemon::protocol::{
    encode_event_payload, write_frame, ClientEvent, Hello, HelloAck, JournalEvent,
    LiveSessionSummary, ProtocolErr, RequestCmd, Response, FRAME_EVENT, FRAME_HELLO,
    FRAME_HELLO_ACK, FRAME_RESPONSE, FRAME_ERROR, PROTOCOL_VERSION,
};
use crate::daemon::runtime::{
    ensure_run_dir, generate_token, make_instance_info, new_instance_id, socket_path,
    write_instance_info, write_token, InstanceLock,
};
use crate::lifecycle_journal::{LifecycleEvent, LifecycleEventKind, LifecycleJournal};
use crate::output_hub::{AgentOutputHub, HubSubscriber, OutputChunk};
use crate::session_store::SessionStore;
use std::collections::HashMap;
use std::io;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
use std::os::windows::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Per-agent daemon bookkeeping: the incarnation generation and its output hub.
struct AgentEntry {
    generation: u64,
    hub: Arc<AgentOutputHub>,
}

/// Per-connection state shared with output subscribers. `closed` is set when the
/// connection thread sees EOF/error; subscribers check it so a dead client is
/// detached by the hub's failing-subscriber rule instead of blocking the PTY.
struct ConnState {
    writer: Mutex<UnixStream>,
    closed: AtomicBool,
}

/// Hub subscriber that pushes an agent's output to one client connection as
/// `Output` event frames. A closed client returns `Err`, which the hub turns
/// into "remove this subscriber only" (§2.2).
struct ClientOutputSub {
    state: Arc<ConnState>,
    generation: u64,
}

impl HubSubscriber for ClientOutputSub {
    fn on_output(&self, chunk: OutputChunk) -> SinkResult {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(SinkError::Closed);
        }
        let ev = ClientEvent::Output {
            agent_id: chunk.agent_id,
            generation: self.generation,
            seq: chunk.seq,
            data: chunk.data,
        };
        let payload = match encode_event_payload(&ev) {
            Ok(p) => p,
            Err(_) => return Err(SinkError::Closed),
        };
        let mut w = self.state.writer.lock().unwrap_or_else(|p| p.into_inner());
        if write_frame(&mut *w, FRAME_EVENT, 0, &payload).is_err() {
            self.state.closed.store(true, Ordering::Release);
            return Err(SinkError::Closed);
        }
        Ok(())
    }
}

/// Configuration for a daemon server instance.
pub struct DaemonConfig {
    /// CaPilot base dir (`~/CaPilot`): `sessions.db`, `workspaces/`, `run/`.
    pub base: PathBuf,
    /// Application version sent in the handshake.
    pub app_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("another daemon instance is already running")]
    AlreadyRunning,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("bind: {0}")]
    Bind(String),
}

pub struct DaemonServer {
    pty: Arc<PtyCore>,
    store: Arc<SessionStore>,
    journal: Arc<LifecycleJournal>,
    agents: Arc<Mutex<HashMap<String, AgentEntry>>>,
    connections: Arc<Mutex<Vec<Arc<ConnState>>>>,
    /// Input control lease (§4.2): agent_id → the connection that spawned or
    /// last attached to it. Only the lease holder may `Write` to that agent, so
    /// two clients can't type into the same TUI. Released on conn close / kill.
    leases: Arc<Mutex<HashMap<String, Arc<ConnState>>>>,
    /// Status-hook sidecar dir (`~/CaPilot/status`). The monitor thread polls it
    /// and journals+broadcasts `working → idle`-style transitions so they survive
    /// a GUI offline window (§6.2/§9.4).
    status_dir: PathBuf,
    instance_id: String,
    token: String,
    listener: UnixListener,
    shutdown: AtomicBool,
    #[allow(dead_code)] // held for the whole server lifetime (flock)
    _lock: InstanceLock,
}

impl DaemonServer {
    /// Acquire the instance lock, write token + identity, and bind the socket.
    /// Returns the server (not yet accepting). Call [`DaemonServer::run`] to
    /// start serving.
    pub fn bind(config: DaemonConfig) -> Result<Arc<Self>, DaemonError> {
        ensure_run_dir(&config.base)?;
        let lock = match InstanceLock::try_acquire(&config.base)? {
            Some(l) => l,
            None => return Err(DaemonError::AlreadyRunning),
        };
        let token = generate_token();
        write_token(&config.base, &token)?;
        let instance_id = new_instance_id();
        write_instance_info(&config.base, &make_instance_info(&instance_id))?;

        // Holding the lock proves no other daemon owns the PTY set, so a stale
        // socket file is safe to unlink before binding (§4.1).
        let path = socket_path(&config.base);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .map_err(|e| DaemonError::Bind(format!("{}: {e}", path.display())))?;
        set_socket_perms(&path)?;

        let store = SessionStore::from_base(config.base.clone())
            .map_err(|e| DaemonError::Bind(format!("open store: {e}")))?;

        // Status-hook sidecars live under the CaPilot base dir in production
        // (`~/CaPilot/status`); in tests this is the temp base, so the monitor
        // never scans the real user status dir.
        let status_dir = config.base.join("status");

        Ok(Arc::new(Self {
            pty: Arc::new(PtyCore::new()),
            store: Arc::new(store),
            journal: Arc::new(LifecycleJournal::new()),
            agents: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(Vec::new())),
            leases: Arc::new(Mutex::new(HashMap::new())),
            status_dir,
            instance_id,
            token,
            listener,
            shutdown: AtomicBool::new(false),
            _lock: lock,
        }))
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Accept loop. Blocks until shutdown is requested, then kills every live
    /// PTY (explicit stop only, §9.4) and returns. A detached status-hook
    /// monitor runs alongside the accept loop for the server's whole lifetime.
    pub fn run(self: &Arc<Self>) {
        self.spawn_status_monitor();
        self.listener
            .set_nonblocking(true)
            .expect("daemon listener nonblocking");
        while !self.shutdown.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((stream, _)) => self.serve(stream),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    log::warn!("daemon accept error: {e}");
                    break;
                }
            }
        }
        // Intentional teardown → sessions stay `running` in the DB and resume
        // next launch (same semantic as the GUI's exit handler).
        self.pty.kill_all();
    }

    /// Ask the accept loop to stop.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    fn serve(self: &Arc<Self>, stream: UnixStream) {
        let server = self.clone();
        std::thread::Builder::new()
            .name("daemon-conn".into())
            .spawn(move || {
                let _ = server.serve_connection(stream);
            })
            .expect("spawn daemon connection thread");
    }

    fn serve_connection(&self, stream: UnixStream) -> io::Result<()> {
        // Write timeout so a slow/dead client can't stall a PTY reader thread
        // forever on an event write (§4.3 — Phase 3 replaces this with a bounded
        // send queue; a timeout-bounded write keeps the invariant today).
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;

        let mut reader = stream.try_clone()?;
        let conn = Arc::new(ConnState {
            writer: Mutex::new(stream),
            closed: AtomicBool::new(false),
        });

        // ── Handshake (§4.1) ──
        let frame = match crate::daemon::protocol::read_frame(&mut reader) {
            Ok(f) => f,
            Err(e) => {
                let _ = send_protocol_error(&conn, "bad_hello", &e.to_string());
                return Ok(());
            }
        };
        if frame.kind != FRAME_HELLO {
            send_protocol_error(&conn, "expect_hello", "first frame must be Hello")?;
            return Ok(());
        }
        let hello: Hello = match serde_json::from_slice(&frame.payload) {
            Ok(h) => h,
            Err(e) => {
                send_protocol_error(&conn, "bad_hello", &format!("invalid Hello: {e}"))?;
                return Ok(());
            }
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            send_protocol_error(
                &conn,
                "protocol_mismatch",
                &format!(
                    "client protocol {} != daemon protocol {PROTOCOL_VERSION}",
                    hello.protocol_version
                ),
            )?;
            return Ok(());
        }
        if hello.token != self.token {
            send_protocol_error(&conn, "bad_token", "token mismatch")?;
            return Ok(());
        }
        let ack = HelloAck {
            protocol_version: PROTOCOL_VERSION,
            daemon_instance_id: self.instance_id.clone(),
            capabilities: vec![
                crate::daemon::protocol::CAPABILITY_BASIC_IO.into(),
                crate::daemon::protocol::CAPABILITY_ATTACH.into(),
                crate::daemon::protocol::CAPABILITY_EVENT_REPLAY.into(),
            ],
        };
        let payload = serde_json::to_vec(&ack).expect("ack serializes");
        write_frame(
            &mut *conn.writer.lock().unwrap_or_else(|p| p.into_inner()),
            FRAME_HELLO_ACK,
            0,
            &payload,
        )?;

        self.connections
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(conn.clone());

        // ── Command loop ──
        loop {
            let frame = match crate::daemon::protocol::read_frame(&mut reader) {
                Ok(f) => f,
                Err(_) => break, // EOF / transport failure → detach
            };
            if frame.kind != crate::daemon::protocol::FRAME_REQUEST {
                let _ = send_protocol_error(&conn, "bad_frame", "expected request frame");
                break;
            }
            let req: RequestCmd = match serde_json::from_slice(&frame.payload) {
                Ok(r) => r,
                Err(e) => {
                    let _ = send_protocol_error(&conn, "bad_request", &format!("invalid request: {e}"));
                    break;
                }
            };
            let resp = self.handle_request(&conn, req);
            let payload = serde_json::to_vec(&resp).expect("response serializes");
            if write_frame(
                &mut *conn.writer.lock().unwrap_or_else(|p| p.into_inner()),
                FRAME_RESPONSE,
                frame.request_id,
                &payload,
            )
            .is_err()
            {
                break;
            }
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
        }

        conn.closed.store(true, Ordering::Release);
        self.connections
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|c| !Arc::ptr_eq(c, &conn));
        // A disconnected client can't steer input any more — release any input
        // lease it held so the next attach/spawn gets a clean control channel
        // (§4.2). The agent's output subscription dies via the hub's
        // failing-subscriber rule; the PTY keeps running (§2.2).
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, holder| !Arc::ptr_eq(holder, &conn));
        Ok(())
    }

    fn handle_request(&self, conn: &Arc<ConnState>, req: RequestCmd) -> Response {
        match req {
            RequestCmd::Spawn {
                agent_id,
                program,
                args,
                cwd,
                env,
                rows,
                cols,
            } => self.cmd_spawn(conn, agent_id, program, args, cwd, env, rows, cols),
            RequestCmd::Write {
                agent_id,
                generation,
                data,
            } => self.cmd_write(conn, &agent_id, generation, data),
            RequestCmd::Resize {
                agent_id,
                generation,
                rows,
                cols,
            } => self.cmd_resize(&agent_id, generation, rows, cols),
            RequestCmd::Kill {
                agent_id,
                generation,
            } => self.cmd_kill(&agent_id, generation),
            RequestCmd::Attach {
                agent_id,
                generation,
                rows,
                cols,
                after_seq,
            } => self.cmd_attach(conn, agent_id, generation, rows, cols, after_seq),
            RequestCmd::List => self.cmd_list(),
            RequestCmd::Detach => self.cmd_detach(conn),
            RequestCmd::SyncEvents { last_seq } => self.cmd_sync_events(last_seq),
            RequestCmd::Shutdown => {
                self.request_shutdown();
                Response::Ok
            }
        }
    }

    fn cmd_spawn(
        &self,
        conn: &Arc<ConnState>,
        agent_id: String,
        program: String,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
        rows: u16,
        cols: u16,
    ) -> Response {
        // The hub's parser starts at the spawn size so a checkpoint rendered
        // right after spawn (before any resize) already matches the PTY.
        let hub = Arc::new(AgentOutputHub::new(&agent_id, rows, cols));
        let on_exit = self.make_on_exit();
        match self.pty.spawn(
            agent_id.clone(),
            &program,
            &args,
            &PathBuf::from(&cwd),
            rows,
            cols,
            hub.clone(),
            Some(on_exit),
            &env,
        ) {
            Ok(info) => {
                // The reader thread may already have reaped an instant-exit child
                // between spawn returning and this lookup (its `on_exit` fired
                // and removed the entry). Only a live entry gets a subscription
                // and a hub entry; a reaped spawn is reported as a zero-generation
                // `Spawned` so the GUI still sees the (already-sent) `Exited`
                // event in order — real generations are ≥ 1 (see pty_core).
                let generation = match self.pty.generation(&agent_id) {
                    Some(g) => g,
                    None => {
                        return Response::Spawned {
                            agent_id,
                            pid: info.pid.unwrap_or(0),
                            generation: 0,
                        }
                    }
                };
                // Attach this client's output subscription before registering the
                // entry. `subscribe_from_beginning` replays the hub's retained log
                // (PTY output that arrived between spawn and subscribe) and then
                // streams live — the fresh client's terminal sees the whole
                // session, not just post-subscribe output (§5 minimal form).
                let sub = Arc::new(ClientOutputSub {
                    state: conn.clone(),
                    generation,
                });
                hub.subscribe_from_beginning(sub);
                self.agents
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        agent_id.clone(),
                        AgentEntry {
                            generation,
                            hub: hub.clone(),
                        },
                    );
                // The spawner is the session's input controller until a re-attach
                // (or disconnect) transfers the lease (§4.2).
                self.leases
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(agent_id.clone(), conn.clone());
                Response::Spawned {
                    agent_id,
                    pid: info.pid.unwrap_or(0),
                    generation,
                }
            }
            Err(e) => {
                // hub dropped with no subscribers → pre-attach buffer discarded.
                let (code, message) = match e {
                    AgentError::CapacityReached { limit } => {
                        ("capacity".to_string(), format!("session limit reached ({limit})"))
                    }
                    other => ("spawn".to_string(), other.to_string()),
                };
                Response::Error { code, message }
            }
        }
    }

    fn cmd_write(&self, conn: &Arc<ConnState>, agent_id: &str, generation: u64, data: String) -> Response {
        match self.pty.generation(agent_id) {
            Some(g) if g == generation => {
                // Input control lease (§4.2): a client that didn't spawn or
                // attach this agent may not write into its TUI. A lease held by
                // a disconnected conn is treated as free (its cleanup lags the
                // close by one lock acquisition).
                {
                    let leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(holder) = leases.get(agent_id) {
                        if !Arc::ptr_eq(holder, conn)
                            && !holder.closed.load(Ordering::Acquire)
                        {
                            return Response::Error {
                                code: "lease_held".into(),
                                message: "another client holds the input lease for this agent".into(),
                            };
                        }
                    }
                }
                match self.pty.write(agent_id, data.as_bytes()) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error {
                        code: "write".into(),
                        message: e.to_string(),
                    },
                }
            }
            Some(g) => Response::Error {
                code: "stale_generation".into(),
                message: format!("generation {generation} != live {g}"),
            },
            None => Response::Error {
                code: "not_found".into(),
                message: "agent not live".into(),
            },
        }
    }

    fn cmd_resize(&self, agent_id: &str, generation: u64, rows: u16, cols: u16) -> Response {
        match self.pty.generation(agent_id) {
            Some(g) if g == generation => {
                // Keep the hub's VT parser at the PTY's geometry so a later
                // checkpoint is rendered at the current size (§5).
                if let Some(e) = self.agents.lock().unwrap_or_else(|p| p.into_inner()).get(agent_id)
                {
                    e.hub.resize(rows, cols);
                }
                match self.pty.resize(agent_id, rows, cols) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error {
                        code: "resize".into(),
                        message: e.to_string(),
                    },
                }
            }
            Some(g) => Response::Error {
                code: "stale_generation".into(),
                message: format!("generation {generation} != live {g}"),
            },
            None => Response::Error {
                code: "not_found".into(),
                message: "agent not live".into(),
            },
        }
    }

    fn cmd_kill(&self, agent_id: &str, generation: Option<u64>) -> Response {
        if let Some(g) = generation {
            match self.pty.generation(agent_id) {
                Some(cur) if cur != g => {
                    return Response::Error {
                        code: "stale_generation".into(),
                        message: format!("generation {g} != live {cur}"),
                    }
                }
                None => {
                    return Response::Error {
                        code: "not_found".into(),
                        message: "agent not live".into(),
                    }
                }
                _ => {}
            }
        }
        let _ = self.pty.kill(agent_id);
        // Explicit kill suppresses the natural-exit callback, so the hub entry
        // is removed here (output subscription dies with it). The input lease
        // goes too — a killed agent has no controller.
        self.agents.lock().unwrap_or_else(|p| p.into_inner()).remove(agent_id);
        self.leases.lock().unwrap_or_else(|p| p.into_inner()).remove(agent_id);
        Response::Ok
    }

    /// Re-attach a client to a live agent (§4.2/§5). Under the hub's lock the
    /// daemon renders the checkpoint (or the `after_seq` gap), registers the
    /// client's output subscriber, and returns both — the client applies them
    /// then receives only `seq > snapshot_seq` live. The attaching client takes
    /// the input lease.
    fn cmd_attach(
        &self,
        conn: &Arc<ConnState>,
        agent_id: String,
        generation: u64,
        rows: u16,
        cols: u16,
        after_seq: Option<u64>,
    ) -> Response {
        // Liveness + incarnation check (same shape as cmd_write/cmd_resize).
        let hub = match self.pty.generation(&agent_id) {
            Some(g) if g == generation => match self
                .agents
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&agent_id)
            {
                Some(e) => e.hub.clone(),
                None => {
                    return Response::Error {
                        code: "not_found".into(),
                        message: "agent not live".into(),
                    }
                }
            },
            Some(g) => {
                return Response::Error {
                    code: "stale_generation".into(),
                    message: format!("generation {generation} != live {g}"),
                }
            }
            None => {
                return Response::Error {
                    code: "not_found".into(),
                    message: "agent not live".into(),
                }
            }
        };
        // §5: apply the client's initial_size BEFORE the snapshot so the
        // checkpoint is rendered at the terminal's real geometry (no TUI
        // residue when the new terminal is larger than the old one).
        let _ = self.pty.resize(&agent_id, rows, cols);
        hub.resize(rows, cols);

        let sub = Arc::new(ClientOutputSub {
            state: conn.clone(),
            generation,
        });
        let snap = hub.attach(sub, after_seq);

        // The attaching client takes over input control (§4.2).
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(agent_id, conn.clone());
        Response::Attached {
            snapshot_seq: snap.snapshot_seq,
            checkpoint: snap.checkpoint,
            replay: snap.replay,
        }
    }

    fn cmd_list(&self) -> Response {
        let pid_map: HashMap<String, u32> = self.pty.pids().into_iter().collect();
        let sessions = self
            .agents
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(|(id, e)| {
                let pid = pid_map.get(id).copied()?;
                Some(LiveSessionSummary {
                    agent_id: id.clone(),
                    pid,
                    generation: e.generation,
                    last_seq: e.hub.last_seq(),
                })
            })
            .collect();
        Response::Listed { sessions }
    }

    /// Phase 4 detach (§9.4): the GUI is going away but the daemon and its
    /// sessions keep running. Release every input lease this connection holds
    /// (a reconnecting GUI can immediately take control) and mark the conn
    /// closed so the hub's failing-subscriber rule drops its output
    /// subscriptions on the next chunk. The connection itself stays open until
    /// the client closes it (EOF cleanup in `serve_connection`), but it is
    /// inert from here.
    fn cmd_detach(&self, conn: &Arc<ConnState>) -> Response {
        self.leases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|_, holder| !Arc::ptr_eq(holder, conn));
        conn.closed.store(true, Ordering::Release);
        Response::Ok
    }

    /// Phase 4 offline replay (§6.2): return every journaled lifecycle event
    /// with `seq > last_seq` plus the journal's current high-water mark. The
    /// client applies them in order and then dedupes live lifecycle events by
    /// `event_seq`, so nothing that happened while it was offline is lost
    /// (natural exits, delete-mode removals, hook-status transitions).
    fn cmd_sync_events(&self, last_seq: u64) -> Response {
        let events: Vec<JournalEvent> = self
            .journal
            .since(last_seq)
            .into_iter()
            .map(journal_event_to_protocol)
            .collect();
        Response::EventLog {
            last_seq: self.journal.last_seq(),
            events,
        }
    }

    /// Poll the status-hook sidecar dir and journal+broadcast each observed
    /// transition. Runs for the daemon's whole lifetime. The FIRST scan seeds
    /// the baseline without recording, so only changes that happen while this
    /// daemon is running become events — a GUI that was offline across a
    /// `working → idle` transition gets it on reconnect, while stale files from
    /// before daemon start are not replayed as if they were new.
    fn spawn_status_monitor(self: &Arc<Self>) {
        let server = self.clone();
        std::thread::Builder::new()
            .name("capilot-status-monitor".into())
            .spawn(move || {
                // agent_id → (status, ts). Presence = baseline seen.
                let mut seen: HashMap<String, (String, i64)> = HashMap::new();
                let mut first_scan = true;
                while !server.shutdown.load(Ordering::Acquire) {
                    let mut changed: Vec<(String, String, i64)> = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(&server.status_dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                                continue;
                            };
                            // Only per-agent sidecars (`<id>.json`); skip hook.sh
                            // and hooks.json and any non-agent file.
                            let Some(agent_id) = name.strip_suffix(".json") else {
                                continue;
                            };
                            if agent_id.is_empty()
                                || agent_id.starts_with("hook")
                                || agent_id.ends_with(".tmp")
                            {
                                continue;
                            }
                            let Ok(raw) = std::fs::read_to_string(&path) else {
                                continue;
                            };
                            let Ok(v) = serde_json::from_str::<StatusSidecar>(&raw) else {
                                continue;
                            };
                            let key = (v.status.clone(), v.ts);
                            if !first_scan && seen.get(agent_id) != Some(&key) {
                                changed.push((agent_id.to_string(), v.status, v.ts));
                            }
                            seen.insert(agent_id.to_string(), key);
                        }
                    }
                    first_scan = false;
                    for (agent_id, status, ts) in changed {
                        server.record_hook_status(&agent_id, &status, ts);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .expect("spawn status monitor");
    }

    /// Journal a hook-status transition and broadcast it to connected clients.
    fn record_hook_status(&self, agent_id: &str, status: &str, ts: i64) {
        let event_seq = self.journal.record(
            agent_id,
            LifecycleEventKind::HookStatus,
            Some(serde_json::json!({ "status": status, "ts": ts })),
        );
        let ev = ClientEvent::HookStatus {
            agent_id: agent_id.to_string(),
            status: status.to_string(),
            ts,
            event_seq,
        };
        broadcast_client_event(&self.connections, &ev);
    }

    /// Daemon-side natural-exit handler: persist via `SessionStore`, record in
    /// the `LifecycleJournal`, then broadcast the matching event to clients. The
    /// GUI receives it and re-emits to the WebView — "persist the event" and
    /// "tell the frontend" stay layered (§6.1).
    fn make_on_exit(&self) -> OnExit {
        let store = self.store.clone();
        let journal = self.journal.clone();
        let agents = self.agents.clone();
        let leases = self.leases.clone();
        let connections = self.connections.clone();
        Arc::new(move |agent_id, exit_code| {
            let generation = agents
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&agent_id)
                .map(|e| e.generation)
                .unwrap_or(0);
            // A natural exit ends the input lease too — the agent is gone, so
            // no one may write to it (§4.2).
            leases
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&agent_id);
            let outcome = store.apply_natural_exit(&agent_id);
            let event_seq = if outcome.deleted {
                journal.record(&agent_id, LifecycleEventKind::Removed, None)
            } else {
                journal.record(
                    &agent_id,
                    LifecycleEventKind::Exited,
                    Some(serde_json::json!({ "exit_code": exit_code })),
                )
            };
            let ev = if outcome.deleted {
                ClientEvent::Removed {
                    agent_id,
                    generation,
                    event_seq,
                }
            } else {
                ClientEvent::Exited {
                    agent_id,
                    generation,
                    exit_code,
                    event_seq,
                }
            };
            broadcast_client_event(&connections, &ev);
        })
    }
}

/// Push one lifecycle event to every open client connection. A dead/slow
/// client is marked closed (its subscriber then detaches via the hub's
/// failing-subscriber rule); the others get the frame. Free function so both
/// the natural-exit handler and the status monitor share one broadcast path.
fn broadcast_client_event(connections: &Mutex<Vec<Arc<ConnState>>>, ev: &ClientEvent) {
    let Ok(payload) = encode_event_payload(ev) else {
        return;
    };
    let conns = connections.lock().unwrap_or_else(|p| p.into_inner());
    for c in conns.iter() {
        if c.closed.load(Ordering::Acquire) {
            continue;
        }
        let mut w = c.writer.lock().unwrap_or_else(|p| p.into_inner());
        if write_frame(&mut *w, FRAME_EVENT, 0, &payload).is_err() {
            c.closed.store(true, Ordering::Release);
        }
    }
}

/// Raw shape of a status-hook sidecar (`~/CaPilot/status/<agent_id>.json`).
#[derive(Debug, serde::Deserialize)]
struct StatusSidecar {
    status: String,
    ts: i64,
}

/// Convert a journaled lifecycle event into the wire `JournalEvent` used by
/// `Response::EventLog`. The `kind` string matches `LifecycleEventKind`'s
/// snake_case serialization; `exit_code`/`status` are lifted from the payload.
fn journal_event_to_protocol(ev: LifecycleEvent) -> JournalEvent {
    let (kind, exit_code, status) = match ev.kind {
        LifecycleEventKind::Exited => (
            "exited".to_string(),
            ev.payload.as_ref().and_then(|p| p.get("exit_code")),
            None,
        ),
        LifecycleEventKind::Removed => ("removed".to_string(), None, None),
        LifecycleEventKind::HookStatus => (
            "hook_status".to_string(),
            None,
            ev.payload.as_ref().and_then(|p| p.get("status")),
        ),
    };
    JournalEvent {
        seq: ev.seq,
        ts: ev.ts,
        agent_id: ev.agent_id,
        kind,
        exit_code: exit_code.and_then(|v| v.as_i64()).map(|v| v as i32),
        status: status.and_then(|v| v.as_str()).map(|s| s.to_string()),
    }
}

fn send_protocol_error(conn: &Arc<ConnState>, code: &str, message: &str) -> io::Result<()> {
    let payload = serde_json::to_vec(&ProtocolErr {
        code: code.to_string(),
        message: message.to_string(),
    })
    .expect("err serializes");
    write_frame(
        &mut *conn.writer.lock().unwrap_or_else(|p| p.into_inner()),
        FRAME_ERROR,
        0,
        &payload,
    )
    .map_err(From::from)
}

#[cfg(unix)]
fn set_socket_perms(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(windows)]
fn set_socket_perms(_path: &std::path::Path) -> io::Result<()> {
    // Windows AF_UNIX socket access is governed by the socket's security
    // descriptor at bind time, not a chmod-style mode; the socket path lives
    // under the instance run dir, which is private to the user. Nothing to do.
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::daemon::protocol::{
        decode_event_payload, read_frame, write_frame, HelloAck, FRAME_ERROR, FRAME_HELLO_ACK,
        FRAME_REQUEST,
    };
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};

    static SERVER_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_base() -> PathBuf {
        std::env::temp_dir().join(format!(
            "capilot_daemon_server_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERVER_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Test client over a raw UnixStream (exercise the wire protocol directly,
    /// not the convenience client). Event frames that arrive while a request is
    /// in flight are buffered, not dropped — mirroring the real `DaemonClient`,
    /// so an instant-exit `Exited` that races ahead of its `Spawned` response is
    /// still observable.
    struct TestClient {
        stream: UnixStream,
        next_rid: u64,
        pending_events: std::collections::VecDeque<ClientEvent>,
    }

    impl TestClient {
        fn connect(base: &PathBuf, token: &str) -> Self {
            let path = socket_path(base);
            let stream = UnixStream::connect(&path).expect("connect to daemon socket");
            let mut c = Self {
                stream,
                next_rid: 1,
                pending_events: std::collections::VecDeque::new(),
            };
            let hello = Hello {
                protocol_version: PROTOCOL_VERSION,
                app_version: "test".into(),
                token: token.to_string(),
            };
            write_frame(
                &mut c.stream,
                FRAME_HELLO,
                0,
                &serde_json::to_vec(&hello).unwrap(),
            )
            .unwrap();
            let ack = read_frame(&mut c.stream).unwrap();
            assert_eq!(ack.kind, FRAME_HELLO_ACK);
            let _ack: HelloAck = serde_json::from_slice(&ack.payload).unwrap();
            c
        }

        fn request(&mut self, req: &RequestCmd) -> Response {
            let rid = self.next_rid;
            self.next_rid += 1;
            write_frame(
                &mut self.stream,
                FRAME_REQUEST,
                rid,
                &serde_json::to_vec(req).unwrap(),
            )
            .unwrap();
            loop {
                let f = read_frame(&mut self.stream).unwrap();
                if f.kind == FRAME_RESPONSE && f.request_id == rid {
                    return serde_json::from_slice(&f.payload).unwrap();
                }
                if f.kind == FRAME_EVENT {
                    if let Ok(ev) = decode_event_payload(&f.payload) {
                        self.pending_events.push_back(ev);
                    }
                }
            }
        }

        /// Read the next event frame (panics on protocol failure/timeout).
        fn next_event(&mut self, timeout: Duration) -> ClientEvent {
            if let Some(ev) = self.pending_events.pop_front() {
                return ev;
            }
            self.stream.set_read_timeout(Some(timeout)).unwrap();
            let f = read_frame(&mut self.stream).unwrap();
            assert_eq!(f.kind, FRAME_EVENT, "expected event frame");
            decode_event_payload(&f.payload).unwrap()
        }

        fn spawn_echo(&mut self, agent_id: &str) -> u64 {
            let resp = self.request(&RequestCmd::Spawn {
                agent_id: agent_id.into(),
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "echo __READY__; while read x; do echo \"got:$x\"; done".into(),
                ],
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                env: vec![],
                rows: 24,
                cols: 80,
            });
            match resp {
                Response::Spawned { generation, pid, .. } => {
                    assert!(pid > 0);
                    generation
                }
                other => panic!("spawn failed: {other:?}"),
            }
        }

        /// Spawn an instant-exit program (`/bin/true`) — the daemon's
        /// natural-exit path records an `Exited` journal event shortly after.
        fn spawn_instant(&mut self, agent_id: &str) -> u64 {
            let resp = self.request(&RequestCmd::Spawn {
                agent_id: agent_id.into(),
                program: "/bin/true".into(),
                args: vec![],
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                env: vec![],
                rows: 24,
                cols: 80,
            });
            match resp {
                Response::Spawned { generation, pid, .. } => {
                    assert!(pid > 0);
                    generation
                }
                other => panic!("spawn failed: {other:?}"),
            }
        }
    }

    #[test]
    fn handshake_wrong_token_is_rejected() {
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

        let mut stream = UnixStream::connect(socket_path(&base)).unwrap();
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: "test".into(),
            token: "wrong".into(),
        };
        write_frame(&mut stream, FRAME_HELLO, 0, &serde_json::to_vec(&hello).unwrap()).unwrap();
        // The server sends an ERROR frame, then closes.
        let err = read_frame(&mut stream).unwrap();
        assert_eq!(err.kind, FRAME_ERROR);
        // Read again → EOF.
        assert!(read_frame(&mut stream).is_err());

        server.request_shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn spawn_write_list_kill_roundtrip_with_output() {
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

        let mut client = TestClient::connect(&base, server.token());

        let g = client.spawn_echo("a1");
        assert!(g >= 1);

        // Output events: __READY__ should arrive (pre-attach buffer + live).
        let ev = client.next_event(Duration::from_secs(5));
        match ev {
            ClientEvent::Output { agent_id, data, .. } => {
                assert_eq!(agent_id, "a1");
                assert!(
                    String::from_utf8_lossy(&data).contains("__READY__"),
                    "first output must include the banner: {:?}",
                    String::from_utf8_lossy(&data)
                );
            }
            other => panic!("expected Output, got {other:?}"),
        }

        // Write with the correct generation → echo "got:ping".
        let resp = client.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "ping\n".into(),
        });
        assert!(matches!(resp, Response::Ok), "{resp:?}");

        // Collect output until the echo line appears.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = String::new();
        while Instant::now() < deadline {
            match client.next_event(Duration::from_millis(500)) {
                ClientEvent::Output { data, .. } => {
                    got.push_str(&String::from_utf8_lossy(&data));
                    if got.contains("got:ping") {
                        break;
                    }
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert!(got.contains("got:ping"), "echo output missing: {got:?}");

        // List shows the live session.
        let resp = client.request(&RequestCmd::List);
        match resp {
            Response::Listed { sessions } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].agent_id, "a1");
                assert_eq!(sessions[0].generation, g);
                assert!(sessions[0].pid > 0);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Stale generation write is rejected.
        let resp = client.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g + 999,
            data: "nope\n".into(),
        });
        assert!(matches!(resp, Response::Error { ref code, .. } if code == "stale_generation"));

        // Kill → OK, then List is empty, and no natural-exit event is sent.
        let resp = client.request(&RequestCmd::Kill {
            agent_id: "a1".into(),
            generation: Some(g),
        });
        assert!(matches!(resp, Response::Ok), "{resp:?}");
        let resp = client.request(&RequestCmd::List);
        match resp {
            Response::Listed { sessions } => assert!(sessions.is_empty()),
            other => panic!("unexpected {other:?}"),
        }

        // Graceful shutdown.
        let resp = client.request(&RequestCmd::Shutdown);
        assert!(matches!(resp, Response::Ok));
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn natural_exit_persists_and_broadcasts_exited_event() {
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

        let mut client = TestClient::connect(&base, server.token());

        // The GUI inserts the session row at spawn; the daemon only applies the
        // natural-exit policy to the existing row. Simulate the GUI here, before
        // the child exits.
        let store = SessionStore::from_base(base.clone()).unwrap();
        store
            .db()
            .lock()
            .unwrap()
            .insert(&crate::persistence::AgentSessionRecord {
                id: "fast".into(),
                workspace_id: None,
                project: "p".into(),
                runtime: "claude".into(),
                resume_key: None,
                cwd: crate::persistence::agent_dir("p", "fast"),
                title: "t".into(),
                status: "running".into(),
                mode: "ask".into(),
                speed: "auto".into(),
                model: None,
                created_at: 1,
                updated_at: 1,
            })
            .expect("insert row");

        // Spawn a shell that exits immediately with code 7.
        let resp = client.request(&RequestCmd::Spawn {
            agent_id: "fast".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            env: vec![],
            rows: 24,
            cols: 80,
        });
        let generation = match resp {
            Response::Spawned { generation, .. } => generation,
            other => panic!("spawn failed: {other:?}"),
        };

        // The daemon should broadcast an Exited event (default session_end_mode
        // keeps the session → status done).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got_exited = false;
        while Instant::now() < deadline {
            let ev = client.next_event(Duration::from_millis(500));
            match ev {
                ClientEvent::Output { .. } => continue,
                ClientEvent::Exited {
                    agent_id,
                    generation: gen,
                    exit_code,
                    ..
                } => {
                    assert_eq!(agent_id, "fast");
                    assert_eq!(gen, generation);
                    assert_eq!(exit_code, 7);
                    got_exited = true;
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(got_exited, "Exited event not received");

        // The session row should be marked done (default keep policy).
        let rec = store.db().lock().unwrap().get("fast").unwrap();
        let rec = rec.expect("session row persisted");
        assert_eq!(rec.status, "done");

        // List is empty (entry removed on exit).
        let resp = client.request(&RequestCmd::List);
        match resp {
            Response::Listed { sessions } => assert!(sessions.is_empty()),
            other => panic!("unexpected {other:?}"),
        }

        client.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Read output events with a deadline until `needle` appears, returning the
    /// accumulated text and the highest output seq seen. Panics if the needle
    /// never shows up.
    fn read_until(client: &mut TestClient, needle: &str, timeout: Duration) -> (String, u64) {
        let deadline = Instant::now() + timeout;
        let mut got = String::new();
        let mut last_seq = 0u64;
        while Instant::now() < deadline {
            match client.next_event(Duration::from_millis(500)) {
                ClientEvent::Output { seq, data, .. } => {
                    last_seq = last_seq.max(seq);
                    got.push_str(&String::from_utf8_lossy(&data));
                    if got.contains(needle) {
                        return (got, last_seq);
                    }
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        panic!("needle {needle:?} not seen; got {got:?}");
    }

    #[test]
    fn attach_fresh_client_gets_checkpoint_then_live_only() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        let g = c1.spawn_echo("a1");
        let (_, _) = read_until(&mut c1, "__READY__", Duration::from_secs(5));

        // A second client attaches with no baseline → full checkpoint, no gap.
        let mut c2 = TestClient::connect(&base, server.token());
        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g,
            rows: 24,
            cols: 80,
            after_seq: None,
        });
        let (snapshot_seq, checkpoint, replay) = match resp {
            Response::Attached {
                snapshot_seq,
                checkpoint,
                replay,
            } => {
                assert!(checkpoint.is_some(), "fresh attach must get a checkpoint");
                assert!(replay.is_empty(), "no gap for a fresh attach");
                (snapshot_seq, checkpoint.unwrap(), replay)
            }
            other => panic!("attach failed: {other:?}"),
        };
        let _ = replay;

        // The checkpoint reconstructs the current screen (includes the banner).
        let mut p = vt100::Parser::new(24, 80, 200);
        p.process(&checkpoint);
        assert!(
            p.screen().contents().contains("__READY__"),
            "checkpoint must rebuild the banner screen: {:?}",
            p.screen().contents()
        );

        // Live output reaches the attaching client only for seq > snapshot_seq.
        // c2 now holds the input lease (attach transfers it), so c2 writes.
        let resp = c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "ping\n".into(),
        });
        assert!(matches!(resp, Response::Ok), "{resp:?}");
        let (got, seq) = read_until(&mut c2, "got:ping", Duration::from_secs(5));
        assert!(
            seq > snapshot_seq,
            "live event seq {seq} must be > snapshot {snapshot_seq}"
        );
        assert!(got.contains("got:ping"));

        c1.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn attach_with_after_seq_replays_only_the_gap() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        let g = c1.spawn_echo("a1");
        let (_, first_seq) = read_until(&mut c1, "__READY__", Duration::from_secs(5));

        // A client current through first_seq → no checkpoint, no gap.
        let mut c2 = TestClient::connect(&base, server.token());
        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g,
            rows: 24,
            cols: 80,
            after_seq: Some(first_seq),
        });
        match resp {
            Response::Attached {
                snapshot_seq,
                checkpoint,
                replay,
            } => {
                assert_eq!(snapshot_seq, first_seq);
                assert!(checkpoint.is_none());
                assert!(replay.is_empty());
            }
            other => panic!("attach failed: {other:?}"),
        }

        // Produce a gap: c2 (the lease holder after its attach) writes, and c2
        // sees the echo at a higher seq.
        let resp = c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "gap\n".into(),
        });
        assert!(matches!(resp, Response::Ok), "{resp:?}");
        let (_, gap_seq) = read_until(&mut c2, "got:gap", Duration::from_secs(5));
        assert!(gap_seq > first_seq, "gap seq {gap_seq} must exceed {first_seq}");

        // A NEW client attaching at first_seq gets ONLY the gap bytes.
        let mut c3 = TestClient::connect(&base, server.token());
        let resp = c3.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g,
            rows: 24,
            cols: 80,
            after_seq: Some(first_seq),
        });
        match resp {
            Response::Attached {
                snapshot_seq,
                checkpoint,
                replay,
            } => {
                assert_eq!(snapshot_seq, gap_seq);
                assert!(checkpoint.is_none(), "current client needs no checkpoint");
                let replay_text = String::from_utf8_lossy(&replay);
                assert!(
                    replay_text.contains("got:gap"),
                    "replay must carry the gap bytes: {replay_text:?}"
                );
                assert!(
                    !replay_text.contains("__READY__"),
                    "replay must not re-send pre-gap bytes: {replay_text:?}"
                );
            }
            other => panic!("attach failed: {other:?}"),
        }

        c1.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn input_lease_transfers_on_attach_and_rejects_foreign_writer() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        let g = c1.spawn_echo("a1");
        let (_, _) = read_until(&mut c1, "__READY__", Duration::from_secs(5));

        let mut c2 = TestClient::connect(&base, server.token());

        // c1 spawned → holds the lease; c2 is a foreign writer.
        let resp = c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "hi\n".into(),
        });
        assert!(
            matches!(resp, Response::Error { ref code, .. } if code == "lease_held"),
            "foreign writer must be rejected: {resp:?}"
        );

        // c2 attaches → takes over the lease (§4.2).
        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g,
            rows: 24,
            cols: 80,
            after_seq: None,
        });
        assert!(matches!(resp, Response::Attached { .. }), "{resp:?}");

        // Now c1 (former holder) is rejected and c2 writes fine.
        let resp = c1.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "x\n".into(),
        });
        assert!(
            matches!(resp, Response::Error { ref code, .. } if code == "lease_held"),
            "old holder must lose write access: {resp:?}"
        );
        let resp = c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "y\n".into(),
        });
        assert!(matches!(resp, Response::Ok), "{resp:?}");
        read_until(&mut c2, "got:y", Duration::from_secs(5));

        c1.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn attach_rejects_stale_generation_and_not_found() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        let g = c1.spawn_echo("a1");
        let (_, _) = read_until(&mut c1, "__READY__", Duration::from_secs(5));

        let mut c2 = TestClient::connect(&base, server.token());

        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g + 100,
            rows: 24,
            cols: 80,
            after_seq: None,
        });
        assert!(
            matches!(resp, Response::Error { ref code, .. } if code == "stale_generation"),
            "{resp:?}"
        );

        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "ghost".into(),
            generation: 1,
            rows: 24,
            cols: 80,
            after_seq: None,
        });
        assert!(
            matches!(resp, Response::Error { ref code, .. } if code == "not_found"),
            "{resp:?}"
        );

        c1.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_lock_prevents_second_daemon() {
        let base = tmp_base();
        let _server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let second = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        });
        assert!(matches!(second, Err(DaemonError::AlreadyRunning)));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Detach (§9.4) releases the client's input lease but leaves the daemon
    /// and the session running: after `Detach`, another client can attach and
    /// take over the lease, and writes flow again. (This is the GUI-restart
    /// handshake: exit → detach, relaunch → attach to the same generation.)
    #[test]
    fn detach_releases_lease_keeps_session_live() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        let g = c1.spawn_echo("a1");
        let _ = read_until(&mut c1, "__READY__", Duration::from_secs(5));

        // c1 owns the lease; a foreign writer is rejected.
        let mut c2 = TestClient::connect(&base, server.token());
        let resp = c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "nope\n".into(),
        });
        assert!(
            matches!(resp, Response::Error { ref code, .. } if code == "lease_held"),
            "foreign write before detach must be rejected: {resp:?}"
        );

        // Detach c1: lease released, session untouched, daemon still up.
        assert!(matches!(c1.request(&RequestCmd::Detach), Response::Ok));

        // c2 attaches and takes the lease → its write echoes.
        let resp = c2.request(&RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: g,
            rows: 24,
            cols: 80,
            after_seq: None,
        });
        assert!(matches!(resp, Response::Attached { .. }), "{resp:?}");
        c2.request(&RequestCmd::Write {
            agent_id: "a1".into(),
            generation: g,
            data: "ping\n".into(),
        });
        read_until(&mut c2, "got:ping", Duration::from_secs(5));

        c2.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Offline replay (§6.2): a client syncs from its high-water mark and gets
    /// exactly the journaled events past that point, plus the watermark. Events
    /// recorded before the replay (here: a natural exit) are delivered once.
    #[test]
    fn sync_events_replays_journal_and_watermark() {
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

        let mut c1 = TestClient::connect(&base, server.token());
        c1.spawn_instant("b1");
        // The natural exit is journaled + broadcast.
        let exited_seq = match c1.next_event(Duration::from_secs(5)) {
            ClientEvent::Exited {
                agent_id,
                event_seq,
                ..
            } => {
                assert_eq!(agent_id, "b1");
                event_seq
            }
            other => panic!("expected Exited, got {other:?}"),
        };

        // A fresh client replays from 0 and sees the exit + watermark.
        let mut c2 = TestClient::connect(&base, server.token());
        match c2.request(&RequestCmd::SyncEvents { last_seq: 0 }) {
            Response::EventLog { last_seq, events } => {
                assert!(last_seq >= exited_seq, "watermark must advance");
                assert_eq!(events.len(), 1, "only the recorded exit: {events:?}");
                assert_eq!(events[0].kind, "exited");
                assert_eq!(events[0].agent_id, "b1");
                assert_eq!(events[0].exit_code, Some(0));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        // Syncing from the delivered watermark yields nothing new.
        match c2.request(&RequestCmd::SyncEvents {
            last_seq: exited_seq,
        }) {
            Response::EventLog { events, .. } => {
                assert!(events.is_empty(), "no replay past the watermark")
            }
            other => panic!("unexpected response: {other:?}"),
        }

        c2.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The status monitor seeds a baseline on its first scan (no event) and
    /// journals + broadcasts only subsequent transitions — so a GUI that was
    /// offline across a `working → idle` move gets exactly that transition on
    /// reconnect, not a stale replay of pre-daemon files.
    #[test]
    fn status_monitor_seeds_baseline_then_records_transitions() {
        use std::io::Write;

        let base = tmp_base();
        let status_dir = base.join("status");
        std::fs::create_dir_all(&status_dir).unwrap();
        // Baseline exists BEFORE the daemon starts: the first scan seeds it.
        write_status_sidecar(&status_dir, "a1", "working", 1);

        let server = DaemonServer::bind(DaemonConfig {
            base: base.clone(),
            app_version: "test".into(),
        })
        .unwrap();
        let thread = {
            let s = server.clone();
            std::thread::spawn(move || s.run())
        };
        let mut c1 = TestClient::connect(&base, server.token());

        // Give the monitor time for its first scan + a full idle poll. Then the
        // baseline must NOT be in the journal (seeded, not recorded).
        std::thread::sleep(Duration::from_millis(1200));
        match c1.request(&RequestCmd::SyncEvents { last_seq: 0 }) {
            Response::EventLog { events, .. } => {
                assert!(
                    events.is_empty(),
                    "pre-daemon baseline must be seeded, not recorded: {events:?}"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        // working → idle: journaled + broadcast as HookStatus.
        write_status_sidecar(&status_dir, "a1", "idle", 2);
        match c1.next_event(Duration::from_secs(5)) {
            ClientEvent::HookStatus {
                agent_id,
                status,
                ts,
                event_seq,
            } => {
                assert_eq!(agent_id, "a1");
                assert_eq!(status, "idle");
                assert_eq!(ts, 2);
                assert!(event_seq >= 1);
            }
            other => panic!("expected HookStatus, got {other:?}"),
        }

        // And it's replayable for a reconnecting client.
        match c1.request(&RequestCmd::SyncEvents { last_seq: 0 }) {
            Response::EventLog { events, .. } => {
                let hook: Vec<_> = events.iter().filter(|e| e.kind == "hook_status").collect();
                assert_eq!(hook.len(), 1, "one transition recorded: {events:?}");
                assert_eq!(hook[0].agent_id, "a1");
                assert_eq!(hook[0].status.as_deref(), Some("idle"));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        c1.request(&RequestCmd::Shutdown);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);

        fn write_status_sidecar(dir: &std::path::Path, agent_id: &str, status: &str, ts: i64) {
            let mut f = std::fs::File::create(dir.join(format!("{agent_id}.json"))).unwrap();
            writeln!(f, "{}", serde_json::json!({ "status": status, "ts": ts })).unwrap();
        }
    }
}
