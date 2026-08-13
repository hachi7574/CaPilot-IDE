//! Tauri-independent PTY lifecycle core (design doc §2.1).
//!
//! Everything needed to own agent PTYs — spawn / write / resize / kill / reap,
//! the same-id spawn token, stale-reader generation checks, the natural-exit
//! callback, and the atomic live-session slot budget — lives here, with **no**
//! dependency on Tauri (or tokio). The GUI bridge (`lib.rs`) and, later, the PTY
//! daemon both drive this one core, so the race semantics fixed in `pty.rs`
//! (Bugs 1–5) cannot drift between the two owners.
//!
//! The one seam between `pty_core` and the outside world is the
//! [`OutputSink`] trait: the core pushes PTY bytes into whatever sink the
//! caller supplied. A sink error means the *subscriber* went away — it never
//! terminates the child (§2.3 "没有订阅者 → PTY 继续运行"). Killing a session
//! because its frontend channel died is a GUI-bridge policy, kept out of this
//! module (§2.2). The in-process fallback implements it with a sink that calls
//! [`PtyCore::kill`] on send failure (`ChannelSink` in `lib.rs`).

use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo, AgentStatus};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Hard cap on live PTY sessions (in-flight spawns + live children), enforced
/// atomically inside [`PtyCore::spawn`]. The old `live_count() >= MAX` check in
/// `lib.rs` was a check-then-act TOCTOU across different agent ids (§3).
pub const MAX_LIVE_SESSIONS: usize = 64;

/// Monotonic counter used to give each spawn attempt a unique token so that a
/// stale `kill()` can cancel only its *own* in-flight spawn (Bug 4).
///
/// Starts at 1 so a real generation is never `0` — `0` is the "missing/unset"
/// sentinel used by the daemon wire protocol (§4.2) and by callers that fall
/// back to `unwrap_or(0)` when an entry has already been reaped.
static NEXT_SPAWN_TOKEN: AtomicU64 = AtomicU64::new(1);

/// A *subscriber* failure, never a subprocess failure. Returning one of these
/// from [`OutputSink::send`] detaches the subscriber in `pty_core`; the child
/// keeps running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    /// The subscriber can no longer receive output (e.g. the Tauri Channel for
    /// this frontend closed). `pty_core` detaches the sink and keeps draining
    /// the PTY so the child stays unblocked.
    Closed,
    /// The subscriber cannot keep up (backpressure). Reserved for the daemon's
    /// bounded per-client queues (§4.3); the in-process fallback maps every
    /// Channel failure to `Closed` because the Tauri Channel API exposes no
    /// saturation classification.
    #[allow(dead_code)]
    Saturated,
}

pub type SinkResult = Result<(), SinkError>;

/// Output event interface between `pty_core` and its consumers. Must be
/// `Send + Sync + 'static` so it can cross into the reader thread and, later,
/// the daemon's IPC boundary. A sink that reports an error is detached — it
/// never causes the child to be killed (§2.2).
pub trait OutputSink: Send + Sync + 'static {
    fn send(&self, data: Vec<u8>) -> SinkResult;
}

/// Natural-exit callback: `(agent_id, exit_code)`. Fired by the reader thread
/// when the child exits on its own (EOF / read error). Intentional kills
/// (`kill()`, policy-sink kills) clear/suppress it so a session that was
/// deliberately stopped is never misreported as a natural "done".
pub type OnExit = Arc<dyn Fn(String, i32) + Send + Sync>;

/// One reserved live-session slot. Dropping the guard (spawn failure, spawn
/// cancellation, kill, or natural-exit reap) releases the slot exactly once.
struct SlotGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Atomic live-session budget (§3, §7). The count includes in-flight spawns, so
/// N concurrent spawn attempts can never all observe `live() < MAX` and overrun
/// the cap.
struct SlotReservation {
    counter: Arc<AtomicUsize>,
}

