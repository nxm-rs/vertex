use alloy::primitives::{Address, PrimitiveSignature, B256};
use libp2p::Multiaddr;
use nectar_primitives_traits::SwarmAddress;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NodeAddressError {
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid signature: {0}")]
    InvalidAlloySignature(#[from] alloy::primitives::SignatureError),
    #[error("Signer error: {0}")]
    SignerError(#[from] alloy::signers::Error),
}

/// A NodeAddress trait that represents a node's address on the network.
pub trait NodeAddress<const SWARM: u64> {
    fn overlay_address(&self) -> SwarmAddress;
    fn underlay_address(&self) -> &Multiaddr;
    fn chain_address(&self) -> Address;
    fn nonce(&self) -> &B256;
    fn signature(&self) -> Result<PrimitiveSignature, NodeAddressError>;
}
