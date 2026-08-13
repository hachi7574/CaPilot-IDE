//! OutputHub (§2.2) — the boundary that takes `pty_core` output, tags it with a
//! per-agent sequence number, maintains a recoverable terminal checkpoint, and
//! fans output out to subscribers.
//!
//! The rule that "a subscriber failure removes only that subscriber, never the
//! PTY" lives here and is tested now, so the daemon (Phase 2) cannot regress it
//! when it wires real IPC subscribers. In-process fallback mode does not use the
//! hub (the GUI bridge passes a `ChannelSink` straight into `pty_core`); the
//! daemon constructs one `AgentOutputHub` per agent and hands it to `pty_core`
//! as the sink.
//!
//! Phase 3 (attach) adds the two pieces §5 requires:
//!
//! - a `vt100::Parser` that consumes every chunk, so [`AgentOutputHub::attach`]
//!   can rebuild the current screen at a complete parse boundary — raw ring
//!   buffers cannot, because a truncated stream can start mid-UTF-8 / mid-CSI
//!   and depend on earlier screen state;
//! - a bounded increment log, so a reconnecting client whose own screen is
//!   current through `after_seq` gets only the gap as raw bytes instead of a
//!   full redraw.
//!
//! [`AgentOutputHub::attach`] is the atomic snapshot+subscribe (§4.2): under one
//! lock it reads the sequence, renders the checkpoint (or collects the gap
//! bytes), registers the subscriber, and returns both — so no byte is lost or
//! duplicated across the attach window. The subscriber then receives only chunks
//! with `seq > snapshot_seq`.

use crate::agent_runtime::pty_core::{OutputSink, SinkResult};
use crate::daemon::vt_checkpoint::render_checkpoint;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use vt100::Parser;

/// Bound for the raw increment log. A client re-attaching with `after_seq` needs
/// the gap bytes since that sequence; once the log overflows the oldest chunks
/// are dropped and a full checkpoint (which covers ALL output via the parser) is
/// served instead (§5 "超限时生成新 checkpoint 后才能丢弃旧增量").
pub const MAX_INCREMENT_BYTES: usize = 512 * 1024;

/// How many scrollback rows the per-session VT parser keeps. Scrollback is
/// maintained so the parser's state is faithful, but the checkpoint serializes
/// only the visible screen (matching tmux/screen attach behavior); a modest
/// bound keeps per-session memory capped (§5 global/session caps).
const SCROLLBACK_LEN: usize = 200;

/// A sequence-numbered output chunk delivered to hub subscribers.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub agent_id: String,
    /// 1-based per-agent sequence.
    pub seq: u64,
    pub data: Vec<u8>,
}

/// Subscriber to an agent's output stream. Returning `Err` detaches THIS
/// subscriber only — it never signals `pty_core` to stop or kill the child.
pub trait HubSubscriber: Send + Sync + 'static {
    fn on_output(&self, chunk: OutputChunk) -> SinkResult;
}

/// Result of an atomic snapshot+subscribe (§4.2). The caller applies the
/// checkpoint (if any) to its terminal, then the gap `replay` bytes, then
/// forwards live `Output` events with `seq > snapshot_seq`.
pub struct AttachSnapshot {
    /// The sequence the parser is current through. Live increments begin after
    /// this value.
    pub snapshot_seq: u64,
    /// Full terminal reconstruction (buffer switch + clear + active screen +
    /// cursor) to apply to a reset client. `None` when `replay` covers the gap
    /// from the client's `after_seq`.
    pub checkpoint: Option<Vec<u8>>,
    /// Raw bytes to feed after the checkpoint to bring the client current
    /// through `snapshot_seq` (empty for a full checkpoint).
    pub replay: Vec<u8>,
}

