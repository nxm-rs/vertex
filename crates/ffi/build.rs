//! Build script for the Vertex client FFI crate.
//!
//! Registers the `frb_expand` cfg that the flutter_rust_bridge `#[frb]` macro
//! emits, so the workspace `unexpected_cfgs` lint stays clean without disabling
//! it crate-wide. The codegen sets this cfg when it expands the API for binding
//! generation; a normal build never sets it.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(frb_expand)");
}
