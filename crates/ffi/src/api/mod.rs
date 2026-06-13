//! The public FFI surface.
//!
//! Every item reachable from here is a candidate for binding generation by
//! flutter_rust_bridge. The module is the single source of truth for the C ABI:
//! the generated bindings (Dart, Swift, Kotlin, and the C header) are produced
//! from these signatures, never hand-maintained alongside them.

pub mod client;
pub mod error;
pub mod logging;
pub mod metrics;
pub mod types;