/// Everything guarded by the hub's single mutex so attach and output delivery
/// cannot interleave (§4.2 atomicity).
struct HubState {
    subscribers: Vec<std::sync::Arc<dyn HubSubscriber>>,
    parser: Parser,
    /// Contiguous suffix of output chunks (oldest → newest), bounded by
    /// [`MAX_INCREMENT_BYTES`]. Only ever trimmed from the front.
    log: VecDeque<OutputChunk>,
    log_bytes: usize,
}

/// Per-agent output hub. Implements [`OutputSink`] so it can be handed to
/// `pty_core` as the sink for one agent.
pub struct AgentOutputHub {
    agent_id: String,
    seq: AtomicU64,
    state: Mutex<HubState>,
}

impl AgentOutputHub {
    pub fn new(agent_id: &str, rows: u16, cols: u16) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            seq: AtomicU64::new(0),
            state: Mutex::new(HubState {
                subscribers: Vec::new(),
                parser: Parser::new(rows, cols, SCROLLBACK_LEN),
                log: VecDeque::new(),
                log_bytes: 0,
            }),
        }
    }

    /// Attach a subscriber that wants ALL output from the very beginning —
    /// used right after spawn, where the client's terminal is fresh and the
    /// retained log (plus live chunks) is the correct stream. If the log was
    /// trimmed before a subscriber ever attached, the earliest bytes are lost;
    /// the spawn→subscribe window is short and far under [`MAX_INCREMENT_BYTES`].
    ///
    /// The caller should instead use [`AgentOutputHub::attach`] when the client
    /// may already have terminal state.
    pub fn subscribe_from_beginning(&self, sub: std::sync::Arc<dyn HubSubscriber>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for chunk in state.log.iter().cloned() {
            if sub.on_output(chunk).is_err() {
                return; // already failing → don't register
            }
        }
        state.subscribers.push(sub);
    }

    /// Atomic snapshot+subscribe (§4.2 / §5).
    ///
    /// Under one lock: reads the current sequence, renders the checkpoint (or
    /// collects the gap bytes when `after_seq` is satisfiable from the log),
    /// registers the subscriber. Returns the snapshot the caller must apply;
    /// the subscriber then receives only chunks with `seq > snapshot_seq`.
    pub fn attach(
        &self,
        sub: std::sync::Arc<dyn HubSubscriber>,
        after_seq: Option<u64>,
    ) -> AttachSnapshot {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let snapshot_seq = self.seq.load(Ordering::Relaxed);
        let (checkpoint, replay) = match after_seq {
            // No baseline: full reconstruction.
            None => (Some(render_checkpoint(&state.parser)), Vec::new()),
            Some(a) if a > snapshot_seq => (None, Vec::new()), // client already ahead
            Some(a) => {
                // The log is a contiguous suffix [first.seq .. snapshot_seq].
                // If it still reaches `a + 1`, every gap byte is present.
                let contiguous = state
                    .log
                    .front()
                    .map(|f| f.seq <= a + 1)
                    .unwrap_or(a == snapshot_seq);
                if contiguous {
                    let mut replay = Vec::new();
                    for chunk in state.log.iter() {
                        if chunk.seq > a {
                            replay.extend_from_slice(&chunk.data);
                        }
                    }
                    (None, replay)
                } else {
                    // The client is too far behind to be patched — full rebuild.
                    (Some(render_checkpoint(&state.parser)), Vec::new())
                }
            }
        };
        state.subscribers.push(sub);
        AttachSnapshot {
            snapshot_seq,
            checkpoint,
            replay,
        }
    }

    pub fn detach_all(&self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .subscribers
            .clear();
    }

    /// Keep the VT parser's size in sync with the PTY, so a checkpoint rendered
    /// after a resize reflects the new geometry (§5 — attach's `initial_size`
    /// is applied before the snapshot is generated).
    pub fn resize(&self, rows: u16, cols: u16) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .parser
            .screen_mut()
            .set_size(rows, cols);
    }

    /// Highest sequence delivered so far (0 when nothing has been sent).
    pub fn last_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Bytes retained in the increment log (diagnostics/tests).
    #[cfg(test)]
    fn log_bytes(&self) -> usize {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).log_bytes
    }
}

