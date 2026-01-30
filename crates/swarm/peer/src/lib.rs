//! Peer identity and addressing primitives for the Ethereum Swarm P2P network.
//!
//! - [`SwarmPeer`] - Canonical peer identity type
//! - Multiaddr serialization (Bee-compatible)
//! - Signature verification and overlay validation

mod error;
mod serde_multiaddr;
mod util;

pub use error::{MultiAddrError, SwarmPeerError};
pub use serde_multiaddr::{deserialize_multiaddrs, serialize_multiaddrs};
pub use util::arbitrary_multiaddr;

use util::generate_sign_message;
use vertex_swarm_api::Identity;
use vertex_swarm_primitives::compute_overlay;

pub use nectar_primitives::SwarmAddress;
pub use nectar_swarms::{NamedSwarm, Swarm, SwarmKind};
pub use vertex_swarm_primitives::SwarmNodeType;

use alloy_primitives::{Address, B256, Signature};
use alloy_signer::SignerSync;
use libp2p::Multiaddr;
use std::sync::Arc;

/// A Swarm peer's identity and addressing information.
///
/// Contains everything needed to identify, verify, and connect to a peer:
/// multiaddrs, signature, overlay address, nonce, and ethereum address.
///
/// Construct via [`with_signer`](Self::with_signer) for local identity,
/// [`from_signed`](Self::from_signed) for received protocol data, or
/// [`from_validated`](Self::from_validated) for pre-validated data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwarmPeer {
    multiaddrs: Vec<Multiaddr>,
    signature: Signature,
    overlay: SwarmAddress,
    nonce: B256,
    ethereum_address: Address,
}

impl SwarmPeer {
    /// Create a `SwarmPeer` for the local node by signing with the provided signer.
    pub fn with_signer<S: alloy_signer::Signer + SignerSync>(
        multiaddrs: Vec<Multiaddr>,
        nonce: B256,
        network_id: u64,
        signer: Arc<S>,
    ) -> Result<Self, SwarmPeerError> {
        if multiaddrs.is_empty() {
            return Err(SwarmPeerError::NoMultiaddrs);
        }

        let ethereum_address = signer.address();
        let overlay = compute_overlay(&ethereum_address, network_id, &nonce);

        let multiaddr_bytes = serialize_multiaddrs(&multiaddrs);
        let msg = generate_sign_message(&multiaddr_bytes, &overlay, network_id);
        let signature = signer.sign_message_sync(&msg)?;

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            ethereum_address,
        })
    }

    /// Create a `SwarmPeer` from an [`Identity`] and observed multiaddrs.
    pub fn from_identity<I: Identity>(
        identity: &I,
        multiaddrs: Vec<Multiaddr>,
    ) -> Result<Self, SwarmPeerError> {
        use vertex_swarmspec::SwarmSpec;
        Self::with_signer(
            multiaddrs,
            identity.nonce(),
            identity.spec().network_id(),
            identity.signer(),
        )
    }

    /// Create a `SwarmPeer` from protocol data, recovering the ethereum address from signature.
    ///
    /// Empty multiaddrs are allowed for inbound-only peers (browsers, WebRTC, NAT-restricted).
    pub fn from_signed(
        multiaddrs_bytes: &[u8],
        signature: Signature,
        overlay: SwarmAddress,
        nonce: B256,
        network_id: u64,
        validate_overlay: bool,
    ) -> Result<Self, SwarmPeerError> {
        let multiaddrs = deserialize_multiaddrs(multiaddrs_bytes)?;
        let ethereum_address = recover_signer(multiaddrs_bytes, &overlay, &signature, network_id)?;

        if validate_overlay {
            let expected_overlay = compute_overlay(&ethereum_address, network_id, &nonce);
            if expected_overlay != overlay {
                return Err(SwarmPeerError::InvalidOverlay);
            }
        }

        Ok(Self {
            multiaddrs,
            signature,
            overlay,
            nonce,
            ethereum_address,
        })
    }

    /// Create a `SwarmPeer` from pre-validated data (no verification performed).
    pub fn from_validated(
        multiaddrs: Vec<Multiaddr>,
        signature: Signature,
        overlay: B256,
        nonce: B256,
        ethereum_address: Address,
    ) -> Self {
        Self {
            multiaddrs,
            signature,
            overlay: SwarmAddress::from(overlay),
            nonce,
            ethereum_address,
        }
    }

    pub fn multiaddrs(&self) -> &[Multiaddr] {
        &self.multiaddrs
    }

    pub fn multiaddr(&self) -> Option<&Multiaddr> {
        self.multiaddrs.first()
    }

    /// Returns true if this peer has dialable addresses.
    pub fn is_dialable(&self) -> bool {
        !self.multiaddrs.is_empty()
    }

    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    pub fn overlay(&self) -> &SwarmAddress {
        &self.overlay
    }

    pub fn nonce(&self) -> &B256 {
        &self.nonce
    }

    pub fn ethereum_address(&self) -> &Address {
        &self.ethereum_address
    }

    pub fn serialize_multiaddrs(&self) -> Vec<u8> {
        serialize_multiaddrs(&self.multiaddrs)
    }
}

