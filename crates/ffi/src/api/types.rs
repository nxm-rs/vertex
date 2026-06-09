//! Plain data shapes that cross the FFI boundary.
//!
//! These are the language-neutral inputs and outputs the bindings expose to a
//! host (Dart, Swift, Kotlin, C++). They carry raw bytes and primitives only;
//! the strong domain types (`StampedChunk`, `ChunkAddress`, `PushReceipt`) are
//! reconstructed immediately inside Rust and never leak across the boundary.

use flutter_rust_bridge::frb;

/// Which network the embedded client joins.
#[frb(non_opaque)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VertexNetwork {
    /// The Swarm mainnet.
    #[default]
    Mainnet,
    /// The Swarm testnet.
    Testnet,
    /// An isolated development network.
    Dev,
}

/// Configuration for building an embedded client.
///
/// The host supplies an optional signing key and the target network. An absent
/// key yields a random ephemeral identity, which is appropriate for a short
/// lived client that only uploads and downloads.
#[frb(non_opaque)]
#[derive(Debug, Clone, Default)]
pub struct VertexClientConfig {
    /// The network to join.
    pub network: VertexNetwork,
    /// Optional 32-byte secp256k1 private key. When `None`, the client runs with
    /// a random ephemeral identity.
    pub private_key: Option<Vec<u8>>,
    /// Optional multiaddrs of bootnodes to dial on startup. When empty, the
    /// network spec's defaults are used.
    pub bootnodes: Vec<String>,
}

/// A pre-stamped chunk to upload.
///
/// `address` and `stamp` are the chunk's 32-byte address and its 113-byte wire
/// stamp. `data` is the chunk's wire encoding (span plus payload). The chunk type
/// is inferred from `data` and `address` during reconstruction, so the host does
/// not pass it separately: an address that does not match the bytes is rejected.
#[frb(non_opaque)]
#[derive(Debug, Clone)]
pub struct VertexChunkUpload {
    /// The chunk's 32-byte address.
    pub address: Vec<u8>,
    /// The chunk's wire-encoded bytes (span plus payload).
    pub data: Vec<u8>,
    /// The chunk's 113-byte postage stamp.
    pub stamp: Vec<u8>,
    /// When true, the stamp signature is recovered before the chunk is pushed.
    /// When false, the chunk is pushed without stamp validation.
    pub validate: bool,
}

/// Proof that a storer accepted an uploaded chunk.
#[frb(non_opaque)]
#[derive(Debug, Clone)]
pub struct VertexPushReceipt {
    /// Overlay address of the accepting storer, hex-encoded.
    pub storer: String,
    /// The storer's signature over the receipt (65 bytes).
    pub signature: Vec<u8>,
    /// The nonce the storer used when signing (32 bytes).
    pub nonce: Vec<u8>,
    /// The storer's storage radius at the time of acceptance.
    pub storage_radius: u32,
}

/// A downloaded chunk and its postage stamp.
#[frb(non_opaque)]
#[derive(Debug, Clone)]
pub struct VertexChunkDownload {
    /// The chunk's wire-encoded bytes (span plus payload).
    pub data: Vec<u8>,
    /// The chunk's 113-byte postage stamp.
    pub stamp: Vec<u8>,
    /// Overlay address of the peer that served the chunk, hex-encoded.
    pub served_by: String,
}
