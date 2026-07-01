//! Client chunk providers: the network-backed retrieval/push provider both
//! client entry points expose as their RPC chunk surface.

mod providers;

pub use providers::NetworkChunkProvider;
