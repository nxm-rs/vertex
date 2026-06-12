//! Generated flutter_rust_bridge glue.
//!
//! This file is normally produced by the flutter_rust_bridge codegen from the
//! signatures under [`crate::api`]. It is committed as a minimal placeholder so
//! a plain `cargo build -p vertex-ffi` succeeds without running the external
//! codegen binary. Running the codegen (see `flutter_rust_bridge.yaml`)
//! overwrites this file with the real C ABI dispatch table, the wire codecs, and
//! the `StreamSink` type the host bindings call into.
//!
//! Do not hand-edit the regenerated contents; edit the API under [`crate::api`]
//! and regenerate.
//!
//! The codegen emits a `StreamSink<T>` type here (via flutter_rust_bridge's
//! `frb_generated_stream_sink!` macro) that the streaming API signatures in
//! [`crate::api`] reference. So those signatures compile before the codegen has
//! ever run, the placeholder below stands in a structurally compatible
//! `StreamSink<T>`: an opaque handle with an `add` method that is a no-op until
//! the codegen replaces it with the real Dart-backed sink. A host that has not
//! run the codegen gets a node that logs nowhere; a host that has gets the live
//! Dart stream.

use std::marker::PhantomData;

/// Placeholder stand-in for the codegen-emitted `StreamSink<T>`.
///
/// Carries no channel: [`StreamSink::add`] drops its value. The flutter_rust_bridge
/// codegen overwrites this type with one backed by a Dart message port, at which
/// point `add` delivers each value to the host stream.
pub struct StreamSink<T> {
    _phantom: PhantomData<T>,
}

impl<T> StreamSink<T> {
    /// Push a value to the host stream.
    ///
    /// The placeholder discards the value and reports success; the codegen
    /// replacement delivers it to Dart and reports a closed-stream send error.
    pub fn add(&self, _value: T) -> Result<(), StreamSinkError> {
        Ok(())
    }
}

/// Placeholder stand-in for the codegen send-error type.
#[derive(Debug)]
pub struct StreamSinkError;