impl OutputSink for AgentOutputHub {
    fn send(&self, data: Vec<u8>) -> SinkResult {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let chunk = OutputChunk {
            agent_id: self.agent_id.clone(),
            seq,
            data,
        };
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // Feed the VT parser so an attach can rebuild the current screen at a
        // complete parse boundary (§5).
        state.parser.process(&chunk.data);
        // Maintain the bounded increment log for the after_seq fast path.
        state.log.push_back(chunk.clone());
        state.log_bytes += chunk.data.len();
        while state.log_bytes > MAX_INCREMENT_BYTES && state.log.len() > 1 {
            state.log_bytes -= state.log.pop_front().map(|c| c.data.len()).unwrap_or(0);
        }
        // retain() both delivers and drops failing subscribers in one pass.
        // A failing subscriber is removed; the hub keeps absorbing output, so a
        // client disconnect can never cascade into pty_core killing the child
        // (§2.2).
        state.subscribers.retain(|s| s.on_output(chunk.clone()).is_ok());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::vt_checkpoint::render_checkpoint;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// Collects delivered chunks (seqs) into a shared Vec.
    #[derive(Default)]
    struct Recorder {
        seqs: Mutex<Vec<u64>>,
        total: AtomicUsize,
    }
    impl Recorder {
        fn subscriber(self: &Arc<Self>) -> Arc<RecorderSub> {
            Arc::new(RecorderSub { rec: self.clone() })
        }
        fn seqs(&self) -> Vec<u64> {
            self.seqs.lock().unwrap().clone()
        }
        fn count(&self) -> usize {
            self.total.load(Ordering::Relaxed)
        }
    }
    struct RecorderSub {
        rec: Arc<Recorder>,
    }
    impl HubSubscriber for RecorderSub {
        fn on_output(&self, chunk: OutputChunk) -> SinkResult {
            self.rec.seqs.lock().unwrap().push(chunk.seq);
            self.rec.total.fetch_add(chunk.data.len(), Ordering::Relaxed);
            Ok(())
        }
    }

    /// Fails on the Nth delivery (1-based), then stays failing.
    struct FailAfter {
        fail_on: u64,
        seen: AtomicU64,
    }
    impl HubSubscriber for FailAfter {
        fn on_output(&self, _chunk: OutputChunk) -> SinkResult {
            let n = self.seen.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.fail_on {
                Err(crate::agent_runtime::pty_core::SinkError::Closed)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn sequences_are_monotonic_and_fan_out_to_all_subscribers() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        let r1 = Arc::new(Recorder::default());
        let r2 = Arc::new(Recorder::default());
        hub.subscribe_from_beginning(r1.subscriber());
        hub.subscribe_from_beginning(r2.subscriber());

        hub.send(b"x".to_vec()).unwrap();
        hub.send(b"yz".to_vec()).unwrap();
        hub.send(b"".to_vec()).unwrap();

        assert_eq!(hub.last_seq(), 3);
        assert_eq!(r1.seqs(), vec![1, 2, 3]);
        assert_eq!(r2.seqs(), vec![1, 2, 3]);
        // Both got all 3 bytes total.
        assert_eq!(r1.count(), 3);
        assert_eq!(r2.count(), 3);
    }

    #[test]
    fn failing_subscriber_is_removed_not_the_hub() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        let good = Arc::new(Recorder::default());
        let bad = Arc::new(FailAfter {
            fail_on: 2,
            seen: AtomicU64::new(0),
        });
        hub.subscribe_from_beginning(good.subscriber());
        hub.subscribe_from_beginning(bad);

        // Delivery 1: both succeed. Delivery 2: bad errors → removed.
        hub.send(b"a".to_vec()).unwrap();
        hub.send(b"b".to_vec()).unwrap();
        // Delivery 3: only `good` remains, hub still Ok.
        hub.send(b"c".to_vec()).unwrap();

        assert_eq!(good.seqs(), vec![1, 2, 3]);
        assert_eq!(hub.last_seq(), 3);
        // The hub keeps absorbing output even with a dead subscriber.
    }

    #[test]
    fn no_subscribers_is_ok_never_a_sink_error() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        // pty_core sees Ok and keeps the child running — a client disconnect
        // must never look like a fatal sink error (§2.2).
        assert!(hub.send(b"data".to_vec()).is_ok());
        assert_eq!(hub.last_seq(), 1);
    }

    #[test]
    fn beginning_subscriber_replays_the_log_in_seq_order() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        // PTY output before any client attaches.
        hub.send(b"hello ".to_vec()).unwrap();
        hub.send(b"world".to_vec()).unwrap();
        assert_eq!(hub.last_seq(), 2);

        // subscribe_from_beginning replays the retained prefix in seq order.
        let rec = Arc::new(Recorder::default());
        hub.subscribe_from_beginning(rec.subscriber());
        assert_eq!(rec.seqs(), vec![1, 2]);
        assert_eq!(rec.count(), 11);

        // Live output continues normally after subscribe.
        hub.send(b"!".to_vec()).unwrap();
        assert_eq!(rec.seqs(), vec![1, 2, 3]);
    }

