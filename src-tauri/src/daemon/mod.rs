//! PTY daemon + GUI bridge (§2). The daemon owns `pty_core` out of process; the
//! GUI bridge (in `bridge.rs`) either talks to it over a Unix socket or falls
//! back to the in-process `PtyCore` (§8). The protocol and runtime layout are
//! shared, so the daemon binary and the GUI cannot drift.

pub mod bin;
pub mod client;
pub mod protocol;
pub mod runtime;
pub mod server;
pub mod vt_checkpoint;
