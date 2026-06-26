use std::error::Error;

use vergen_gitcl::{Emitter, GitclBuilder};

fn main() {
    // Stamp the short commit sha into `VERGEN_GIT_SHA` for `--version`. The
    // Docker build context excludes `.git`, so a missing repository must not
    // fail the build: fall back to a stable placeholder instead.
    if emit_git_sha().is_err() {
        println!("cargo::rustc-env=VERGEN_GIT_SHA=unknown");
    }
}

fn emit_git_sha() -> Result<(), Box<dyn Error>> {
    let gitcl = GitclBuilder::default().sha(true).build()?;
    Emitter::default()
        .fail_on_error()
        .add_instructions(&gitcl)?
        .emit()?;
    Ok(())
}
