//! Client chunk providers: the network-backed retrieval/push provider and the
//! config-gated download verification wrapper both client entry points expose as
//! their RPC chunk surface.

mod providers;
mod verify;

pub use providers::NetworkChunkProvider;
pub use verify::{ChunkVerifyConfig, VerifyingChunkProvider};
