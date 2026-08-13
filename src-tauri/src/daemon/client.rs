//! GUI-side daemon client (§4). Connects to the Unix socket, handshakes with
//! the token from the runtime file, sends control requests, and receives output
//! / lifecycle events. The reader thread routes responses back to awaiting
//! callers by `request_id` and pushes events into a channel the GUI consumes.
//!
//! §8 fallback rules are enforced by the caller (`bridge.rs`): a `NotRunning`
//! connect lets the GUI decide whether to spawn a daemon or fall back to the
//! in-process `PtyCore`; handshake/protocol failures are hard errors, never a
//! silent fallback.

use crate::daemon::protocol::{
    decode_event_payload, read_frame, write_frame, ClientEvent, Hello, HelloAck, JournalEvent,
    LiveSessionSummary, ProtocolErr, RequestCmd, Response, FRAME_EVENT, FRAME_HELLO, FRAME_HELLO_ACK,
    FRAME_REQUEST, FRAME_RESPONSE, PROTOCOL_VERSION,
};
use crate::daemon::runtime::{read_token, socket_path};
use serde::Serialize;
use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long to wait for a response to a request before declaring the daemon
/// unresponsive. Events don't count toward this — they interleave freely.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
/// Write timeout on the client socket (mirrors the daemon's 500ms; a wedged
/// socket fails a request instead of stalling the GUI's async command forever).
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_millis(500);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Shared connection state between the caller and the reader thread.
#[derive(Debug)]
struct ConnState {
    writer: Mutex<UnixStream>,
    closed: AtomicBool,
    pending: Mutex<HashMap<u64, std::sync::mpsc::Sender<Response>>>,
    events_tx: std::sync::mpsc::Sender<ClientEvent>,
}

/// Errors surfaced to the GUI bridge.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("no daemon running")]
    NotRunning,
    #[error("daemon handshake failed: {0}")]
    Handshake(String),
    #[error("daemon closed the connection")]
    Closed,
    #[error("response timeout")]
    Timeout,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("daemon error: {message}")]
    Request { code: String, message: String },
}

/// Transport/protocol failures from the frame layer become `Io` (connection
/// problem) or `Handshake` (protocol violation). Both are surfaced verbatim —
/// a malformed frame from a live daemon is a hard error, never a silent
/// fallback (§8).
impl From<crate::daemon::protocol::ProtocolError> for ClientError {
    fn from(e: crate::daemon::protocol::ProtocolError) -> Self {
        match e {
            crate::daemon::protocol::ProtocolError::Io(io) => ClientError::Io(io),
            other => ClientError::Handshake(other.to_string()),
        }
    }
}

/// Result of a daemon `Attach` (§4.2): a terminal checkpoint (full visible
/// screen, rendered at the client's rows/cols) plus a gap `replay` (raw bytes
/// the client is missing since its `after_seq`). Apply checkpoint → replay →
/// then forward live `Output` events with `seq > snapshot_seq`.
#[derive(Debug)]
pub struct AttachResult {
    pub snapshot_seq: u64,
    pub checkpoint: Option<Vec<u8>>,
    pub replay: Vec<u8>,
}

/// Result of `DaemonClient::sync_events`: journaled events past the caller's
/// watermark plus the journal's new high-water mark (Phase 4b).
#[derive(Debug, Serialize)]
pub struct SyncEventsResult {
    pub last_seq: u64,
    pub events: Vec<JournalEvent>,
}

/// Connected daemon client. Commands are synchronized on the writer + pending
/// map. The single event receiver is owned by this struct (the GUI's event loop
/// drains it); share the client via `Arc<DaemonClient>`.
#[derive(Debug)]
pub struct DaemonClient {
    conn: Arc<ConnState>,
    events: Mutex<std::sync::mpsc::Receiver<ClientEvent>>,
    instance_id: String,
}

