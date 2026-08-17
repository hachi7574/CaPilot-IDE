//! Daemon-mode process entry (§4.1). The GUI spawns `current_exe() --daemon`;
//! this module binds the instance lock + socket and runs the accept loop until
//! a `Shutdown` request arrives (Phase 2: the GUI explicitly closes the daemon
//! it spawned — §9.2). No Tauri is initialized in this mode.
//!
//! The binary stays the same file so it is trivially sidecar-packaged by Tauri
//! (`bundle.externalBin` picks up the app binary); a separate Cargo target
//! would need explicit packaging work (§4.1).

use crate::daemon::runtime::socket_path;
use crate::daemon::server::{DaemonConfig, DaemonError, DaemonServer};
use std::path::PathBuf;
use std::process;

/// Application version shared with the GUI for the version handshake. The GUI
/// uses the same Cargo package version via `env!("CARGO_PKG_VERSION")`.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Standard CaPilot data root — parent of `sessions.db`, `workspaces/` and
/// `run/`. Mirrors `Persistence::open()` / `SessionStore::open()` so daemon and
/// GUI cannot drift, but without opening the DB (the GUI bridge calls this at
/// every startup). See [`crate::persistence::data_root`].
pub fn daemon_base() -> PathBuf {
    crate::persistence::data_root()
}

/// Run the daemon until shutdown. Never returns normally except via exit.
pub fn run_daemon_mode() {
    // Phase 4 (§9.4): the daemon is a resident service. Ignore SIGHUP so a
    // terminal hangup (the GUI that spawned us was launched from one, or the
    // user logged out of that session) can't take the sessions down with it.
    // The GUI additionally spawns us in our own process group, so we don't even
    // receive the GUI's group-level SIGHUP; this covers a direct terminal
    // start too.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    let base = daemon_base();
    let server = match DaemonServer::bind(DaemonConfig {
        base: base.clone(),
        app_version: APP_VERSION.to_string(),
    }) {
        Ok(s) => s,
        Err(DaemonError::AlreadyRunning) => {
            // Another daemon owns the lock and PTY set. Nothing to do — the GUI
            // will connect to it. Exit quietly (this is not an error state).
            process::exit(0)
        }
        Err(e) => {
            eprintln!("capilot daemon: bind failed at {}: {e}", base.display());
            process::exit(1)
        }
    };

    eprintln!(
        "capilot daemon: listening on {} (instance {})",
        socket_path(&base).display(),
        server.instance_id()
    );

    // Blocks until a Shutdown request, then kills PTYs and returns.
    server.run();
}
