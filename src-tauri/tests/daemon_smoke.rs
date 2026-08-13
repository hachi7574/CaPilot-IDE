//! Binary-level smoke test for the PTY daemon (§4.1/§4.2).
//!
//! Spawns the REAL `capilot-ide --daemon` executable (the same `current_exe()
//! --daemon` the GUI bridge spawns) with a redirected `HOME`, then drives it
//! through the `DaemonClient`: spawn → banner output → second-client attach
//! (checkpoint) → live echo. This exercises the actual process boundary, not
//! just the in-process `DaemonServer` used by the library tests.

use capilot_ide_lib::daemon::client::{ClientError, DaemonClient};
use capilot_ide_lib::daemon::protocol::ClientEvent;
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
            &["-c".into(), "echo __READY__; while read x; do echo \"got:$x\"; done".into()],
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
    assert!(result.checkpoint.is_some(), "fresh attach needs a checkpoint");
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
            &["-c".into(), "echo __READY__; while read x; do echo \"got:$x\"; done".into()],
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
    assert_eq!(sessions[0].generation, generation, "same generation after reattach");
    assert_eq!(sessions[0].pid, pid, "same pid — no respawn");

    // GUI #2 attaches to the same generation and takes the lease.
    let result = c2.attach("keep1", generation, 24, 80, None).expect("attach");
    assert!(result.checkpoint.is_some(), "checkpoint rebuilds the live screen");
    c2.write("keep1", generation, "ping\n").expect("write");
    read_until(&c2, "got:ping", Duration::from_secs(5));

    // Clean shutdown only when the last GUI leaves for good.
    c2.shutdown().expect("shutdown");
    let status = child.wait().expect("wait for daemon");
    assert!(status.success(), "daemon exited abnormally: {status:?}");
    let _ = std::fs::remove_dir_all(&home);
}
