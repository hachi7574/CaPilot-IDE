//! Framed binary protocol between the GUI bridge and the PTY daemon (§4).
//!
//! Everything on the wire is a length-bounded frame:
//!
//! ```text
//! [u32_le body_len][u8 kind][u64_le request_id][payload]
//! ```
//!
//! `body_len` covers `kind + request_id + payload` and is capped at
//! [`MAX_FRAME_LEN`]; frames larger than that are a protocol violation, never
//! allocated. `request_id` is 0 for handshake / events / protocol errors and a
//! positive echo token for requests — the client routes responses back to the
//! awaiting caller by it.
//!
//! Payloads are JSON except the high-volume `Output` event, which uses a compact
//! binary layout so raw PTY bytes are not base64-bloated. Control messages stay
//! JSON for debuggability (a single protocol version, no versioned encoding).

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Current wire protocol version. Bump on any incompatible change; the daemon
/// and client must agree (brief §4.1 — an incompatible daemon is an upgrade
/// error, never a silent second PTY manager). v2 adds `Attach`/`Attached`
/// (§4.2, Phase 3); v3 adds `Detach`, `SyncEvents`/`EventLog` and the
/// `HookStatus` event (Phase 4) — an old client can't drive a new daemon's
/// detach/replay surface.
pub const PROTOCOL_VERSION: u32 = 3;

/// App identifier / capability advertised during the handshake.
pub const CAPABILITY_BASIC_IO: &str = "basic_io";

/// Capability: the daemon can rebuild a terminal screen on attach (§4.2/§5).
/// Same-binary GUI/daemon means it is always available at v2, but advertising
/// it keeps the handshake honest for a future mixed-version pairing.
pub const CAPABILITY_ATTACH: &str = "attach";

/// Capability: the daemon journals lifecycle events (natural exit, removal,
/// hook-status transitions) and can replay them to a (re)connecting GUI via
/// `SyncEvents` (§6.2/§9.4). Added at v3.
pub const CAPABILITY_EVENT_REPLAY: &str = "event_replay";

/// Upper bound for a single frame (16 MiB). Guards against an unbounded frame
/// exhausting daemon or client memory (§4.2). A single PTY read chunk is
/// ~64 KiB, so this is far above any legitimate message.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

// Frame kinds.
pub const FRAME_HELLO: u8 = 1;
pub const FRAME_HELLO_ACK: u8 = 2;
pub const FRAME_REQUEST: u8 = 3;
pub const FRAME_RESPONSE: u8 = 4;
pub const FRAME_EVENT: u8 = 5;
pub const FRAME_ERROR: u8 = 6;

// Event kinds (only meaningful inside a FRAME_EVENT payload).
pub const EVENT_OUTPUT: u8 = 1;
pub const EVENT_EXITED: u8 = 2;
pub const EVENT_REMOVED: u8 = 3;
pub const EVENT_HOOK_STATUS: u8 = 4;

/// Handshake frame sent by the client (GUI) first (§4.1). The token is read
/// from a user-only runtime file the daemon wrote at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub app_version: String,
    pub token: String,
}

/// Reply to a valid [`Hello`]. `daemon_instance_id` lets the GUI reconcile
/// `(daemon_instance_id, agent_id, generation)` after a reconnect (§6.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub daemon_instance_id: String,
    pub capabilities: Vec<String>,
}

/// Protocol-level error (handshake rejection, malformed frame, oversize frame).
/// The connection is closed after sending one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolErr {
    pub code: String,
    pub message: String,
}