    #[test]
    fn increment_log_is_bounded() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        // 6000 * 100 bytes = 600KB > 512KB bound → oldest chunks dropped.
        for _ in 0..6000 {
            hub.send(vec![b'x'; 100]).unwrap();
        }
        assert!(hub.log_bytes() <= MAX_INCREMENT_BYTES);
        assert_eq!(hub.last_seq(), 6000);
    }

    // ── Phase 3: attach snapshot ─────────────────────────────────────────

    #[test]
    fn attach_without_baseline_returns_full_checkpoint() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        hub.send(b"line one\n".to_vec()).unwrap();
        hub.send(b"line two".to_vec()).unwrap();
        assert_eq!(hub.last_seq(), 2);

        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), None);

        assert_eq!(snap.snapshot_seq, 2);
        assert!(snap.checkpoint.is_some(), "fresh attach needs a checkpoint");
        assert!(snap.replay.is_empty());

        // The checkpoint reconstructs the current screen when fed to a parser.
        let mut p = Parser::new(24, 80, SCROLLBACK_LEN);
        p.process(&snap.checkpoint.clone().unwrap());
        assert!(p.screen().contents().contains("line one"));
        assert!(p.screen().contents().contains("line two"));

        // The subscriber gets only future chunks — nothing ≤ snapshot_seq.
        assert!(rec.seqs().is_empty());
        hub.send(b"more".to_vec()).unwrap();
        assert_eq!(rec.seqs(), vec![3]);
    }

    #[test]
    fn attach_with_after_seq_replays_only_the_gap() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        hub.send(b"aaaa".to_vec()).unwrap(); // seq 1
        hub.send(b"bbbb".to_vec()).unwrap(); // seq 2
        hub.send(b"cccc".to_vec()).unwrap(); // seq 3
        hub.send(b"dddd".to_vec()).unwrap(); // seq 4
        hub.send(b"eeee".to_vec()).unwrap(); // seq 5

        // Client already has state through seq 3 → gap is chunks 4,5.
        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), Some(3));

        assert_eq!(snap.snapshot_seq, 5);
        assert!(snap.checkpoint.is_none(), "gap replay must avoid a checkpoint");
        assert_eq!(snap.replay, b"ddddeeee".to_vec());

        // Live increments continue after the snapshot sequence.
        hub.send(b"ffff".to_vec()).unwrap();
        assert_eq!(rec.seqs(), vec![6]);
    }

    #[test]
    fn attach_with_stale_after_seq_falls_back_to_checkpoint() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        // Overflow the log so the oldest seqs are trimmed.
        for _ in 0..6000 {
            hub.send(vec![b'x'; 100]).unwrap();
        }
        let last = hub.last_seq();

        // after_seq=1 is far below the trimmed log → full checkpoint.
        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), Some(1));
        assert!(snap.checkpoint.is_some());
        assert_eq!(snap.snapshot_seq, last);
        assert!(rec.seqs().is_empty());
    }

    #[test]
    fn attach_after_seq_equal_to_snapshot_has_no_gap() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        hub.send(b"x".to_vec()).unwrap(); // seq 1
        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), Some(1));
        assert!(snap.checkpoint.is_none());
        assert!(snap.replay.is_empty());
        assert_eq!(snap.snapshot_seq, 1);
    }

    #[test]
    fn resize_updates_parser_size_for_checkpoint() {
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        // Content that overflows the 10-row target: after the shrink the visible
        // screen is a 10-row grid, and the cursor is pinned to the bottom row —
        // a stale 24-row parser would keep the cursor mid-screen instead.
        let mut bytes = Vec::new();
        for i in 0..12 {
            bytes.extend_from_slice(format!("row {i}\n").as_bytes());
        }
        hub.send(bytes.clone()).unwrap();
        hub.resize(10, 40);
        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), None);
        let ckpt = snap.checkpoint.unwrap();

        // The checkpoint is a faithful 10-row reconstruction: fed to a fresh
        // parser at the post-resize size it reproduces the hub's parser exactly.
        // Rebuilding the reference through the same 24→10 resize independently
        // proves the hub's resize AND the checkpoint geometry agree.
        let mut reference = Parser::new(24, 80, SCROLLBACK_LEN);
        reference.process(&bytes);
        reference.screen_mut().set_size(10, 40);
        let mut rebuilt = Parser::new(10, 40, SCROLLBACK_LEN);
        rebuilt.process(&ckpt);
        assert_eq!(
            rebuilt.screen().contents(),
            reference.screen().contents(),
            "resized checkpoint must rebuild the resized screen: {:?}",
            String::from_utf8_lossy(&ckpt)
        );
        assert_eq!(
            rebuilt.screen().cursor_position(),
            reference.screen().cursor_position(),
            "resized checkpoint must rebuild the resized cursor"
        );

        // The 10-row geometry is real: the render positions within the 10-row
        // grid (a no-op resize would keep the cursor on a 24-row grid).
        assert!(
            std::str::from_utf8(&ckpt)
                .unwrap()
                .contains("\x1b[10;"),
            "resized checkpoint must position within the 10-row grid: {:?}",
            String::from_utf8_lossy(&ckpt)
        );
    }

    #[test]
    fn full_checkpoint_reconstruction_roundtrips() {
        // The checkpoint renderer is round-trip tested in vt_checkpoint; here we
        // prove the hub feeds the parser the same bytes it would replay.
        let hub = Arc::new(AgentOutputHub::new("a", 24, 80));
        let seq = b"\x1b[32mGREEN\x1b[0m\nplain";
        hub.send(seq.to_vec()).unwrap();
        let rec = Arc::new(Recorder::default());
        let snap = hub.attach(rec.subscriber(), None);
        let ckpt = snap.checkpoint.unwrap();

        // A fresh parser fed ONLY the checkpoint sees the same text.
        let mut p = Parser::new(24, 80, SCROLLBACK_LEN);
        p.process(&ckpt);
        assert!(p.screen().contents().contains("GREEN"));
        assert!(p.screen().contents().contains("plain"));

        // And a fresh parser fed the ORIGINAL bytes sees the same text, proving
        // the checkpoint is a faithful reconstruction (§11 no-loss/no-dup).
        let mut orig = Parser::new(24, 80, SCROLLBACK_LEN);
        orig.process(seq);
        assert_eq!(p.screen().contents(), orig.screen().contents());
        // The render used inside the hub is the same tested function.
        let direct = render_checkpoint(&orig);
        let mut from_direct = Parser::new(24, 80, SCROLLBACK_LEN);
        from_direct.process(&direct);
        assert_eq!(from_direct.screen().contents(), orig.screen().contents());
    }
}