impl SlotReservation {
    fn new() -> Self {
        Self {
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn reserve(&self) -> Result<SlotGuard, AgentError> {
        let mut cur = self.counter.load(Ordering::Relaxed);
        loop {
            if cur >= MAX_LIVE_SESSIONS {
                return Err(AgentError::CapacityReached {
                    limit: MAX_LIVE_SESSIONS,
                });
            }
            match self.counter.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(SlotGuard { counter: self.counter.clone() }),
                Err(actual) => cur = actual,
            }
        }
    }

    /// Live sessions including in-flight spawns (for diagnostics / tests).
    fn live(&self) -> usize {
        self.counter.load(Ordering::Relaxed)
    }
}

/// Wrapper around a running PTY child process
struct PtyChild {
    pid: u32,
    /// The master PTY handle — kept alive so we can resize (TIOCSWINSZ) later.
    master: Box<dyn MasterPty + Send>,
    /// Writer into the PTY (frontend input). Wrapped in a per-agent Mutex so a
    /// blocking `write_all` never holds the global `children` lock (Bug 2).
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// The spawned child process. Stored so `kill()` actually terminates it
    /// (the old code leaked it via `std::mem::forget`).
    child: Box<dyn Child + Send + Sync>,
    /// Background reader thread streaming PTY output to the sink. The handle is
    /// stored only so the entry owns it; killing detaches it (a `std::thread`
    /// cannot be aborted, and the pre-existing abort was best-effort anyway —
    /// the real stale-reader protection is the generation check).
    reader_handle: Option<std::thread::JoinHandle<()>>,
    /// Natural-exit callback (see `OnExit`). `None` after an intentional kill.
    on_exit: Option<OnExit>,
    /// Set once the exit was intentional (kill / policy-sink kill), so the
    /// reader never fires `on_exit` for a deliberately stopped session.
    killed: Arc<AtomicBool>,
    /// Monotonic generation for this entry — the spawn token that created it.
    /// The reader thread compares the live map entry's generation to its own
    /// before reaping on EOF/read-error, so a stale reader left over from a
    /// `kill()` + respawn never removes the NEW child's entry (Bug 5).
    generation: u64,
    /// Live-session slot held while this entry exists. Released on drop.
    _slot: SlotGuard,
}

/// RAII guard that keeps `agent_id` in `PtyCore.spawning` for the duration
/// of a spawn, so concurrent spawn/resume calls for the same agent are
/// serialized (Bug 4). Removes the marker on drop (every exit path).
struct SpawnGuard {
    spawning: Arc<Mutex<HashMap<AgentId, u64>>>,
    id: AgentId,
    token: u64,
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Ok(mut spawning) = self.spawning.lock() {
            // Only remove our own marker; a newer spawn may have taken the slot.
            if spawning.get(&self.id) == Some(&self.token) {
                spawning.remove(&self.id);
            }
        }
    }
}

/// Remove a PTY entry from the map and reap (wait) the child process, returning
/// its exit code (-1 if unknown). Safe to call more than once: if the entry is
/// already gone (e.g. `kill()` won the race), this is a no-op. The map lock is
/// only held for the `remove` — the blocking `wait()` runs lock-free so other
/// PTY ops are never stalled.
fn reap_and_remove(children: &Arc<Mutex<HashMap<AgentId, PtyChild>>>, id: &AgentId) -> i32 {
    let pc = children.lock().unwrap().remove(id);
    if let Some(pc) = pc {
        // Drop the rest of the entry (master, writer, reader handle, slot),
        // then reap the child to avoid a zombie process.
        let PtyChild { child, .. } = pc;
        let mut child = child;
        child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1)
    } else {
        -1
    }
}

/// True if the live map entry for `id` is the one this reader started — i.e.
/// its generation still equals the token the reader captured at spawn time.
///
/// A stale reader can outlive its entry: killing cannot cancel a reader thread
/// blocked in `reader.read()`, so after a `kill()`-then-respawn (e.g.
/// `agent_resume` / `agent_switch_runtime`) the old reader eventually sees EOF
/// and would otherwise `reap_and_remove` the NEW child's entry. Guarding with
/// the generation keeps the new process alive and lets the stale reader just
/// clean itself up by returning (Bug 5).
fn is_own_entry(
    children: &Arc<Mutex<HashMap<AgentId, PtyChild>>>,
    id: &AgentId,
    generation: u64,
) -> bool {
    children
        .lock()
        .unwrap()
        .get(id)
        .map(|c| c.generation == generation)
        .unwrap_or(false)
}

