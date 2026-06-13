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
//! [`api::metrics::init_metrics`] installs a metrics recorder whose state the
//! host reads back on demand through [`api::metrics::metrics_snapshot`].
//!
//! # Bindings
//!
//! The C ABI and the per-language bindings (Dart, Swift, Kotlin) are generated
//! from the [`api`] module by flutter_rust_bridge; the [`frb_generated`] module
//! and the `bindings/` tree hold the committed codegen output. The signatures
//! in [`api`] are the single source of truth: there is no hand-maintained
//! parallel C header. After changing [`api`], regenerate with the
//! flutter_rust_bridge codegen against `flutter_rust_bridge.yaml` and commit
//! the output; the generated glue is plain Rust, so a checkout builds and
//! tests without the codegen binary installed.
//!
//! # Native only
//!
//! The cdylib is a native artifact. The browser embedding path is wasm-bindgen,
//! a separate surface, so this crate's runtime-bearing pieces are gated to
//! non-wasm targets and the wasm cone never pulls them in.

pub mod api;

// The generated glue declares `pub` items behind this private module and
// unwraps where the bridge invariants hold; allow both here rather than
// touching generated code (the codegen owns the file).
#[allow(unreachable_pub, clippy::unwrap_used)]
mod frb_generated;

// The error module lives under `api` so the binding codegen resolves
// `FfiResult` to a plain `Result` and surfaces `FfiError` as a typed host
// exception; `crate::error` stays valid as a path for Rust consumers.
pub use api::error;
pub use api::error::{FfiError, FfiResult};
