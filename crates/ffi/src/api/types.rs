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

/// One item in a streaming download: either a verified chunk or a per-address
/// failure.
///
/// The download stream yields exactly one of these per requested address, in
/// request order. A failed retrieval (peer miss, wrong bytes, no candidates)
/// arrives as an item with `error` set and `data` empty, so a host can decide
/// per address whether to abort or skip without tearing down the whole stream.
/// The payload is copied once here, at the boundary: inside Rust the chunk stays
/// `Bytes`.
#[frb(non_opaque)]
#[derive(Debug, Clone)]
pub struct VertexChunkData {
    /// Zero-based position of this item in the requested address list.
    pub index: u64,
    /// The chunk's 32-byte address.
    pub address: Vec<u8>,
    /// The chunk's wire-encoded bytes (span plus payload). Empty when `error`
    /// is set.
    pub data: Vec<u8>,
    /// The chunk's 113-byte postage stamp. Empty when `error` is set.
    pub stamp: Vec<u8>,
    /// A failure message when this address could not be served, otherwise
    /// `None`.
    pub error: Option<String>,
}

/// One acknowledgement in a streaming upload.
///
/// The upload stream yields exactly one of these per fed chunk, in feed order.
/// A successful push carries the storer's receipt; a failure (a bad chunk or a
/// rejected push) carries `error` and no receipt. The host pulls acks one at a
/// time; the pipeline admits a new push only as the host pulls, so a host that
/// stops pulling pauses the network pushes. Reconstruction of each chunk into its
/// strong type is also lazy under the pull, so the only chunks materialized at
/// once are the ones inside the byte window (the host still owns the input list
/// it passed, but Rust adds no second resident copy of it).
#[frb(non_opaque)]
#[derive(Debug, Clone)]
pub struct VertexUploadAck {
    /// Zero-based position of this ack in the fed chunk list.
    pub index: u64,
    /// The chunk's 32-byte address.
    pub address: Vec<u8>,
    /// The storer's receipt for a successful push. `None` when `error` is set.
    pub receipt: Option<VertexPushReceipt>,
    /// A failure message when the chunk could not be stored, otherwise `None`.
    pub error: Option<String>,
}

/// Tuning for a streaming download or upload.
///
/// `window_bytes` is the memory ceiling on outstanding payload, expressed in
/// bytes so the host sizes it against a real budget. `max_concurrency` caps
/// simultaneous in-flight requests on top of the byte window. Both are clamped
/// to at least one inside Rust, so a zero never deadlocks the stream.
#[frb(non_opaque)]
#[derive(Debug, Clone, Copy)]
pub struct VertexStreamConfig {
    /// Soft byte ceiling on outstanding in-flight payload.
    pub window_bytes: u64,
    /// Hard cap on simultaneous in-flight requests.
    pub max_concurrency: u32,
}