/// Manages all PTY sessions. Tauri-independent; used by the GUI bridge now and
/// by the PTY daemon later.
///
/// Uses a `std::sync::Mutex` with synchronous methods so both async Tauri
/// commands and synchronous runtime callbacks can use it without holding locks
/// across await points.
pub struct PtyCore {
    children: Arc<Mutex<HashMap<AgentId, PtyChild>>>,
    /// Agent ids with a spawn currently in flight → the unique token of that
    /// spawn attempt. Used to serialize concurrent spawn/resume (Bug 4).
    spawning: Arc<Mutex<HashMap<AgentId, u64>>>,
    /// Atomic live-session budget (in-flight spawns + live children).
    slots: SlotReservation,
}

impl PtyCore {
    pub fn new() -> Self {
        Self {
            children: Arc::new(Mutex::new(HashMap::new())),
            spawning: Arc::new(Mutex::new(HashMap::new())),
            slots: SlotReservation::new(),
        }
    }

    /// Spawn a command in a new PTY and stream output to `sink`.
    ///
    /// Atomically reserves a live-session slot first, so the 64-session cap
    /// holds under concurrent spawn pressure and a failed spawn releases its
    /// quota (§11). A sink error detaches the sink — the child is never killed
    /// from inside `pty_core` (§2.2).
    pub fn spawn(
        &self,
        agent_id: AgentId,
        cmd: &str,
        args: &[String],
        cwd: &PathBuf,
        rows: u16,
        cols: u16,
        sink: Arc<dyn OutputSink>,
        on_exit: Option<OnExit>,
        env_overrides: &[(String, String)],
    ) -> Result<AgentInfo, AgentError> {
        // Reserve a live slot BEFORE any work. The guard releases it on every
        // failure path (openpty error, spawn error, cancellation) and moves it
        // into the map entry on success, so each live or in-flight PTY counts
        // exactly once until it is reaped or killed.
        let slot = self.slots.reserve()?;

        // Serialize concurrent spawn/resume for the same agent (Bug 4). The
        // token distinguishes each spawn attempt so `kill()` can cancel only
        // the spawn it was aimed at.
        let token = NEXT_SPAWN_TOKEN.fetch_add(1, Ordering::Relaxed);
        {
            let mut spawning = self.spawning.lock().unwrap();
            if spawning.contains_key(&agent_id) {
                return Err(AgentError::PtyError(format!(
                    "spawn in progress for agent {}",
                    agent_id
                )));
            }
            spawning.insert(agent_id.clone(), token);
        }
        let _guard = SpawnGuard {
            spawning: self.spawning.clone(),
            id: agent_id.clone(),
            token,
        };

        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;

        // Extract reader and writer from the master BEFORE spawning via slave
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| AgentError::PtyError(e.to_string()))?,
        ));
        // Keep the master handle for resize
        let master = pair.master;

        // Build command
        let mut command = CommandBuilder::new(cmd);
        for arg in args {
            command.arg(arg);
        }
        command.cwd(cwd);
        for (k, v) in env_overrides {
            command.env(k, v);
        }

        // Spawn the child via the slave (stdin/stdout/stderr connected to slave)
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let pid = child.process_id().unwrap_or(0);

        // The reader thread needs the same `killed`/`on_exit` that go into the
        // map entry, plus this spawn's unique token as its *generation*. Capture
        // them up front (clones) so a concurrent `kill()` + respawn between the
        // map insert and the reader starting can never make the reader bind
        // itself to a DIFFERENT (newer) entry (Bug 5).
        let generation = token;
        let killed = Arc::new(AtomicBool::new(false));
        let reader_killed = killed.clone();
        let reader_on_exit = on_exit.clone();
        let reader_sink = sink.clone();

        // Cancel-check + insert happen under ONE lock acquisition, so there is
        // no window where `kill()` can remove the `spawning` marker between the
        // check and the insert and still have the live child land in the map
        // (Bug 4 TOCTOU). The entry stores this spawn's token as `generation`,
        // so later readers / stale readers can tell which spawn owns it.
        {
            let spawning = self.spawning.lock().unwrap();
            let mut children = self.children.lock().unwrap();
            if spawning.get(&agent_id) != Some(&token) {
                let _ = child.kill();
                let _ = child.wait();
                // `slot` is still a local here → dropped on return → quota freed.
                return Err(AgentError::PtyError(format!(
                    "spawn cancelled for agent {}",
                    agent_id
                )));
            }
            // Insert into the map BEFORE starting the reader, so the reader's
            // EOF / sink-close cleanup can always find (and reap) the entry.
            // (Bug 1 — without this, a fast-exiting child would leave a stale
            // entry.)
            children.insert(
                agent_id.clone(),
                PtyChild {
                    pid,
                    master,
                    writer,
                    child,
                    reader_handle: None,
                    on_exit,
                    killed,
                    generation,
                    _slot: slot,
                },
            );
        }

        // Spawn a blocking reader thread (PTY reads are blocking I/O). On EOF
        // or read error it removes the map entry and reaps the child (Bug 1),
        // so no zombie and no stale state — but only if the live entry is still
        // this reader's own generation (Bug 5). Natural exit (EOF / read error)
        // also fires `on_exit`; intentional kills and policy-sink kills never
        // do.
        //
        // A sink error detaches the sink and the reader keeps draining (so the
        // child stays unblocked); it NEVER kills the child (§2.2). The
        // kill-on-dead-frontend policy lives in the GUI bridge's sink.
        let children_clone = self.children.clone();
        let reader_agent_id = agent_id.clone();
        let reader_handle = std::thread::Builder::new()
            .name(format!("pty-reader-{}", &reader_agent_id))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                let mut sink = Some(reader_sink);
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            // EOF — child exited. Remove + reap only our own
                            // entry (a stale reader must not tear down a
                            // respawned child), then report the natural exit so
                            // the session row can be finalized.
                            if is_own_entry(&children_clone, &reader_agent_id, generation) {
                                let exit_code =
                                    reap_and_remove(&children_clone, &reader_agent_id);
                                if !reader_killed.load(Ordering::SeqCst) {
                                    if let Some(cb) = &reader_on_exit {
                                        cb(reader_agent_id.clone(), exit_code);
                                    }
                                }
                            }
                            break;
                        }
                        Ok(n) => {
                            if let Some(s) = &sink {
                                if s.send(buf[..n].to_vec()).is_err() {
                                    // Subscriber gone. Detach and keep draining
                                    // so the child never blocks on a full PTY
                                    // buffer; the session continues headless.
                                    // Killing on a dead sink is the bridge's
                                    // policy, not pty_core's.
                                    sink = None;
                                }
                            }
                            // No sink (detached): output is discarded. The
                            // daemon replaces this with a bounded log (§5).
                        }
                        Err(_) => {
                            // Read error (e.g. master closed) — remove + reap
                            // only our own entry, then report the natural exit
                            // like EOF.
                            if is_own_entry(&children_clone, &reader_agent_id, generation) {
                                let exit_code =
                                    reap_and_remove(&children_clone, &reader_agent_id);
                                if !reader_killed.load(Ordering::SeqCst) {
                                    if let Some(cb) = &reader_on_exit {
                                        cb(reader_agent_id.clone(), exit_code);
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            })
            .map_err(|e| AgentError::PtyError(e.to_string()))?;

        // Attach the reader handle now that the reader is actually running. If
        // the reader already cleaned up (instant EOF), the entry is gone and
        // the handle is simply dropped (detached).
        if let Some(child) = self.children.lock().unwrap().get_mut(&agent_id) {
            child.reader_handle = Some(reader_handle);
        }

        Ok(AgentInfo {
            id: agent_id,
            workspace_id: None,
            project: None,
            runtime: String::new(),
            status: AgentStatus::Running,
            title: String::new(),
            cwd: cwd.clone(),
            pid: Some(pid),
            mode: String::new(),
            speed: String::new(),
            model: None,
            last_usage: None,
        })
    }

    /// Write input to an agent's PTY.
    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<(), AgentError> {
        // Clone the per-agent writer Arc under the map lock (brief), then drop
        // the map lock before doing the blocking `write_all`. This keeps the
        // global `children` Mutex uncontended (Bug 2).
        let writer = {
            let children = self.children.lock().unwrap();
            let child = children
                .get(agent_id)
                .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
            child.writer.clone()
        };
        let mut writer = writer.lock().unwrap();
        writer
            .write_all(data)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        Ok(())
    }

    /// Resize an agent's PTY via the stored master fd (TIOCSWINSZ).
    pub fn resize(&self, agent_id: &str, rows: u16, cols: u16) -> Result<(), AgentError> {
        let mut children = self.children.lock().unwrap();
        let child = children
            .get_mut(agent_id)
            .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        child
            .master
            .resize(size)
            .map_err(|e| AgentError::PtyError(e.to_string()))
    }

    /// Kill an agent's PTY process and clean up.
    pub fn kill(&self, agent_id: &str) -> Result<(), AgentError> {
        // Cancel any in-flight spawn for this agent (Bug 4): removing the
        // marker makes that spawn's post-`spawn_command` check fail, so it
        // kills + reaps its own child instead of inserting a stale entry.
        self.spawning.lock().unwrap().remove(agent_id);

        // Remove the entry up front. The reader (if it wins the race to EOF)
        // will see its own generation is gone and skip `reap_and_remove`
        // (Bug 5), so a subsequent respawn's entry is never torn down.
        let pc = self.children.lock().unwrap().remove(agent_id);
        if let Some(mut pc) = pc {
            // Detach the reader thread. A `std::thread` cannot be interrupted,
            // so it keeps running until its blocking read returns; the real
            // protection for a respawned entry is the generation check (Bug 5).
            pc.reader_handle.take();
            // Intentional kill — the reader (if it wins the race to EOF) must
            // not fire `on_exit` and misreport this as a natural session end.
            pc.killed.store(true, Ordering::SeqCst);
            pc.on_exit = None;
            let _ = pc.child.kill();
            // Reap to avoid a zombie (Bug 1).
            let _ = pc.child.wait();
        }
        Ok(())
    }

    /// Kill every live PTY (used on app quit so no agent process is orphaned).
    /// Same semantics as `kill`: intentional teardown, so `on_exit` is
    /// suppressed and the session rows stay `running` (recoverable next launch).
    pub fn kill_all(&self) {
        let ids: Vec<AgentId> = self.children.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.kill(&id);
        }
    }

    /// Snapshot of live agent PIDs — `(agent_id, pid)`. Used by the resource
    /// monitor (§10) to sample whole process trees per agent.
    pub fn pids(&self) -> Vec<(String, u32)> {
        self.children
            .lock()
            .unwrap()
            .iter()
            .map(|(id, c)| (id.clone(), c.pid))
            .collect()
    }

    /// Generation of a live agent's PTY incarnation, or `None` when the agent is
    /// not live. Exposed for the daemon's wire protocol — `Write`/`Resize`/`Kill`
    /// carry a generation so a stale client can't steer a respawned process (§4.2).
    pub fn generation(&self, agent_id: &str) -> Option<u64> {
        self.children
            .lock()
            .unwrap()
            .get(agent_id)
            .map(|c| c.generation)
    }

    /// Number of live PTY sessions, including in-flight spawns (diagnostics).
    /// Consumed by the pty_core tests today and by the daemon's `List` (§4.2)
    /// in Phase 2.
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.slots.live()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A sink that accepts and discards all output.
    struct NoopSink;
    impl OutputSink for NoopSink {
        fn send(&self, _data: Vec<u8>) -> SinkResult {
            Ok(())
        }
    }

    /// A sink that always reports the subscriber is gone — used to exercise the
    /// daemon semantics: pty_core must detach the sink, not kill the child.
    struct AlwaysFailSink;
    impl OutputSink for AlwaysFailSink {
        fn send(&self, _data: Vec<u8>) -> SinkResult {
            Err(SinkError::Closed)
        }
    }

    /// The in-process fallback policy (§8): a dead frontend channel terminates
    /// the session. The policy lives in the SINK (the GUI bridge), never in
    /// pty_core.
    struct KillOnFailSink {
        pty: Arc<PtyCore>,
        agent_id: String,
    }
    impl OutputSink for KillOnFailSink {
        fn send(&self, _data: Vec<u8>) -> SinkResult {
            let _ = self.pty.kill(&self.agent_id);
            Err(SinkError::Closed)
        }
    }

    fn sh(script: &str) -> (&'static str, Vec<String>) {
        ("/bin/sh", vec!["-c".to_string(), script.to_string()])
    }

    fn spawn_script(
        pty: &Arc<PtyCore>,
        id: &str,
        script: &str,
        on_exit: Option<OnExit>,
    ) -> Result<AgentInfo, AgentError> {
        let (cmd, args) = sh(script);
        pty.spawn(
            id.to_string(),
            cmd,
            &args,
            &std::env::current_dir().unwrap(),
            24,
            80,
            Arc::new(NoopSink),
            on_exit,
            &[],
        )
    }

    /// Poll `cond` until it is true or `timeout` elapses.
    fn wait_until(cond: impl Fn() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    // ------------------------------------------------------------------
    // Bug 1 — a fast-exiting child must not leave a stale map entry, and its
    // natural exit must be reported with the real exit code.
    // ------------------------------------------------------------------
    #[test]
    fn fast_exit_leaves_no_stale_entry_and_fires_on_exit() {
        let pty = Arc::new(PtyCore::new());
        let (tx, rx) = mpsc::channel();
        let on_exit: OnExit = Arc::new(move |id, code| {
            let _ = tx.send((id, code));
        });
        let info = spawn_script(&pty, "fast", "exit 3", Some(on_exit)).unwrap();
        assert!(info.pid.is_some());

        let (fired_id, code) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(fired_id, "fast");
        assert_eq!(code, 3, "natural exit must carry the child's exit code");

        // No stale entry survives the fast exit.
        assert!(
            wait_until(|| pty.live_count() == 0, Duration::from_secs(2)),
            "live_count()={} should drain to 0 after a fast exit",
            pty.live_count()
        );
        assert!(pty.pids().is_empty());
    }

    // ------------------------------------------------------------------
    // Bug 2 — a blocking `write_all` must not hold the global `children` lock.
    // We fill agent A's PTY input buffer (its child never reads stdin), so the
    // write blocks on the per-agent writer mutex; meanwhile killing agent B must
    // still complete promptly instead of waiting behind A's write.
    // ------------------------------------------------------------------
    #[test]
    fn blocking_write_does_not_hold_global_lock() {
        let pty = Arc::new(PtyCore::new());
        // Both children live long and never read stdin.
        spawn_script(&pty, "a", "sleep 1000", None).unwrap();
        spawn_script(&pty, "b", "sleep 1000", None).unwrap();

        // A thread that blocks writing a large payload to A.
        let pty_w = pty.clone();
        let writer = std::thread::spawn(move || {
            let big = vec![0u8; 512 * 1024];
            let _ = pty_w.write("a", &big);
        });
        // Give the write time to fill the PTY buffer and block.
        std::thread::sleep(Duration::from_millis(300));

        // A concurrent kill of a different agent must not be stalled by A's
        // blocked write (the global lock is not held during write_all).
        let t0 = Instant::now();
        pty.kill("b").unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "kill(b) blocked behind a's in-flight write: {:?}",
            t0.elapsed()
        );

        // Killing A closes its master, unblocking the writer thread.
        pty.kill("a").unwrap();
        let _ = writer.join();
        assert_eq!(pty.live_count(), 0);
    }

    // ------------------------------------------------------------------
    // Bug 3 — a dead sink must NOT kill the child inside pty_core (daemon
    // semantics: "没有订阅者 → PTY 继续运行"). The session keeps its entry.
    // ------------------------------------------------------------------
    #[test]
    fn sink_failure_detaches_without_killing_child() {
        let pty = Arc::new(PtyCore::new());
        let (cmd, args) = sh("while true; do echo x; done"); // chatty, never exits
        let info = pty
            .spawn(
                "a".to_string(),
                cmd,
                &args,
                &std::env::current_dir().unwrap(),
                24,
                80,
                Arc::new(AlwaysFailSink),
                None,
                &[],
            )
            .unwrap();
        let pid = info.pid.unwrap();

        // The reader hits the failing sink on the first output chunk and
        // detaches — but the child must keep running and keep its entry.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            pty.live_count(),
            1,
            "a dead sink must not tear the session down inside pty_core"
        );
        assert_eq!(pty.pids(), vec![("a".to_string(), pid)]);

        // Explicit kill is the only thing that ends it.
        pty.kill("a").unwrap();
        assert_eq!(pty.live_count(), 0);
    }

    // ------------------------------------------------------------------
    // Bug 3 fallback — the GUI bridge's policy sink (kill on send failure)
    // reproduces the pre-daemon "channel gone ⇒ kill" behavior, and the kill
    // must suppress the natural-exit callback.
    // ------------------------------------------------------------------
    #[test]
    fn fallback_policy_kills_on_sink_failure_and_suppresses_on_exit() {
        let pty = Arc::new(PtyCore::new());
        let (tx, rx) = mpsc::channel();
        let on_exit: OnExit = Arc::new(move |id, code| {
            let _ = tx.send((id, code));
        });
        let sink = Arc::new(KillOnFailSink {
            pty: pty.clone(),
            agent_id: "a".to_string(),
        });
        let (cmd, args) = sh("while true; do echo x; done");
        pty.spawn(
            "a".to_string(),
            cmd,
            &args,
            &std::env::current_dir().unwrap(),
            24,
            80,
            sink,
            Some(on_exit),
            &[],
        )
        .unwrap();

        // The policy sink kills on the first output chunk → entry removed.
        assert!(
            wait_until(|| pty.live_count() == 0, Duration::from_secs(3)),
            "policy sink should have torn the session down"
        );
        // Intentional teardown → on_exit must NOT fire.
        assert!(
            rx.try_recv().is_err(),
            "on_exit must be suppressed for a policy-sink kill"
        );
    }

    // ------------------------------------------------------------------
    // Bug 4 — concurrent spawn/resume for the same agent is serialized by the
    // `spawning` marker, and failed/cancelled spawns never leak a slot.
    // ------------------------------------------------------------------
    #[test]
    fn concurrent_same_id_spawn_is_serialized() {
        let pty = Arc::new(PtyCore::new());
        let cwd = std::env::current_dir().unwrap();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let pty = pty.clone();
            let cwd = cwd.clone();
            handles.push(std::thread::spawn(move || {
                let (cmd, args) = sh("sleep 0.05");
                pty.spawn(
                    "same".to_string(),
                    cmd,
                    &args,
                    &cwd,
                    24,
                    80,
                    Arc::new(NoopSink),
                    None,
                    &[],
                )
            }));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // No panics; every rejection is the in-progress error, not corruption.
        for r in &results {
            if let Err(e) = r {
                assert!(
                    e.to_string().contains("spawn in progress"),
                    "unexpected error for concurrent same-id spawn: {e}"
                );
            }
        }
        // All 8 reservations eventually drain (children exit on their own).
        assert!(
            wait_until(|| pty.live_count() == 0, Duration::from_secs(3)),
            "slots leaked: live_count()={}",
            pty.live_count()
        );
    }

    // ------------------------------------------------------------------
    // Bug 5 — after kill()+respawn, the stale reader from the old process must
    // not remove the new child's entry; the new child's natural exit is the one
    // reported.
    // ------------------------------------------------------------------
    #[test]
    fn kill_then_respawn_stale_reader_keeps_new_entry() {
        let pty = Arc::new(PtyCore::new());
        let (tx, rx) = mpsc::channel();
        let on_exit: OnExit = Arc::new(move |id, code| {
            let _ = tx.send((id, code));
        });
        let cwd = std::env::current_dir().unwrap();

        // First child is chatty, so its reader is actively reading when killed.
        let (cmd1, args1) = sh("while true; do echo x; done");
        let info1 = pty
            .spawn(
                "a".to_string(),
                cmd1,
                &args1,
                &cwd,
                24,
                80,
                Arc::new(NoopSink),
                Some(on_exit.clone()),
                &[],
            )
            .unwrap();
        let pid1 = info1.pid.unwrap();
        std::thread::sleep(Duration::from_millis(150)); // reader is now mid-read

        pty.kill("a").unwrap();
        assert_eq!(pty.live_count(), 0);

        // Respawn the SAME id — a brand-new process and generation.
        let (cmd2, args2) = sh("sleep 0.4");
        let info2 = pty
            .spawn(
                "a".to_string(),
                cmd2,
                &args2,
                &cwd,
                24,
                80,
                Arc::new(NoopSink),
                Some(on_exit),
                &[],
            )
            .unwrap();
        let pid2 = info2.pid.unwrap();
        assert_ne!(pid1, pid2, "kill+respawn must produce a new process");

        // The new child exits naturally; the stale reader's EOF must not reap
        // the new entry first (its generation no longer matches).
        let (fired_id, code) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(fired_id, "a");
        assert_eq!(code, 0);
        assert!(
            wait_until(|| pty.live_count() == 0, Duration::from_secs(2)),
            "new entry should be reaped after natural exit"
        );
    }

    // ------------------------------------------------------------------
    // Natural vs explicit exit — an explicit kill suppresses the natural-exit
    // callback.
    // ------------------------------------------------------------------
    #[test]
    fn explicit_kill_suppresses_natural_exit_callback() {
        let pty = Arc::new(PtyCore::new());
        let (tx, rx) = mpsc::channel();
        let on_exit: OnExit = Arc::new(move |id, code| {
            let _ = tx.send((id, code));
        });
        spawn_script(&pty, "a", "sleep 30", Some(on_exit)).unwrap();
        std::thread::sleep(Duration::from_millis(100)); // let the reader start

        pty.kill("a").unwrap();
        assert_eq!(pty.live_count(), 0);
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "explicit kill must suppress the natural-exit callback"
        );
    }

    // ------------------------------------------------------------------
    // Session cap — MAX_LIVE_SESSIONS is enforced atomically, a rejected spawn
    // does not leak a slot, and killing one frees the slot for a replacement
    // (respawn replaces an existing live slot without a permanent double-count).
    // ------------------------------------------------------------------
    #[test]
    fn capacity_cap_is_enforced_and_failed_spawns_release_quota() {
        let pty = Arc::new(PtyCore::new());
        let cwd = std::env::current_dir().unwrap();
        let (tx, _rx) = mpsc::channel::<(String, i32)>();
        let on_exit: OnExit = Arc::new(move |id, code| {
            let _ = tx.send((id, code));
        });

        // Fill to the cap with long-lived children.
        for i in 0..MAX_LIVE_SESSIONS {
            let id = format!("s{i}");
            let (cmd, args) = sh("sleep 30");
            let res = pty.spawn(
                id.clone(),
                cmd,
                &args,
                &cwd,
                24,
                80,
                Arc::new(NoopSink),
                Some(on_exit.clone()),
                &[],
            );
            assert!(res.is_ok(), "spawn {id} failed: {:?}", res.err());
        }
        assert_eq!(pty.live_count(), MAX_LIVE_SESSIONS);

        // The (MAX+1)-th spawn is rejected with the capacity error.
        let (cmd, args) = sh("sleep 30");
        let err = pty
            .spawn(
                "overflow".to_string(),
                cmd,
                &args,
                &cwd,
                24,
                80,
                Arc::new(NoopSink),
                None,
                &[],
            )
            .unwrap_err();
        assert!(
            matches!(err, AgentError::CapacityReached { limit } if limit == MAX_LIVE_SESSIONS),
            "expected CapacityReached, got {err}"
        );
        assert_eq!(
            pty.live_count(),
            MAX_LIVE_SESSIONS,
            "a rejected spawn must not leak a slot"
        );

        // Killing one live session frees its slot for a replacement.
        pty.kill("s0").unwrap();
        assert_eq!(pty.live_count(), MAX_LIVE_SESSIONS - 1);
        let (cmd, args) = sh("sleep 30");
        let res = pty.spawn(
            "replacement".to_string(),
            cmd,
            &args,
            &cwd,
            24,
            80,
            Arc::new(NoopSink),
            None,
            &[],
        );
        assert!(res.is_ok(), "slot should be reusable after a kill");
        assert_eq!(pty.live_count(), MAX_LIVE_SESSIONS);

        // Clean up so no reader thread or child outlives the test.
        for i in 0..MAX_LIVE_SESSIONS {
            let _ = pty.kill(&format!("s{i}"));
        }
        let _ = pty.kill("replacement");
        assert!(wait_until(|| pty.live_count() == 0, Duration::from_secs(3)));
    }
}