impl DaemonClient {
    /// Connect + handshake. Returns [`ClientError::NotRunning`] when no daemon
    /// is reachable (caller decides spawn/fallback), and a hard error for
    /// authentication/protocol failures.
    pub fn connect(base: &Path, app_version: &str) -> Result<Self, ClientError> {
        let token = read_token(base).map_err(|_| ClientError::NotRunning)?;
        let stream = UnixStream::connect(socket_path(base)).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ClientError::NotRunning,
            _ => ClientError::Io(e),
        })?;
        stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))?;

        // Handshake (§4.1).
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            app_version: app_version.to_string(),
            token,
        };
        let mut s = stream;
        write_frame(
            &mut s,
            FRAME_HELLO,
            0,
            &serde_json::to_vec(&hello).map_err(|e| ClientError::Handshake(e.to_string()))?,
        )?;
        let ack_frame = read_frame(&mut s).map_err(|e| ClientError::Io(io::Error::other(e)))?;
        if ack_frame.kind != FRAME_HELLO_ACK {
            let msg = serde_json::from_slice::<ProtocolErr>(&ack_frame.payload)
                .map(|p| p.message)
                .unwrap_or_else(|_| format!("expected HelloAck, got frame kind {}", ack_frame.kind));
            return Err(ClientError::Handshake(msg));
        }
        let ack: HelloAck = serde_json::from_slice(&ack_frame.payload)
            .map_err(|e| ClientError::Handshake(format!("bad HelloAck: {e}")))?;
        if ack.protocol_version != PROTOCOL_VERSION {
            return Err(ClientError::Handshake(format!(
                "daemon protocol {} != client protocol {PROTOCOL_VERSION}",
                ack.protocol_version
            )));
        }

        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let writer = s.try_clone()?;
        let conn = Arc::new(ConnState {
            writer: Mutex::new(writer),
            closed: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            events_tx: ev_tx,
        });

        // Reader thread: routes responses and events.
        let state = conn.clone();
        std::thread::Builder::new()
            .name("daemon-client-reader".into())
            .spawn(move || {
                reader_loop(s, state);
            })
            .expect("spawn daemon client reader");

        Ok(Self {
            conn,
            events: Mutex::new(ev_rx),
            instance_id: ack.daemon_instance_id,
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Block for the next daemon event (or a timeout). Returns the event, or
    /// [`std::sync::mpsc::RecvTimeoutError::Disconnected`] when the daemon's
    /// connection dropped. The GUI's event loop calls this to forward output and
    /// lifecycle events.
    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ClientEvent, std::sync::mpsc::RecvTimeoutError> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .recv_timeout(timeout)
    }

    fn request(&self, cmd: &RequestCmd) -> Result<Response, ClientError> {
        if self.conn.closed.load(Ordering::Acquire) {
            return Err(ClientError::Closed);
        }
        let rid = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.conn
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(rid, tx);
        let payload =
            serde_json::to_vec(cmd).map_err(|e| ClientError::Handshake(e.to_string()))?;
        if let Err(e) = write_frame(
            &mut *self.conn.writer.lock().unwrap_or_else(|p| p.into_inner()),
            FRAME_REQUEST,
            rid,
            &payload,
        ) {
            self.conn
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&rid);
            return Err(ClientError::Io(io::Error::other(e)));
        }
        match rx.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(resp) => Ok(resp),
            Err(_) => {
                self.conn
                    .pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&rid);
                Err(ClientError::Timeout)
            }
        }
    }

    fn request_ok(&self, cmd: &RequestCmd) -> Result<(), ClientError> {
        match self.request(cmd)? {
            Response::Ok => Ok(()),
            Response::Error { code, message } => Err(ClientError::Request { code, message }),
            other => Err(ClientError::Handshake(format!("unexpected response: {other:?}"))),
        }
    }

    pub fn spawn(
        &self,
        agent_id: &str,
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> Result<(u32, u64), ClientError> {
        let resp = self.request(&RequestCmd::Spawn {
            agent_id: agent_id.to_string(),
            program: program.to_string(),
            args: args.to_vec(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: env.to_vec(),
            rows,
            cols,
        })?;
        match resp {
            Response::Spawned {
                agent_id: _,
                pid,
                generation,
            } => Ok((pid, generation)),
            Response::Error { code, message } => Err(ClientError::Request { code, message }),
            other => Err(ClientError::Handshake(format!("unexpected response: {other:?}"))),
        }
    }

    pub fn write(&self, agent_id: &str, generation: u64, data: &str) -> Result<(), ClientError> {
        self.request_ok(&RequestCmd::Write {
            agent_id: agent_id.to_string(),
            generation,
            data: data.to_string(),
        })
    }

    pub fn resize(&self, agent_id: &str, generation: u64, rows: u16, cols: u16) -> Result<(), ClientError> {
        self.request_ok(&RequestCmd::Resize {
            agent_id: agent_id.to_string(),
            generation,
            rows,
            cols,
        })
    }

    /// Re-attach to a live agent's output stream (§4.2/§5). The daemon applies
    /// `rows`/`cols` to the PTY and VT parser BEFORE rendering the snapshot, so
    /// the checkpoint matches the client's terminal geometry. The caller applies
    /// `checkpoint` (if any) then `replay`, and forwards only live `Output`
    /// events with `seq > snapshot_seq`.
    pub fn attach(
        &self,
        agent_id: &str,
        generation: u64,
        rows: u16,
        cols: u16,
        after_seq: Option<u64>,
    ) -> Result<AttachResult, ClientError> {
        let resp = self.request(&RequestCmd::Attach {
            agent_id: agent_id.to_string(),
            generation,
            rows,
            cols,
            after_seq,
        })?;
        match resp {
            Response::Attached {
                snapshot_seq,
                checkpoint,
                replay,
            } => Ok(AttachResult {
                snapshot_seq,
                checkpoint,
                replay,
            }),
            Response::Error { code, message } => Err(ClientError::Request { code, message }),
            other => Err(ClientError::Handshake(format!("unexpected response: {other:?}"))),
        }
    }

    pub fn kill(&self, agent_id: &str, generation: Option<u64>) -> Result<(), ClientError> {
        self.request_ok(&RequestCmd::Kill {
            agent_id: agent_id.to_string(),
            generation,
        })
    }

    pub fn list(&self) -> Result<Vec<LiveSessionSummary>, ClientError> {
        match self.request(&RequestCmd::List)? {
            Response::Listed { sessions } => Ok(sessions),
            Response::Error { code, message } => Err(ClientError::Request { code, message }),
            other => Err(ClientError::Handshake(format!("unexpected response: {other:?}"))),
        }
    }

    /// Ask the daemon to shut down (Phase 2: the GUI closes the daemon it
    /// spawned on quit). The daemon kills its PTYs, so this returns before the
    /// process exits.
    pub fn shutdown(&self) -> Result<(), ClientError> {
        self.request_ok(&RequestCmd::Shutdown)
    }

    /// Detach from the daemon (Phase 4, §9.4): release every input lease and
    /// output subscription this client holds, leaving the daemon and its
    /// sessions running. The GUI calls this on exit instead of `shutdown`, so
    /// agents survive a GUI restart and the next launch re-attaches to the same
    /// `(daemon_instance_id, agent_id, generation, pid)`.
    pub fn detach(&self) -> Result<(), ClientError> {
        self.request_ok(&RequestCmd::Detach)
    }

    /// Pull every journaled lifecycle event with `seq > last_seq` plus the
    /// daemon journal's current watermark (Phase 4b, §6.2). The GUI passes its
    /// own high-water mark (what it already applied live), so offline events —
    /// natural exits, delete-mode removals, hook-status transitions — are
    /// replayed exactly once on reconnect.
    pub fn sync_events(&self, last_seq: u64) -> Result<SyncEventsResult, ClientError> {
        let resp = self.request(&RequestCmd::SyncEvents { last_seq })?;
        match resp {
            Response::EventLog { last_seq, events } => Ok(SyncEventsResult {
                last_seq,
                events,
            }),
            Response::Error { code, message } => Err(ClientError::Request { code, message }),
            other => Err(ClientError::Handshake(format!("unexpected response: {other:?}"))),
        }
    }
}

fn reader_loop(mut reader: UnixStream, state: Arc<ConnState>) {
    loop {
        let frame = match read_frame(&mut reader) {
            Ok(f) => f,
            Err(_) => break,
        };
        match frame.kind {
            FRAME_RESPONSE => {
                let sender = state
                    .pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&frame.request_id);
                if let Some(s) = sender {
                    let resp = serde_json::from_slice(&frame.payload)
                        .unwrap_or(Response::Error {
                            code: "bad_response".into(),
                            message: "malformed response from daemon".into(),
                        });
                    let _ = s.send(resp);
                }
            }
            FRAME_EVENT => {
                if let Ok(ev) = decode_event_payload(&frame.payload) {
                    let _ = state.events_tx.send(ev);
                }
            }
            _ => {} // handshake ERROR frames are handled in connect; ignore others
        }
    }
    state.closed.store(true, Ordering::Release);
    // Fail all pending requests so callers don't hang until their timeout.
    let pendings: Vec<_> = state
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .drain()
        .map(|(_, s)| s)
        .collect();
    for s in pendings {
        let _ = s.send(Response::Error {
            code: "closed".into(),
            message: "daemon disconnected".into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::{DaemonConfig, DaemonServer};
    use std::sync::atomic::AtomicU64;
    use std::time::{Duration, Instant};

    static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_base() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "capilot_daemon_client_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn connect_fails_cleanly_when_no_daemon() {
        let base = tmp_base();
        let err = DaemonClient::connect(&base, "test").unwrap_err();
        assert!(matches!(err, ClientError::NotRunning), "{err:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn spawn_write_list_kill_through_client() {
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
        let client = DaemonClient::connect(&base, "test").expect("connect");
        assert!(!client.instance_id().is_empty());

        let (pid, generation) = client
            .spawn(
                "c1",
                "/bin/sh",
                &["-c".into(), "echo __CLI__; sleep 30".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");
        assert!(pid > 0);
        assert!(generation >= 1);

        // Output events arrive via the client's event receiver.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got_banner = false;
        while Instant::now() < deadline {
            match client.recv_event_timeout(Duration::from_millis(500)) {
                Ok(ClientEvent::Output { agent_id, data, .. }) => {
                    assert_eq!(agent_id, "c1");
                    if String::from_utf8_lossy(&data).contains("__CLI__") {
                        got_banner = true;
                        break;
                    }
                }
                Ok(other) => panic!("unexpected {other:?}"),
                Err(_) => continue,
            }
        }
        assert!(got_banner, "did not receive the spawn banner");

        client.write("c1", generation, "x").expect("write");
        client
            .resize("c1", generation, 30, 100)
            .expect("resize");

        let sessions = client.list().expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_id, "c1");
        assert_eq!(sessions[0].generation, generation);

        // Stale generation write rejected.
        let err = client.write("c1", generation + 1, "x").unwrap_err();
        assert!(matches!(err, ClientError::Request { ref code, .. } if code == "stale_generation"));

        client.kill("c1", Some(generation)).expect("kill");
        assert!(client.list().expect("list").is_empty());

        let _ = client.shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn natural_exit_event_arrives_via_client() {
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
        let client = DaemonClient::connect(&base, "test").expect("connect");

        let (_pid, _g) = client
            .spawn(
                "fast",
                "/bin/sh",
                &["-c".into(), "exit 4".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_exit = false;
        while Instant::now() < deadline {
            match client.recv_event_timeout(Duration::from_millis(500)) {
                Ok(ClientEvent::Output { .. }) => continue,
                Ok(ClientEvent::Exited {
                    agent_id,
                    exit_code,
                    ..
                }) => {
                    assert_eq!(agent_id, "fast");
                    assert_eq!(exit_code, 4);
                    saw_exit = true;
                    break;
                }
                Ok(other) => panic!("unexpected {other:?}"),
                Err(_) => continue,
            }
        }
        assert!(saw_exit, "Exited event not received");

        let _ = client.shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn attach_returns_checkpoint_and_rejects_stale_generation() {
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
        let client = DaemonClient::connect(&base, "test").expect("connect");

        let (_pid, generation) = client
            .spawn(
                "c1",
                "/bin/sh",
                &["-c".into(), "echo __CLI__; sleep 30".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");

        // Consume the spawn banner via events so the hub has output to snapshot.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_banner = false;
        while Instant::now() < deadline {
            match client.recv_event_timeout(Duration::from_millis(500)) {
                Ok(ClientEvent::Output { data, .. }) => {
                    if String::from_utf8_lossy(&data).contains("__CLI__") {
                        saw_banner = true;
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert!(saw_banner, "spawn banner not received");

        // A fresh attach (no baseline) returns a full checkpoint.
        let result = client
            .attach("c1", generation, 24, 80, None)
            .expect("attach");
        assert!(result.checkpoint.is_some(), "fresh attach needs a checkpoint");
        assert!(result.replay.is_empty(), "no gap for a fresh attach");
        assert!(result.snapshot_seq >= 1);

        // The checkpoint reconstructs the banner screen.
        let mut p = vt100::Parser::new(24, 80, 200);
        p.process(&result.checkpoint.unwrap());
        assert!(
            p.screen().contents().contains("__CLI__"),
            "checkpoint must rebuild the banner: {:?}",
            p.screen().contents()
        );

        // A stale generation attach is rejected (liveness authority).
        let err = client
            .attach("c1", generation + 1, 24, 80, None)
            .unwrap_err();
        assert!(
            matches!(err, ClientError::Request { ref code, .. } if code == "stale_generation"),
            "{err:?}"
        );

        let _ = client.shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Detach through the client (§9.4): releases the GUI's lease while the
    /// daemon + session stay live, then a second client attach takes over.
    #[test]
    fn detach_releases_lease_then_second_client_takes_over() {
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

        let c1 = DaemonClient::connect(&base, "test").expect("connect");
        let (_pid, generation) = c1
            .spawn(
                "c1",
                "/bin/sh",
                &["-c".into(), "echo __CLI__; sleep 30".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");

        // c1 owns the lease; a second client's write is rejected.
        let c2 = DaemonClient::connect(&base, "test").expect("connect c2");
        let err = c2.write("c1", generation, "x").unwrap_err();
        assert!(
            matches!(err, ClientError::Request { ref code, .. } if code == "lease_held"),
            "foreign write before detach must be rejected: {err:?}"
        );

        // c1 detaches — the daemon stays up and c2 can take the lease.
        c1.detach().expect("detach");
        c2.attach("c1", generation, 24, 80, None)
            .expect("c2 attach after detach");
        c2.write("c1", generation, "ping\n").expect("write after take-over");

        let _ = c2.shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Offline replay through the client (§6.2): sync_events returns the
    /// journaled natural exit past the caller's watermark plus the new
    /// watermark; syncing again from the watermark yields nothing.
    #[test]
    fn sync_events_returns_replay_and_watermark() {
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
        let client = DaemonClient::connect(&base, "test").expect("connect");

        let (_pid, _g) = client
            .spawn(
                "fast",
                "/bin/sh",
                &["-c".into(), "exit 4".into()],
                &std::env::temp_dir(),
                &[],
                24,
                80,
            )
            .expect("spawn");

        // Capture the journaled Exited event_seq.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited_seq = None;
        while Instant::now() < deadline {
            match client.recv_event_timeout(Duration::from_millis(500)) {
                Ok(ClientEvent::Output { .. }) => continue,
                Ok(ClientEvent::Exited {
                    agent_id,
                    exit_code,
                    event_seq,
                    ..
                }) => {
                    assert_eq!(agent_id, "fast");
                    assert_eq!(exit_code, 4);
                    exited_seq = Some(event_seq);
                    break;
                }
                Ok(other) => panic!("unexpected {other:?}"),
                Err(_) => continue,
            }
        }
        let exited_seq = exited_seq.expect("Exited event not received");

        // Replay from 0 → the exit event + watermark.
        let r = client.sync_events(0).expect("sync_events");
        assert!(r.last_seq >= exited_seq);
        assert_eq!(r.events.len(), 1, "replay must include the exit: {:?}", r.events);
        assert_eq!(r.events[0].kind, "exited");
        assert_eq!(r.events[0].exit_code, Some(4));

        // Sync from the delivered watermark → nothing new.
        let r = client.sync_events(exited_seq).expect("sync_events");
        assert!(r.events.is_empty(), "no replay past the watermark: {:?}", r.events);

        let _ = client.shutdown();
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }
}