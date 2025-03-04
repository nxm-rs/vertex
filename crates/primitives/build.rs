use alloy_primitives::Keccak256;

fn main() {
    const DEPTH: usize = 8;
    const SEGMENT_SIZE: usize = 32;
    let mut zero_hashes = vec![[0u8; SEGMENT_SIZE]; DEPTH];

    zero_hashes[0] = [0u8; SEGMENT_SIZE];
    for i in 1..DEPTH {
        let mut hasher = Keccak256::new();
        hasher.update(zero_hashes[i - 1]);
        hasher.update(zero_hashes[i - 1]);

        zero_hashes[i].copy_from_slice(hasher.finalize().as_slice());
    }

    // Generate Rust code for the constant
    let code = format!(
        "pub(crate) const ZERO_HASHES: [[u8; {SEGMENT_SIZE}]; {DEPTH}] = {:?};",
        zero_hashes
    );

    // Write the generated code to a file
    std::fs::write("src/bmt/zero_hashes.rs", code).expect("Failed to write zero_hashes.rs");

    // Ensure the build script is re-run if it changes
    println!("cargo:rerun-if-changed=build.rs");
}
