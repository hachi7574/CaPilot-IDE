//! Binary-level smoke test for the PTY daemon (§4.1/§4.2).
//!
//! Spawns the REAL `capilot-ide --daemon` executable (the same `current_exe()
//! --daemon` the GUI bridge spawns) with a redirected `HOME`, then drives it
//! through the `DaemonClient`: spawn → banner output → second-client attach
//! (checkpoint) → live echo. This exercises the actual process boundary, not
//! just the in-process `DaemonServer` used by the library tests.

use capilot_ide_lib::agent_provider::manager::NewAgentRequest;
use capilot_ide_lib::daemon::client::{ClientError, DaemonClient};
use capilot_ide_lib::daemon::protocol::{ClientEvent, PROTOCOL_VERSION};
use capilot_ide_lib::daemon::runtime::socket_path;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SMOKE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_home() -> PathBuf {
    std::env::temp_dir().join(format!(
        "capilot-smoke-home-{}-{}",
        std::process::id(),
        SMOKE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn spawn_daemon(home: &PathBuf) -> Child {
    Command::new(env!("CARGO_BIN_EXE_capilot-ide"))
        .arg("--daemon")
        .env("HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon binary")
}

/// Poll `DaemonClient::connect` until the freshly-spawned daemon's socket +
/// token exist, or the deadline hits.
fn wait_for_daemon(base: &PathBuf, deadline: Instant) -> DaemonClient {
    loop {
        match DaemonClient::connect(base, "smoke-test") {
            Ok(c) => return c,
            Err(ClientError::NotRunning) => {
                assert!(
                    Instant::now() < deadline,
                    "daemon did not come up within the deadline"
                );
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => panic!("daemon connect failed: {e:?}"),
        }
    }
}

fn read_until(client: &DaemonClient, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut got = String::new();
    while Instant::now() < deadline {
        match client.recv_event_timeout(Duration::from_millis(300)) {
            Ok(ClientEvent::Output { data, .. }) => {
                got.push_str(&String::from_utf8_lossy(&data));
                if got.contains(needle) {
                    return;
                }
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    panic!("needle {needle:?} not seen; got {got:?}");
}

#[test]
fn daemon_binary_spawn_attach_live_smoke() {
    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let mut child = spawn_daemon(&home);
    let base = home.join("CaPilot");

    // The spawner owns the input lease.
    let c1 = wait_for_daemon(&base, Instant::now() + Duration::from_secs(15));
    let (_pid, generation) = c1
        .spawn(
            "smoke1",
            "/bin/sh",
            &[
                "-c".into(),
                "echo __READY__; while read x; do echo \"got:$x\"; done".into(),
            ],
            &std::env::temp_dir(),
            &[],
            24,
            80,
        )
        .expect("spawn");
    read_until(&c1, "__READY__", Duration::from_secs(5));

    // A second client attaches: full checkpoint that rebuilds the banner screen,
    // and takes over the input lease.
    let c2 = wait_for_daemon(&base, Instant::now() + Duration::from_secs(5));
    let result = c2
        .attach("smoke1", generation, 24, 80, None)
        .expect("attach");
    assert!(
        result.checkpoint.is_some(),
        "fresh attach needs a checkpoint"
    );
    assert!(result.replay.is_empty(), "fresh attach has no gap");
    let mut p = vt100::Parser::new(24, 80, 200);
    p.process(&result.checkpoint.unwrap());
    assert!(
        p.screen().contents().contains("__READY__"),
        "checkpoint must rebuild the banner screen"
    );

    // c2 holds the lease now; its write echoes back live.
    c2.write("smoke1", generation, "ping\n").expect("write");
    read_until(&c2, "got:ping", Duration::from_secs(5));

    // Clean shutdown: the daemon kills its PTYs and exits 0.
    c1.shutdown().expect("shutdown");
    let status = child.wait().expect("wait for daemon");
    assert!(status.success(), "daemon exited abnormally: {status:?}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn daemon_binary_wrong_token_is_rejected() {
    use capilot_ide_lib::daemon::protocol::{
        read_frame, write_frame, Hello, FRAME_ERROR, FRAME_HELLO, PROTOCOL_VERSION,
    };
    use capilot_ide_lib::daemon::runtime::socket_path;
    use std::os::unix::net::UnixStream;

    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let mut child = spawn_daemon(&home);
    let base = home.join("CaPilot");

    // Wait for the socket to exist (the daemon writes it at bind time).
    let deadline = Instant::now() + Duration::from_secs(15);
    let socket = socket_path(&base);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "daemon socket never appeared");
        std::thread::sleep(Duration::from_millis(150));
    }

    // Raw hello with a wrong token → the daemon replies with an ERROR frame
    // and closes the connection (never accepts the client).
    let mut stream = UnixStream::connect(&socket).expect("connect to socket");
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        app_version: "smoke-test".into(),
        token: "definitely-wrong".into(),
    };
    write_frame(
        &mut stream,
        FRAME_HELLO,
        0,
        &serde_json::to_vec(&hello).unwrap(),
    )
    .expect("write hello");
    let frame = read_frame(&mut stream).expect("read error frame");
    assert_eq!(frame.kind, FRAME_ERROR, "daemon must reject a wrong token");
    assert!(
        read_frame(&mut stream).is_err(),
        "daemon must close after rejecting"
    );

    // A correct-token client still connects (the daemon is healthy).
    let client = DaemonClient::connect(&base, "smoke-test").expect("correct-token connect");
    client.shutdown().expect("shutdown");
    let status = child.wait().expect("wait for daemon");
    assert!(status.success(), "daemon exited abnormally: {status:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn daemon_binary_version_mismatch_is_detected() {
    use capilot_ide_lib::daemon::protocol::{
        read_frame, write_frame, HelloAck, FRAME_HELLO, FRAME_HELLO_ACK, PROTOCOL_VERSION,
    };
    use capilot_ide_lib::daemon::runtime::{socket_path, write_token};
    use std::os::unix::net::UnixListener;

    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let base = home.join("CaPilot");
    // A client needs the token file before it will connect.
    write_token(&base, "test-token").unwrap();
    let socket = socket_path(&base);

    // Scripted "stale daemon": binds the socket and answers the handshake with
    // an older protocol version (as a leftover resident daemon from a previous
    // build would). The client must surface this as `VersionMismatch` — the
    // bridge's trigger for replacing the stale daemon — not a generic
    // `Handshake` error.
    let listener = UnixListener::bind(&socket).expect("bind test socket");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let frame = read_frame(&mut stream).expect("read hello");
        assert_eq!(frame.kind, FRAME_HELLO);
        let ack = HelloAck {
            protocol_version: 3,
            daemon_instance_id: "d_fake_stale".into(),
            capabilities: vec![],
        };
        write_frame(
            &mut stream,
            FRAME_HELLO_ACK,
            0,
            &serde_json::to_vec(&ack).unwrap(),
        )
        .expect("write ack");
    });

    let err =
        DaemonClient::connect(&base, "smoke-test").expect_err("stale daemon must not connect");
    match err {
        ClientError::VersionMismatch { daemon, client } => {
            assert_eq!(daemon, 3);
            assert_eq!(client, PROTOCOL_VERSION);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
    server.join().expect("server thread panicked");
    let _ = std::fs::remove_dir_all(&home);
}

/// Phase 4 keepalive (§9.4): a GUI that exits DETACHES — the daemon and the
/// agent PTY keep running, and a fresh GUI launch re-attaches to the SAME
/// generation without a provider resume.
#[test]
fn daemon_binary_detach_survives_gui_exit_and_reattaches() {
    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let mut child = spawn_daemon(&home);
    let base = home.join("CaPilot");

    // GUI #1 spawns a session and owns the input lease.
    let c1 = wait_for_daemon(&base, Instant::now() + Duration::from_secs(15));
    let (pid, generation) = c1
        .spawn(
            "keep1",
            "/bin/sh",
            &[
                "-c".into(),
                "echo __READY__; while read x; do echo \"got:$x\"; done".into(),
            ],
            &std::env::temp_dir(),
            &[],
            24,
            80,
        )
        .expect("spawn");
    read_until(&c1, "__READY__", Duration::from_secs(5));

    // GUI #1 exits → detach (NOT shutdown). The daemon stays alive.
    c1.detach().expect("detach");
    drop(c1);

    // GUI #2 launches: the daemon is still up and lists the same session with
    // the SAME (generation, pid) — no provider resume happened.
    let c2 = wait_for_daemon(&base, Instant::now() + Duration::from_secs(5));
    let sessions = c2.list().expect("list after gui restart");
    assert_eq!(sessions.len(), 1, "session must survive GUI exit");
    assert_eq!(sessions[0].agent_id, "keep1");
    assert_eq!(
        sessions[0].generation, generation,
        "same generation after reattach"
    );
    assert_eq!(sessions[0].pid, pid, "same pid — no respawn");

    // GUI #2 attaches to the same generation and takes the lease.
    let result = c2
        .attach("keep1", generation, 24, 80, None)
        .expect("attach");
    assert!(
        result.checkpoint.is_some(),
        "checkpoint rebuilds the live screen"
    );
    c2.write("keep1", generation, "ping\n").expect("write");
    read_until(&c2, "got:ping", Duration::from_secs(5));

    // Clean shutdown only when the last GUI leaves for good.
    c2.shutdown().expect("shutdown");
    let status = child.wait().expect("wait for daemon");
    assert!(status.success(), "daemon exited abnormally: {status:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Structured agent round-trip at the process boundary (architecture §13): the
/// real daemon binary owns an `AgentManager`; the GUI's bridge proxies
/// `AgentCreate`/`AgentGetSnapshot`/`AgentClose` to it. Skipped (self-returning)
/// when `opencode` is not on PATH — the daemon registers only the OpenCode ACP
/// provider (§7.2), so a fresh HOME would otherwise report provider_not_found.
/// Cost: agent_create spawns `opencode acp` but never prompts, so no model
/// tokens are consumed.
#[test]
fn daemon_binary_structured_agent_roundtrip() {
    fn opencode_available() -> bool {
        let probe = Command::new("opencode")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match probe {
            Ok(mut p) => p.wait().map(|s| s.success()).unwrap_or(false),
            Err(_) => false,
        }
    }
    if !opencode_available() {
        eprintln!("skipping: `opencode` not on PATH");
        return;
    }

    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let mut child = spawn_daemon(&home);
    let base = home.join("CaPilot");
    let client = wait_for_daemon(&base, Instant::now() + Duration::from_secs(15));

    // Providers list includes the lazily-registered OpenCode ACP profile, with
    // its backend kind (acp) so the frontend can create agents correctly.
    let providers = client.provider_list().expect("provider_list");
    assert!(
        providers.iter().any(|p| p.provider_id == "opencode"),
        "providers: {providers:?}"
    );
    assert!(
        providers
            .iter()
            .any(|p| p.provider_id == "opencode" && p.backend_kind == "acp"),
        "opencode must report backend_kind=acp: {providers:?}"
    );

    // Create → the manager reserves the record and the ACP handshake emits
    // SessionReady; the snapshot reflects an idle structured agent.
    let snap = client
        .agent_create(NewAgentRequest {
            agent_id: "structured-1".into(),
            provider_id: "opencode".into(),
            backend_kind: "acp".into(),
            workspace_id: None,
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        })
        .expect("agent_create");
    assert_eq!(snap.agent.provider_id, "opencode");
    assert_eq!(snap.agent.backend_kind, "acp");
    assert_eq!(
        snap.agent.status,
        capilot_ide_lib::agent_provider::types::AgentStatus::Idle
    );
    assert!(snap.last_seq >= 1, "SessionReady is sequenced");

    // Snapshot re-read from a second client (reconnect path) must agree.
    let c2 = wait_for_daemon(&base, Instant::now() + Duration::from_secs(5));
    let snap2 = c2
        .agent_get_snapshot("structured-1")
        .expect("agent_get_snapshot");
    assert_eq!(snap2.agent.agent_id, "structured-1");
    assert_eq!(
        snap2.agent.status,
        capilot_ide_lib::agent_provider::types::AgentStatus::Idle
    );

    // Close releases the live ACP session; the record stays (closed ≠ deleted).
    c2.agent_close("structured-1").expect("agent_close");
    let listed = client.agent_list().expect("agent_list");
    assert!(
        listed.iter().any(|a| a.agent_id == "structured-1"),
        "record persists"
    );

    client.shutdown().expect("shutdown");
    let status = child.wait().expect("wait for daemon");
    assert!(status.success(), "daemon exited abnormally: {status:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Full replace path (§8): a resident daemon from a previous build answers the
/// handshake with an old protocol. `PtyBridge::start()` must kill it (never
/// fall back under a live owner) and spawn a fresh daemon on the current
/// binary. Needs a genuinely stale daemon binary; skipped unless
/// `CAPILOT_STALE_DAEMON_EXE` points at one.
#[test]
#[ignore = "requires a stale daemon binary; set CAPILOT_STALE_DAEMON_EXE"]
fn daemon_bridge_replaces_stale_daemon() {
    let stale_exe = match std::env::var("CAPILOT_STALE_DAEMON_EXE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("SKIP: CAPILOT_STALE_DAEMON_EXE is unset");
            return;
        }
    };
    let home = tmp_home();
    std::fs::create_dir_all(&home).unwrap();
    let base = home.join("CaPilot");

    // Resident stale daemon (old protocol) owns `base`.
    let mut stale = Command::new(&stale_exe)
        .arg("--daemon")
        .env("HOME", &home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn stale daemon");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !socket_path(&base).exists() {
        assert!(
            Instant::now() < deadline,
            "stale daemon socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
    // Prove it really is a protocol mismatch before the bridge replaces it.
    match DaemonClient::connect(&base, "smoke-test") {
        Err(ClientError::VersionMismatch { daemon, .. }) => {
            assert_ne!(
                daemon, PROTOCOL_VERSION,
                "stale binary must differ in protocol"
            )
        }
        other => panic!("expected VersionMismatch from stale daemon, got {other:?}"),
    }

    // Drive the real bridge. The spawn seam points at the CURRENT app binary
    // (current_exe() in a test harness is the test binary).
    std::env::set_var("CAPILOT_DAEMON_EXE", env!("CARGO_BIN_EXE_capilot-ide"));
    std::env::set_var("HOME", &home);
    let bridge = capilot_ide_lib::bridge::PtyBridge::start();

    // The stale daemon must have been killed and a fresh one must be serving.
    assert_eq!(
        bridge.mode(),
        "daemon",
        "bridge must be daemon-backed after replacement"
    );
    let stale_gone = (0..50).any(|_| match stale.try_wait().expect("try_wait") {
        Some(_) => true,
        None => {
            std::thread::sleep(Duration::from_millis(100));
            false
        }
    });
    assert!(
        stale_gone,
        "stale daemon process must exit after replacement"
    );

    // Clean up: the replaced daemon answers on the current protocol.
    let fresh = DaemonClient::connect(&base, "smoke-test").expect("replaced daemon connects");
    fresh.shutdown().expect("shutdown");
    let _ = std::fs::remove_dir_all(&home);
}
