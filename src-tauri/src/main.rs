// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Daemon mode: the GUI spawns `current_exe() --daemon`. Run the PTY daemon
    // headless (no Tauri) until a Shutdown request arrives (§4.1, §9.2).
    if std::env::args().any(|a| a == "--daemon") {
        capilot_ide_lib::daemon::bin::run_daemon_mode();
        return;
    }
    capilot_ide_lib::run()
}
