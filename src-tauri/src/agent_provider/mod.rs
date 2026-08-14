//! Provider-neutral agent runtime (architecture §5–§10).
//!
//! `types`    — the unified domain model (capabilities, catalog, events,
//!              timeline, permissions, persistence handle, status).
//! `traits`   — `AgentClient`/`AgentSession` adapter contracts.
//! `timeline` — the per-agent canonical timeline store.
//! `manager`  — `AgentManager`, the single authority for agent state.
//!
//! This module has no Tauri UI dependency. The daemon owns an [`AgentManager`]
//! and forwards structured requests; the GUI consumes snapshots + events.

pub mod acp;
pub mod direct;
pub mod manager;
pub mod rpc_stdio;
pub mod timeline;
pub mod traits;
pub mod types;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
