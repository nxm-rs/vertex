use std::error::Error;
use vergen_gitcl::{BuildBuilder, CargoBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn Error>> {
    // Generate build information
    Emitter::default()
        .add_instructions(&BuildBuilder::default().build_timestamp(true).build()?)?
        .add_instructions(&CargoBuilder::default().features(true).build()?)?
        .add_instructions(
            &GitclBuilder::default()
                .sha(true)
                .describe(true, true, None)
                .build()?,
        )?
        .emit()?;

    Ok(())
}