/// Control commands from the GUI bridge (§4.2). `request_id` lives in the frame
/// header, so this enum is purely the command surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum RequestCmd {
    /// Spawn a fresh PTY for an agent. The daemon owns the process from here on.
    Spawn {
        agent_id: String,
        program: String,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
        rows: u16,
        cols: u16,
    },
    /// Write bytes to an agent's PTY master. `generation` guards against a stale
    /// client writing into a respawned process.
    Write {
        agent_id: String,
        generation: u64,
        data: String,
    },
    Resize {
        agent_id: String,
        generation: u64,
        rows: u16,
        cols: u16,
    },
    /// Kill an agent's PTY. With `generation` set, only kills when it still
    /// matches (the caller aimed at a specific incarnation).
    Kill {
        agent_id: String,
        generation: Option<u64>,
    },
    /// Re-attach a client to a live agent's terminal (§4.2/§5). The daemon
    /// renders a checkpoint (or the gap bytes since `after_seq`) and streams
    /// only `seq > snapshot_seq` to the new subscriber. `initial_size` is
    /// applied to the PTY and the checkpoint BEFORE the snapshot. The attaching
    /// client takes the agent's input control lease (only the lease holder may
    /// `Write`).
    Attach {
        agent_id: String,
        generation: u64,
        rows: u16,
        cols: u16,
        after_seq: Option<u64>,
    },
    /// List live sessions (agent_id, pid, generation, last_seq).
    List,
    /// Detach the calling client from the daemon (§9.4, Phase 4): release every
    /// input lease it holds and unsubscribe its output subscribers. The daemon
    /// and its sessions keep running — the GUI calls this on exit instead of
    /// `Shutdown`, so agents survive a GUI restart.
    Detach,
    /// Replay journaled lifecycle events with `seq > last_seq` (natural exits,
    /// removals, hook-status transitions that happened while the GUI was
    /// offline, §6.2/§9.4). The client applies the returned events, then tracks
    /// live lifecycle events by their `event_seq` and skips already-seen ones.
    SyncEvents {
        last_seq: u64,
    },
    /// Graceful daemon shutdown (Phase 2: the GUI explicitly closes the daemon
    /// it spawned on quit — §9.2; Phase 4 only for an explicit stop).
    Shutdown,
}

/// Reply to a [`RequestCmd`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Spawned {
        agent_id: String,
        pid: u32,
        generation: u64,
    },
    /// Reply to `Attach`: the terminal bytes the client must apply before live
    /// events. When `checkpoint` is `Some`, the client's terminal is fresh and
    /// must reset + apply it; when `None`, `replay` covers the gap from the
    /// client's `after_seq`. Live `Output` events carry only `seq > snapshot_seq`.
    /// The raw bytes are serialized as JSON arrays (a one-shot snapshot, far
    /// under the 16 MiB frame bound, so the compact OUTPUT codec is not reused).
    Attached {
        snapshot_seq: u64,
        checkpoint: Option<Vec<u8>>,
        replay: Vec<u8>,
    },
    Listed {
        sessions: Vec<LiveSessionSummary>,
    },
    /// Reply to `SyncEvents`: the journaled lifecycle events with `seq >
    /// last_seq` plus the journal's current high-water mark. The client applies
    /// the events in order, remembers `last_seq`, and dedupes live lifecycle
    /// events by `event_seq`.
    EventLog {
        last_seq: u64,
        events: Vec<JournalEvent>,
    },
    /// A command-level failure. (Protocol-level failures use the ERROR frame.)
    Error {
        code: String,
        message: String,
    },
}

/// One live session in a `List` reply — the daemon's authority on "is live".
/// `status = running` in the DB can't prove liveness (§6.2); this can.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSessionSummary {
    pub agent_id: String,
    pub pid: u32,
    pub generation: u64,
    pub last_seq: u64,
}

/// One journaled lifecycle event carried by a `SyncEvents` reply (§6.2). A flat,
/// wire-friendly shape the GUI can re-apply without importing the shared store.
/// `kind` is `"exited" | "removed" | "hook_status"`; the kind-specific field
/// (`exit_code` / `status`) is present only for the matching kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    /// 1-based monotonic sequence (global across agents, within a daemon run).
    pub seq: u64,
    pub ts: i64,
    pub agent_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Events the daemon pushes to clients.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Raw PTY bytes for an agent (`seq` is the per-agent output sequence).
    Output {
        agent_id: String,
        generation: u64,
        seq: u64,
        data: Vec<u8>,
    },
    /// Natural exit (kept by `session_end_mode` → status `done`).
    Exited {
        agent_id: String,
        generation: u64,
        exit_code: i32,
        event_seq: u64,
    },
    /// Natural exit in delete mode (row + agent dir removed), or explicit
    /// removal acknowledged by the daemon.
    Removed {
        agent_id: String,
        generation: u64,
        event_seq: u64,
    },
    /// A status-hook sidecar change the daemon observed (`~/CaPilot/status/
    /// <id>.json`). Recorded in the journal so transitions that happen while the
    /// GUI is offline are replayable; broadcast live while it is connected.
    HookStatus {
        agent_id: String,
        status: String,
        ts: i64,
        event_seq: u64,
    },
}

impl ClientEvent {
    pub fn agent_id(&self) -> &str {
        match self {
            ClientEvent::Output { agent_id, .. }
            | ClientEvent::Exited { agent_id, .. }
            | ClientEvent::Removed { agent_id, .. }
            | ClientEvent::HookStatus { agent_id, .. } => agent_id,
        }
    }
}

