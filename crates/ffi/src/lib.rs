//! Native FFI surface for embedding a Vertex Swarm client.
//!
//! This crate is the primary public surface for embedding Vertex into a native
//! host: Dart and Flutter, Swift, Kotlin and JNI, C++, and other native
//! runtimes. It exposes a single embeddable client that can join a Swarm
//! network and upload and download chunks, with no HTTP or JSON anywhere on the
//! path.
//!
//! # Surface
//!
//! The public API lives entirely under [`api`]. [`api::client::VertexClient`] is
//! the embeddable handle; the host builds one from an [`api::types`] config,
//! then drives uploads and downloads through it. Inputs and outputs cross the
//! boundary as flat byte vectors, strings, and primitives; the strong domain
//! types are reconstructed immediately inside Rust and never leak outward.
//! [`api::logging::init_logging`] installs a tracing subscriber that streams the
//! node's diagnostics to the host as typed [`api::logging::LogLine`] events.
//!
//! # Bindings
//!
//! The C ABI and the per-language bindings (Dart, Swift, Kotlin) are generated
//! from the [`api`] module by flutter_rust_bridge; the [`frb_generated`] module
//! holds the generated glue. The signatures in [`api`] are the single source of
//! truth: there is no hand-maintained parallel C header. Regenerate with the
//! flutter_rust_bridge codegen against `flutter_rust_bridge.yaml`. The crate
//! compiles without running the codegen step, so a checkout builds and tests on
//! its own; only a host that actually invokes the bindings needs the regenerated
//! glue.
//!
//! # Native only
//!
//! The cdylib is a native artifact. The browser embedding path is wasm-bindgen,
//! a separate surface, so this crate's runtime-bearing pieces are gated to
//! non-wasm targets and the wasm cone never pulls them in.

pub mod api;
pub mod error;

mod frb_generated;

pub use error::{FfiError, FfiResult};
