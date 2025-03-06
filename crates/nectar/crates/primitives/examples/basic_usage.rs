//! Basic usage example for the primitives crate

use alloy_primitives::hex;
use bytes::{Bytes, BytesMut};
use nectar_access_control::CredentialBase;
use nectar_primitives_new::{
    bmt::BMTHasher,
    chunk::ChunkData,
    error::Result,
    storage::{NoopStorageController, PostageStamp, StorageController, StorageCredential},
};

fn main() -> Result<()> {
    // Create some test data
    let mut data = BytesMut::with_capacity(1024);
    // Add 8-byte span header (the span value in little-endian bytes)
    let span: u64 = 1016; // length of payload
    data.extend_from_slice(&span.to_le_bytes());
    // Add some payload data
    data.extend_from_slice(&[0u8; 1016]);
    let bytes = data.freeze();

    // Create a BMT hasher and compute the chunk address
    println!("Creating a new chunk...");
    let mut hasher = BMTHasher::new();
    hasher.set_span(span);
    let address = hasher.chunk_address(&bytes)?;
    println!("Calculated chunk address: {:?}", address);

    // Create a chunk directly using ChunkData and content addressed type
    let chunk = ChunkData::deserialize(bytes.clone(), false)?;
    println!("Created chunk with address: {:?}", chunk.address());

    // Verify the chunk
    println!("Verifying chunk integrity...");
    chunk.verify_integrity()?;
    println!("Chunk integrity verified successfully");

    // Create a postage stamp
    let batch_id = [1u8; 32];
    let owner = [2u8; 20];
    let stamp = PostageStamp::new(
        [3u8; 32], // id
        batch_id,
        owner,
        8,                                    // depth
        1000,                                 // amount
        None,                                 // no expiration
        Bytes::from_static(&[4u8, 5u8, 6u8]), // arbitrary data
    );

    println!(
        "Created postage stamp with id: {:?}",
        hex::encode(&stamp.id()[..8])
    );
    println!("Batch ID: {:?}", hex::encode(&stamp.batch_id()[..8]));
    println!("Owner: {:?}", hex::encode(&stamp.owner()));
    println!("Depth: {}", stamp.depth());
    println!("Amount: {}", stamp.amount());

    // Store the chunk using a storage controller
    let controller = NoopStorageController;
    println!("Processing storage with postage stamp...");
    controller.process_storage(&chunk, &stamp)?;
    println!("Storage processed successfully");

    // Check if we should store the chunk
    let should_store = controller.should_store_chunk(&chunk, &stamp);
    println!("Should this node store the chunk? {}", should_store);

    // Serialize the chunk
    let serialized = chunk.serialize(true);
    println!("Serialized chunk size: {} bytes", serialized.len());

    // Parse the chunk back
    let parsed_chunk = ChunkData::deserialize(serialized, true)?;
    println!("Parsed chunk address: {:?}", parsed_chunk.address());

    // Verify the parsed chunk has the same address
    assert_eq!(
        chunk.address(),
        parsed_chunk.address(),
        "Addresses don't match!"
    );
    println!("Original and parsed chunk addresses match!");

    Ok(())
}