/// A decoded frame.
#[derive(Debug)]
pub struct Frame {
    pub kind: u8,
    pub request_id: u64,
    pub payload: Vec<u8>,
}

/// Protocol-layer errors. `Io` is a transport failure; the rest are violations
/// that must close the connection.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("frame too large: {0} bytes (max {MAX_FRAME_LEN})")]
    FrameTooLarge(usize),
    #[error("malformed frame body: {0}")]
    Malformed(String),
}

/// Transport callers (server, client) mostly want `io::Error`; the two
/// non-I/O variants are still protocol violations, so surface them as
/// `io::Error::other` rather than losing the failure.
impl From<ProtocolError> for io::Error {
    fn from(e: ProtocolError) -> Self {
        io::Error::other(e)
    }
}

/// Encode one frame. Rejects payloads that would exceed [`MAX_FRAME_LEN`].
pub fn encode_frame(kind: u8, request_id: u64, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let body_len = 1 + 8 + payload.len();
    if body_len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(body_len));
    }
    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&(body_len as u32).to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&request_id.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Read exactly one frame from `reader`.
pub fn read_frame(reader: &mut impl Read) -> Result<Frame, ProtocolError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let body_len = u32::from_le_bytes(len_buf) as usize;
    if body_len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(body_len));
    }
    if body_len < 9 {
        return Err(ProtocolError::Malformed(format!(
            "body_len {body_len} < kind+request_id"
        )));
    }
    let mut body = vec![0u8; body_len];
    reader.read_exact(&mut body)?;
    let kind = body[0];
    let request_id = u64::from_le_bytes(body[1..9].try_into().expect("9 bytes"));
    Ok(Frame {
        kind,
        request_id,
        payload: body[9..].to_vec(),
    })
}

/// Encode and write one frame, flushing afterward.
pub fn write_frame(
    writer: &mut impl Write,
    kind: u8,
    request_id: u64,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    let frame = encode_frame(kind, request_id, payload)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

// ── Event payload codecs ────────────────────────────────────────────

/// Encode an OUTPUT event payload (binary — raw bytes, not base64).
pub fn encode_output_payload(
    agent_id: &str,
    generation: u64,
    seq: u64,
    data: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    let id = agent_id.as_bytes();
    let total = 4 + id.len() + 8 + 8 + 4 + data.len();
    if 1 + total > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(1 + total));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(id.len() as u32).to_le_bytes());
    out.extend_from_slice(id);
    out.extend_from_slice(&generation.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

/// Decode an OUTPUT event payload.
pub fn decode_output_payload(payload: &[u8]) -> Result<(String, u64, u64, Vec<u8>), ProtocolError> {
    let mut p = payload;
    let id_len = read_u32(&mut p)? as usize;
    if id_len > p.len() {
        return Err(ProtocolError::Malformed("agent_id len".into()));
    }
    let id = String::from_utf8(p[..id_len].to_vec())
        .map_err(|_| ProtocolError::Malformed("agent_id not utf-8".into()))?;
    p = &p[id_len..];
    let generation = read_u64(&mut p)?;
    let seq = read_u64(&mut p)?;
    let data_len = read_u32(&mut p)? as usize;
    if data_len > p.len() {
        return Err(ProtocolError::Malformed("data len".into()));
    }
    let data = p[..data_len].to_vec();
    Ok((id, generation, seq, data))
}

fn read_u32(p: &mut &[u8]) -> Result<u32, ProtocolError> {
    if p.len() < 4 {
        return Err(ProtocolError::Malformed("short u32".into()));
    }
    let v = u32::from_le_bytes(p[..4].try_into().unwrap());
    *p = &p[4..];
    Ok(v)
}

fn read_u64(p: &mut &[u8]) -> Result<u64, ProtocolError> {
    if p.len() < 8 {
        return Err(ProtocolError::Malformed("short u64".into()));
    }
    let v = u64::from_le_bytes(p[..8].try_into().unwrap());
    *p = &p[8..];
    Ok(v)
}

/// Encode an EXITED event payload (JSON).
pub fn encode_exited_payload(
    agent_id: &str,
    generation: u64,
    exit_code: i32,
    event_seq: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "agent_id": agent_id,
        "generation": generation,
        "exit_code": exit_code,
        "event_seq": event_seq,
    }))
    .expect("exited payload serializes")
}

