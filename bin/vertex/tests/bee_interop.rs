//! Bee-vs-vertex cross-implementation handshake harness.
//!
//! This test is the foundation for the eventual end-to-end interop CI job:
//! it boots a real `bee` node (built locally from `/code/nxm/swarm/bee`)
//! as a bootnode on a hermetic loopback port, then spawns a vertex client
//! and asserts the handshake succeeds within 10 seconds.
//!
//! The wiring is intentionally minimal: the test is `#[ignore]`d so it is
//! only ever run on demand via:
//!
//! ```text
//! BEE_INTEROP=1 cargo test --include-ignored --test bee_interop -- bee_vertex_handshake
//! ```
//!
//! Without `BEE_INTEROP=1` the body short-circuits so that contributors
//! who run `cargo test --include-ignored` locally don't accidentally
//! trigger a multi-minute Go build.
//!
//! The handshake-itself assertion is left as a `todo!()` until vertex's
//! libp2p stack exposes a programmatic "dial + complete handshake"
//! helper. See [`spawn_bee_bootnode`] for the existing bee-side process
//! plumbing this test will hook into once that helper lands.

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Repo-relative path to the bee Go module (resolved at runtime).
const BEE_REPO: &str = "/code/nxm/swarm/bee";

/// Build the bee binary in-place. Returns the absolute path to the built
/// binary on success.
///
/// Uses `Command::current_dir` rather than shell `cd` because this test is
/// invoked from arbitrary cargo working directories.
fn build_bee_binary() -> std::io::Result<std::path::PathBuf> {
    let status = Command::new("make")
        .arg("binary")
        .current_dir(BEE_REPO)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "bee `make binary` failed with status {status}",
        )));
    }
    Ok(Path::new(BEE_REPO).join("dist").join("bee"))
}

/// Tests are opt-in: returns `true` when the caller has explicitly set
/// `BEE_INTEROP=1` *and* the bee checkout exists.
fn interop_enabled() -> bool {
    if std::env::var_os("BEE_INTEROP").is_none_or(|v| v != "1") {
        return false;
    }
    Path::new(BEE_REPO).join("Makefile").exists()
}

/// Spawn a bee bootnode listening on a hermetic loopback port.
///
/// Returns the child handle plus the chosen API port so the caller can
/// query bee for its libp2p address and pass it to a vertex client.
///
/// NOTE: This is the wiring stub that the full interop test will build
/// on. The real implementation needs to allocate a free port pair (P2P +
/// API), template a minimal bee config, and write a temporary password
/// file; that work will land alongside the vertex-side dial helper.
#[allow(dead_code, reason = "scaffold for the as-yet-unfinished interop test")]
fn spawn_bee_bootnode(_bee_bin: &Path) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::other(
        "spawn_bee_bootnode not yet implemented; pending vertex-side dial helper",
    ))
}

#[test]
#[ignore = "requires local bee checkout + Go toolchain; opt in with BEE_INTEROP=1"]
fn bee_vertex_handshake() {
    if !interop_enabled() {
        eprintln!(
            "bee_interop: skipping (BEE_INTEROP != 1 or bee checkout missing at {BEE_REPO})",
        );
        return;
    }

    let bee_bin = build_bee_binary().expect("build bee binary");
    assert!(
        bee_bin.is_file(),
        "expected bee binary at {} after `make binary`",
        bee_bin.display(),
    );

    // The remaining pieces (port allocation, bee bootnode config, vertex
    // client dial, 10s handshake assertion) land alongside the vertex-side
    // dial helper. Until then we deliberately fail loudly when invoked
    // with `BEE_INTEROP=1` so the gap is visible.
    let _handshake_deadline = Duration::from_secs(10);
    panic!(
        "bee_interop: BEE_INTEROP=1 invoked the handshake assertion, but the \
         vertex-side dial helper is not yet wired up (see spawn_bee_bootnode)",
    );
}
