//! Re-export of the shared chain provider seam.
//!
//! The transport-portable constructor lives in `vertex-chain` so it builds on
//! `wasm32` for the browser client; the builder re-exports it here, and the
//! launch path resolves the config and spec before calling into it.

pub use vertex_chain::{SharedChainProvider, build_chain_provider};
