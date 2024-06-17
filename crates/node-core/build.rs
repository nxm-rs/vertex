#![allow(missing_docs)]

use std::{env, error::Error};
use vergen::EmitBuilder;

fn main() -> Result<(), Box<dyn Error>> {
    // Emit the instructions
    EmitBuilder::builder()
        .git_describe(false, true, None)
        .git_dirty(true)
        .git_sha(true)
        .build_timestamp()
        .cargo_features()
        .cargo_target_triple()
        .emit_and_set()?;

    let sha = env::var("VERGEN_GIT_SHA")?;

    let is_dirty = env::var("VERGEN_GIT_DIRTY")? == "true";
    let not_on_tag = env::var("VERGEN_GIT_DESCRIBE")?.ends_with(&format!("-g{sha}"));
    let is_dev = is_dirty || not_on_tag;
    println!("cargo:rustc-env=BEERS_VERSION_SUFFIX={}", if is_dev { "-dev" } else { "" });
    Ok(())
}
