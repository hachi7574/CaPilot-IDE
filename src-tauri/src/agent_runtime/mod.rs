pub mod adapter;
pub mod cat_breeds;
pub mod pty;
pub mod runtimes;
pub mod status_hooks;

/// Serializes tests that repoint the process-global `HOME` / `CODEX_HOME`
/// (or any env the runtime reads) so parallel test modules never observe each
/// other's env. Every test module that mutates env must lock this same mutex —
/// a module-local lock is NOT enough, since lib.rs and the runtime modules all
/// touch the same process-global variables.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