/// Recover the signer's ethereum address from signature and message components.
fn recover_signer(
    multiaddr_bytes: &[u8],
    overlay: &SwarmAddress,
    signature: &Signature,
    network_id: u64,
) -> Result<Address, SwarmPeerError> {
    let prehash = generate_sign_message(multiaddr_bytes, overlay, network_id);
    Ok(signature.recover_address_from_msg(prehash)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;

    const TEST_NETWORK_ID: u64 = 1;

    #[test]
    fn swarm_peer_roundtrip() {
        let signer = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer1 = SwarmPeer::with_signer(
            vec![multiaddr.clone()],
            nonce,
            TEST_NETWORK_ID,
            signer.clone(),
        )
        .unwrap();

        let multiaddr_bytes = peer1.serialize_multiaddrs();
        let peer2 = SwarmPeer::from_signed(
            &multiaddr_bytes,
            peer1.signature().clone(),
            *peer1.overlay(),
            nonce,
            TEST_NETWORK_ID,
            true,
        )
        .unwrap();

        assert_eq!(peer1, peer2);
    }

    #[test]
    fn signature_recovery() {
        let signer = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer = SwarmPeer::with_signer(vec![multiaddr], nonce, TEST_NETWORK_ID, signer.clone())
            .unwrap();

        let multiaddr_bytes = peer.serialize_multiaddrs();
        let recovered = recover_signer(
            &multiaddr_bytes,
            peer.overlay(),
            peer.signature(),
            TEST_NETWORK_ID,
        )
        .unwrap();

        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn invalid_overlay_rejected() {
        let signer1 = Arc::new(PrivateKeySigner::random());
        let signer2 = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer1 =
            SwarmPeer::with_signer(vec![multiaddr], nonce, TEST_NETWORK_ID, signer1).unwrap();

        let overlay2 = compute_overlay(&signer2.address(), TEST_NETWORK_ID, &nonce);

        let result = SwarmPeer::from_signed(
            &peer1.serialize_multiaddrs(),
            peer1.signature().clone(),
            overlay2,
            nonce,
            TEST_NETWORK_ID,
            true,
        );

        assert!(matches!(result, Err(SwarmPeerError::InvalidOverlay)));
    }

    #[test]
    fn different_networks_different_overlays() {
        let signer = Arc::new(PrivateKeySigner::random());
        let nonce = B256::ZERO;
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();

        let peer1 =
            SwarmPeer::with_signer(vec![multiaddr.clone()], nonce, 1, signer.clone()).unwrap();
        let peer2 =
            SwarmPeer::with_signer(vec![multiaddr.clone()], nonce, 2, signer.clone()).unwrap();
        let peer3 = SwarmPeer::with_signer(vec![multiaddr], nonce, 100, signer).unwrap();

        assert_ne!(peer1.overlay(), peer2.overlay());
        assert_ne!(peer1.overlay(), peer3.overlay());
        assert_ne!(peer2.overlay(), peer3.overlay());
    }
}
