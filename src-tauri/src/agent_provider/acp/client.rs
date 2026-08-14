//! ACP stdio child process + message demux (architecture §8.1).
//!
//! ACP uses the shared JSON-RPC-over-NDJSON transport ([`crate::agent_provider::rpc_stdio`]);
//! this module is a thin alias so the rest of the ACP adapter (and the docs
//! that reference `AcpConnection`) keep a stable, ACP-named surface. The actual
//! spawn/demux lives in the shared transport.

pub use crate::agent_provider::rpc_stdio::RpcConnection as AcpConnection;
pub use crate::agent_provider::rpc_stdio::{Inbound, RpcError, CLOSE_TIMEOUT};
