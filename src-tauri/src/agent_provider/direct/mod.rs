//! Direct provider adapters (architecture §8.2).
//!
//! Direct adapters speak a provider's *native* protocol — there is no generic
//! ACP layer in between. Phase 4 ships the Codex `app-server` JSON-RPC adapter
//! ([`codex`]); OpenCode is served by the generic ACP adapter instead, so no
//! OpenCode Direct adapter is needed yet.

pub mod claude;
pub mod codex;
pub use claude::{claude_profile, ClaudeClient, ClaudeProfile};
pub use codex::{codex_profile, CodexClient, CodexProfile};
