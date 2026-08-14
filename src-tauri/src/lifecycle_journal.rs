//! Append-only, sequence-numbered record of agent lifecycle events (§3, §6.1) —
//! shared by the GUI bridge and the daemon.
//!
//! Natural exits and delete-mode cleanups are recorded here with a monotonic
//! sequence. While the GUI is connected these are redundant with live
//! `agent://*` events; the journal matters when the GUI is offline: Phase 4
//! replays `seq > last_acked` so todo/tab state catches up on reconnect. This is
//! an in-memory log for now — the disk layout / retention policy is a Phase 4
//! decision.

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Lifecycle event kinds the store records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEventKind {
    /// Agent exited on its own; `payload.exit_code` present.
    Exited,
    /// Agent record deleted (`session_end_mode=delete` or explicit delete).
    Removed,
}

/// One recorded lifecycle event. `seq` is globally monotonic across agents.
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleEvent {
    /// 1-based monotonic sequence, stable within a process lifetime.
    pub seq: u64,
    pub ts: i64,
    pub agent_id: String,
    #[serde(flatten)]
    pub kind: LifecycleEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Upper bound on retained journal events. The journal is in-memory (Phase 4 —
/// disk persistence is explicitly out of scope, brief §5), so a long-lived
/// daemon can't let it grow unbounded; a GUI that reconnects after more than
/// this many lifecycle events falls back on DB-driven session restore for the
/// older ones (which is already the source of truth for `done`/`removed`).
pub const MAX_JOURNAL_EVENTS: usize = 4096;

/// Shared lifecycle event log.
pub struct LifecycleJournal {
    events: Mutex<Vec<LifecycleEvent>>,
    next_seq: AtomicU64,
}

impl LifecycleJournal {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(0),
        }
    }

    /// Append an event and return its sequence number. Oldest events are
    /// dropped past [`MAX_JOURNAL_EVENTS`] (front-only, so `since()` stays a
    /// contiguous suffix in seq space).
    pub fn record(
        &self,
        agent_id: &str,
        kind: LifecycleEventKind,
        payload: Option<serde_json::Value>,
    ) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let ev = LifecycleEvent {
            seq,
            ts: now_ms(),
            agent_id: agent_id.to_string(),
            kind,
            payload,
        };
        let mut events = self.events.lock().unwrap_or_else(|p| p.into_inner());
        events.push(ev);
        while events.len() > MAX_JOURNAL_EVENTS {
            events.remove(0);
        }
        seq
    }

    /// Events with `seq > after` — the Phase 4 replay window for a reconnecting
    /// client that already acknowledged up to `after`.
    pub fn since(&self, after: u64) -> Vec<LifecycleEvent> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect()
    }

    /// Highest sequence recorded so far (0 when empty).
    pub fn last_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_are_monotonic_and_since_filters() {
        let j = LifecycleJournal::new();
        let a = j.record(
            "a",
            LifecycleEventKind::Exited,
            Some(serde_json::json!({"exit_code": 0})),
        );
        let b = j.record("b", LifecycleEventKind::Removed, None);
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(j.last_seq(), 2);

        // since(1) → only the second event.
        let tail = j.since(1);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 2);
        assert_eq!(tail[0].agent_id, "b");
        assert_eq!(tail[0].kind, LifecycleEventKind::Removed);

        // since(last) → empty.
        assert!(j.since(2).is_empty());
    }

    #[test]
    fn journal_is_bounded_by_max_events() {
        let j = LifecycleJournal::new();
        for i in 0..(MAX_JOURNAL_EVENTS + 100) {
            j.record(
                "a",
                LifecycleEventKind::Exited,
                Some(serde_json::json!({"n": i})),
            );
        }
        let tail = j.since(0);
        assert_eq!(tail.len(), MAX_JOURNAL_EVENTS, "oldest events dropped");
        // The surviving suffix is contiguous in seq space and reaches the head.
        assert_eq!(tail[0].seq, 101);
        assert_eq!(
            tail[MAX_JOURNAL_EVENTS - 1].seq,
            (MAX_JOURNAL_EVENTS + 100) as u64
        );
        assert_eq!(j.last_seq(), (MAX_JOURNAL_EVENTS + 100) as u64);
    }
}
