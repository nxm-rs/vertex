use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .file_descriptor_set_path(out_dir.join("swarm_descriptor.bin"))
        .compile_protos(&["proto/node.proto", "proto/chunk.proto"], &["proto"])?;

    Ok(())
}
