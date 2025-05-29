#![allow(missing_docs)]

use std::{env, error::Error};

use vergen_gitcl::{BuildBuilder, CargoBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn Error>> {
    let build = BuildBuilder::all_build()?;
    let cargo = CargoBuilder::all_cargo()?;
    let gitcl = GitclBuilder::all_git()?;

    // Emit the instructions
    Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&cargo)?
        .add_instructions(&gitcl)?
        .emit_and_set()?;

    let sha = env::var("VERGEN_GIT_SHA")?;
    let sha_short = &sha[0..7];

    let is_dirty = env::var("VERGEN_GIT_DIRTY")? == "true";
    // > git describe --always --tags
    // if not on a tag: v0.2.0-beta.3-82-g1939939b
    // if on a tag: v0.2.0-beta.3
    let not_on_tag = env::var("VERGEN_GIT_DESCRIBE")?.ends_with(&format!("-g{sha_short}"));
    let version_suffix = if is_dirty || not_on_tag { "-dev" } else { "" };
    println!("cargo:rustc-env=VERTEX_VERSION_SUFFIX={}", version_suffix);

    // Set short SHA
    println!("cargo:rustc-env=VERGEN_GIT_SHA_SHORT={}", &sha[..8]);

    // Set the build profile
    let out_dir = env::var("OUT_DIR").unwrap();
    let profile = out_dir.rsplit(std::path::MAIN_SEPARATOR).nth(3).unwrap();
    println!("cargo:rustc-env=VERTEX_BUILD_PROFILE={profile}");

    // Set formatted version strings
    let pkg_version = env!("CARGO_PKG_VERSION");

    // The short version information for vertex.
    // - The latest version from Cargo.toml
    // - The short SHA of the latest commit.
    // Example: 0.1.0 (defa64b2)
    println!("cargo:rustc-env=VERTEX_SHORT_VERSION={pkg_version}{version_suffix} ({sha_short})");

    // LONG_VERSION
    // The long version information for vertex.
    //
    // - The latest version from Cargo.toml + version suffix (if any)
    // - The full SHA of the latest commit
    // - The build datetime
    // - The build features
    // - The build profile
    //
    // Example:
    //
    // ```text
    // Version: 0.1.0
    // Commit SHA: defa64b2
    // Build Timestamp: 2023-05-19T01:47:19.815651705Z
    // Build Features: jemalloc
    // Build Profile: maxperf
    // ```
    println!("cargo:rustc-env=VERTEX_LONG_VERSION_0=Version: {pkg_version}{version_suffix}");
    println!("cargo:rustc-env=VERTEX_LONG_VERSION_1=Commit SHA: {sha}");
    println!(
        "cargo:rustc-env=VERTEX_LONG_VERSION_2=Build Timestamp: {}",
        env::var("VERGEN_BUILD_TIMESTAMP")?
    );
    println!(
        "cargo:rustc-env=VERTEX_LONG_VERSION_3=Build Features: {}",
        env::var("VERGEN_CARGO_FEATURES")?
    );
    println!("cargo:rustc-env=VERTEX_LONG_VERSION_4=Build Profile: {profile}");

    // The version information for vertex formatted for P2P (libp2p).
    // - The latest version from Cargo.toml
    // - The target triple
    //
    // Example: vertex/v0.1.0-alpha.1-428a6dc2f/aarch64-apple-darwin
    println!(
        "cargo:rustc-env=VERTEX_P2P_CLIENT_VERSION={}",
        format_args!(
            "vertex/v{pkg_version}-{sha_short}/{}",
            env::var("VERGEN_CARGO_TARGET_TRIPLE")?
        )
    );

    Ok(())
}
