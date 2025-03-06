use std::error::Error;
use vergen::EmitBuilder;

fn main() -> Result<(), Box<dyn Error>> {
    // Generate build information
    EmitBuilder::builder()
        .git_describe(true, true, None)
        .git_sha(false)
        .git_commit_timestamp()
        .cargo_features()
        .build_timestamp()
        .emit()?;

    // Extract Git SHA
    let sha = std::env::var("VERGEN_GIT_SHA_SHORT").unwrap_or_else(|_| "unknown".to_string());

    // Get version with suffix for development builds
    let version = env!("CARGO_PKG_VERSION");
    let is_dirty = std::env::var("VERGEN_GIT_DIRTY").unwrap_or_default() == "true";
    let version_suffix = if is_dirty { "-dev" } else { "" };

    // Set environment variables for the build
    println!("cargo:rustc-env=VERTEX_VERSION_SUFFIX={}", version_suffix);
    println!("cargo:rustc-env=VERTEX_GIT_SHA={}", sha);
    println!(
        "cargo:rustc-env=VERTEX_VERSION={}{}",
        version, version_suffix
    );

    Ok(())
}