/// Encode a REMOVED event payload (JSON).
pub fn encode_removed_payload(agent_id: &str, generation: u64, event_seq: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "agent_id": agent_id,
        "generation": generation,
        "event_seq": event_seq,
    }))
    .expect("removed payload serializes")
}

/// Encode a HOOK_STATUS event payload (JSON).
pub fn encode_hook_status_payload(
    agent_id: &str,
    status: &str,
    ts: i64,
    event_seq: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "agent_id": agent_id,
        "status": status,
        "ts": ts,
        "event_seq": event_seq,
    }))
    .expect("hook_status payload serializes")
}

/// Encode a full EVENT frame payload: `[u8 event_kind][event_payload]`.
pub fn encode_event_payload(event: &ClientEvent) -> Result<Vec<u8>, ProtocolError> {
    match event {
        ClientEvent::Output {
            agent_id,
            generation,
            seq,
            data,
        } => {
            let mut out = vec![EVENT_OUTPUT];
            out.extend_from_slice(&encode_output_payload(
                agent_id,
                *generation,
                *seq,
                data,
            )?);
            Ok(out)
        }
        ClientEvent::Exited {
            agent_id,
            generation,
            exit_code,
            event_seq,
        } => {
            let mut out = vec![EVENT_EXITED];
            out.extend_from_slice(&encode_exited_payload(
                agent_id,
                *generation,
                *exit_code,
                *event_seq,
            ));
            Ok(out)
        }
        ClientEvent::Removed {
            agent_id,
            generation,
            event_seq,
        } => {
            let mut out = vec![EVENT_REMOVED];
            out.extend_from_slice(&encode_removed_payload(
                agent_id,
                *generation,
                *event_seq,
            ));
            Ok(out)
        }
        ClientEvent::HookStatus {
            agent_id,
            status,
            ts,
            event_seq,
        } => {
            let mut out = vec![EVENT_HOOK_STATUS];
            out.extend_from_slice(&encode_hook_status_payload(
                agent_id, status, *ts, *event_seq,
            ));
            Ok(out)
        }
    }
}

