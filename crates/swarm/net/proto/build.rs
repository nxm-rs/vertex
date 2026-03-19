#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_rs::{ConfigBuilder, types::FileDescriptor};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo");
    let out_dir = Path::new(&out_dir).join("proto");

    let in_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    )
    .join("proto");

    println!(
        "cargo:rerun-if-changed={}",
        in_dir.to_str().expect("proto dir path must be valid UTF-8")
    );

    let mut protos = Vec::new();
    let proto_ext = Some(Path::new("proto").as_os_str());
    for entry in WalkDir::new(&in_dir) {
        let path = entry
            .expect("failed to read proto directory entry")
            .into_path();
        if path.extension() == proto_ext {
            println!(
                "cargo:rerun-if-changed={}",
                path.to_str().expect("proto file path must be valid UTF-8")
            );
            protos.push(path);
        }
    }

    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).expect("failed to clean proto output directory");
    }
    std::fs::DirBuilder::new()
        .create(&out_dir)
        .expect("failed to create proto output directory");

    let config_builder = ConfigBuilder::new(&protos, None, Some(&out_dir), &[in_dir])
        .expect("failed to configure protobuf builder");
    FileDescriptor::run(&config_builder.dont_use_cow(true).build())
        .expect("failed to generate protobuf code");
}