/// Decode a full EVENT frame payload.
pub fn decode_event_payload(payload: &[u8]) -> Result<ClientEvent, ProtocolError> {
    if payload.is_empty() {
        return Err(ProtocolError::Malformed("empty event payload".into()));
    }
    match payload[0] {
        EVENT_OUTPUT => {
            let (agent_id, generation, seq, data) = decode_output_payload(&payload[1..])?;
            Ok(ClientEvent::Output {
                agent_id,
                generation,
                seq,
                data,
            })
        }
        EVENT_EXITED => {
            let v: serde_json::Value = serde_json::from_slice(&payload[1..])
                .map_err(|e| ProtocolError::Malformed(format!("exited json: {e}")))?;
            Ok(ClientEvent::Exited {
                agent_id: v["agent_id"]
                    .as_str()
                    .ok_or_else(|| ProtocolError::Malformed("exited agent_id".into()))?
                    .to_string(),
                generation: v["generation"].as_u64().unwrap_or(0),
                exit_code: v["exit_code"].as_i64().unwrap_or(0) as i32,
                event_seq: v["event_seq"].as_u64().unwrap_or(0),
            })
        }
        EVENT_REMOVED => {
            let v: serde_json::Value = serde_json::from_slice(&payload[1..])
                .map_err(|e| ProtocolError::Malformed(format!("removed json: {e}")))?;
            Ok(ClientEvent::Removed {
                agent_id: v["agent_id"]
                    .as_str()
                    .ok_or_else(|| ProtocolError::Malformed("removed agent_id".into()))?
                    .to_string(),
                generation: v["generation"].as_u64().unwrap_or(0),
                event_seq: v["event_seq"].as_u64().unwrap_or(0),
            })
        }
        EVENT_HOOK_STATUS => {
            let v: serde_json::Value = serde_json::from_slice(&payload[1..])
                .map_err(|e| ProtocolError::Malformed(format!("hook_status json: {e}")))?;
            Ok(ClientEvent::HookStatus {
                agent_id: v["agent_id"]
                    .as_str()
                    .ok_or_else(|| ProtocolError::Malformed("hook_status agent_id".into()))?
                    .to_string(),
                status: v["status"]
                    .as_str()
                    .ok_or_else(|| ProtocolError::Malformed("hook_status status".into()))?
                    .to_string(),
                ts: v["ts"].as_i64().unwrap_or(0),
                event_seq: v["event_seq"].as_u64().unwrap_or(0),
            })
        }
        other => Err(ProtocolError::Malformed(format!(
            "unknown event kind {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_preserves_kind_request_id_payload() {
        let mut buf = Vec::new();
        let payload = b"\x00\x01hello\xff".to_vec();
        write_frame(&mut buf, FRAME_REQUEST, 42, &payload).unwrap();

        let mut cursor = io::Cursor::new(&buf);
        let frame = read_frame(&mut cursor).unwrap();
        assert_eq!(frame.kind, FRAME_REQUEST);
        assert_eq!(frame.request_id, 42);
        assert_eq!(frame.payload, payload);
    }

    #[test]
    fn hello_and_ack_serialize() {
        let hello = Hello {
            protocol_version: 1,
            app_version: "0.1.0".into(),
            token: "tok".into(),
        };
        let json = serde_json::to_vec(&hello).unwrap();
        let back: Hello = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.protocol_version, 1);
        assert_eq!(back.token, "tok");

        let ack = HelloAck {
            protocol_version: 1,
            daemon_instance_id: "d1".into(),
            capabilities: vec![CAPABILITY_BASIC_IO.into()],
        };
        let back: HelloAck = serde_json::from_slice(&serde_json::to_vec(&ack).unwrap()).unwrap();
        assert_eq!(back.daemon_instance_id, "d1");
        assert_eq!(back.capabilities, vec![CAPABILITY_BASIC_IO]);
    }

    #[test]
    fn request_response_tagged_enum_roundtrip() {
        let req = RequestCmd::Write {
            agent_id: "a1".into(),
            generation: 3,
            data: "ls\r".into(),
        };
        let json = serde_json::to_vec(&req).unwrap();
        let back: RequestCmd = serde_json::from_slice(&json).unwrap();
        match back {
            RequestCmd::Write {
                agent_id,
                generation,
                data,
            } => {
                assert_eq!(agent_id, "a1");
                assert_eq!(generation, 3);
                assert_eq!(data, "ls\r");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        let resp = Response::Listed {
            sessions: vec![LiveSessionSummary {
                agent_id: "a1".into(),
                pid: 1234,
                generation: 3,
                last_seq: 99,
            }],
        };
        let back: Response = serde_json::from_slice(&serde_json::to_vec(&resp).unwrap()).unwrap();
        match back {
            Response::Listed { sessions } => assert_eq!(sessions[0].pid, 1234),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn attach_request_and_attached_response_roundtrip() {
        // Attach request (JSON).
        let req = RequestCmd::Attach {
            agent_id: "a1".into(),
            generation: 3,
            rows: 30,
            cols: 100,
            after_seq: Some(41),
        };
        let back: RequestCmd =
            serde_json::from_slice(&serde_json::to_vec(&req).unwrap()).unwrap();
        match back {
            RequestCmd::Attach {
                agent_id,
                generation,
                rows,
                cols,
                after_seq,
            } => {
                assert_eq!(agent_id, "a1");
                assert_eq!(generation, 3);
                assert_eq!((rows, cols), (30, 100));
                assert_eq!(after_seq, Some(41));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // Attached response carries raw terminal bytes (JSON array-of-bytes).
        let ckpt = b"\x1b[?1049h\x1b[0m\x1b[2J\x1b[H\x1b[32mhi".to_vec();
        let resp = Response::Attached {
            snapshot_seq: 41,
            checkpoint: Some(ckpt.clone()),
            replay: vec![b'x', b'y'],
        };
        let back: Response = serde_json::from_slice(&serde_json::to_vec(&resp).unwrap()).unwrap();
        match back {
            Response::Attached {
                snapshot_seq,
                checkpoint,
                replay,
            } => {
                assert_eq!(snapshot_seq, 41);
                assert_eq!(checkpoint, Some(ckpt));
                assert_eq!(replay, b"xy");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn output_event_binary_roundtrip_with_raw_bytes() {
        let data = b"\x1b[31mred\x00\xff\xfe".to_vec();
        let payload = encode_output_payload("agent-x", 7, 1001, &data).unwrap();
        let (id, gen, seq, out) = decode_output_payload(&payload).unwrap();
        assert_eq!(id, "agent-x");
        assert_eq!(gen, 7);
        assert_eq!(seq, 1001);
        assert_eq!(out, data);
    }

    #[test]
    fn event_envelope_roundtrip_for_all_kinds() {
        for ev in [
            ClientEvent::Output {
                agent_id: "a".into(),
                generation: 1,
                seq: 1,
                data: vec![1, 2, 3],
            },
            ClientEvent::Exited {
                agent_id: "a".into(),
                generation: 1,
                exit_code: 3,
                event_seq: 9,
            },
            ClientEvent::Removed {
                agent_id: "b".into(),
                generation: 2,
                event_seq: 10,
            },
            ClientEvent::HookStatus {
                agent_id: "c".into(),
                status: "working".into(),
                ts: 1_700_000_000,
                event_seq: 11,
            },
        ] {
            let payload = encode_event_payload(&ev).unwrap();
            let back = decode_event_payload(&payload).unwrap();
            match (&ev, &back) {
                (ClientEvent::Output { data, .. }, ClientEvent::Output { data: d2, .. }) => {
                    assert_eq!(data, d2);
                }
                (
                    ClientEvent::Exited {
                        exit_code, event_seq, ..
                    },
                    ClientEvent::Exited {
                        exit_code: e2,
                        event_seq: s2,
                        ..
                    },
                ) => {
                    assert_eq!(exit_code, e2);
                    assert_eq!(event_seq, s2);
                }
                (
                    ClientEvent::Removed { event_seq, .. },
                    ClientEvent::Removed {
                        event_seq: s2, ..
                    },
                ) => assert_eq!(event_seq, s2),
                (
                    ClientEvent::HookStatus {
                        status, ts, event_seq, ..
                    },
                    ClientEvent::HookStatus {
                        status: st2,
                        ts: t2,
                        event_seq: s2,
                        ..
                    },
                ) => {
                    assert_eq!(status, st2);
                    assert_eq!(ts, t2);
                    assert_eq!(event_seq, s2);
                }
                (a, b) => panic!("kind mismatch: {a:?} vs {b:?}"),
            }
        }
    }

    #[test]
    fn detach_sync_events_and_event_log_roundtrip() {
        // Detach (unit variant) serializes.
        let req = RequestCmd::Detach;
        let back: RequestCmd = serde_json::from_slice(&serde_json::to_vec(&req).unwrap()).unwrap();
        assert!(matches!(back, RequestCmd::Detach));

        // SyncEvents carries the client's high-water mark.
        let req = RequestCmd::SyncEvents { last_seq: 7 };
        let back: RequestCmd = serde_json::from_slice(&serde_json::to_vec(&req).unwrap()).unwrap();
        match back {
            RequestCmd::SyncEvents { last_seq } => assert_eq!(last_seq, 7),
            other => panic!("wrong variant: {other:?}"),
        }

        // EventLog carries the watermark + journal events (flat, kind-tagged).
        let resp = Response::EventLog {
            last_seq: 5,
            events: vec![
                JournalEvent {
                    seq: 4,
                    ts: 100,
                    agent_id: "a".into(),
                    kind: "exited".into(),
                    exit_code: Some(3),
                    status: None,
                },
                JournalEvent {
                    seq: 5,
                    ts: 101,
                    agent_id: "b".into(),
                    kind: "hook_status".into(),
                    exit_code: None,
                    status: Some("working".into()),
                },
            ],
        };
        let back: Response = serde_json::from_slice(&serde_json::to_vec(&resp).unwrap()).unwrap();
        match back {
            Response::EventLog { last_seq, events } => {
                assert_eq!(last_seq, 5);
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].kind, "exited");
                assert_eq!(events[0].exit_code, Some(3));
                assert_eq!(events[1].kind, "hook_status");
                assert_eq!(events[1].status.as_deref(), Some("working"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn oversized_frame_is_rejected_not_allocated() {
        // Payload that pushes the body over MAX_FRAME_LEN.
        let big = vec![0u8; MAX_FRAME_LEN];
        assert!(matches!(
            encode_frame(FRAME_REQUEST, 1, &big),
            Err(ProtocolError::FrameTooLarge(_))
        ));

        // A bogus length header that claims more than MAX_FRAME_LEN must be
        // rejected before reading the body.
        let mut buf = Vec::new();
        buf.extend_from_slice(&((MAX_FRAME_LEN as u32) + 1).to_le_bytes());
        buf.extend_from_slice(&[FRAME_REQUEST]);
        let mut cursor = io::Cursor::new(buf);
        assert!(matches!(
            read_frame(&mut cursor),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn malformed_short_frame_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_le_bytes()); // body_len 5 < kind+request_id (9)
        buf.extend_from_slice(&[FRAME_REQUEST, 0, 0, 0, 0]);
        let mut cursor = io::Cursor::new(buf);
        assert!(matches!(
            read_frame(&mut cursor),
            Err(ProtocolError::Malformed(_))
        ));
    }
}
