use alloy::{
    hex, primitives::{Address, FixedBytes}, signers::{wallet::LocalWallet, Signature, Signer}
};
use libp2p::Multiaddr;
use once_cell::sync::Lazy;
use overlay::{Nonce, Overlay, OverlayAddress};
use thiserror::Error;

mod distance;
mod overlay;
mod proximity;

// const STAMP_INDEX_SIZE: u8 = 8;
// const STAMP_TIMESTAMP_SIZE: u8 = 8;
// const SPAN_SIZE: u16 = 8;
// const SECTION_SIZE: u16 = 32;
// const BRANCHES: u16 = 128;
// const ENCRYPTED_BRANCHES: u16 = BRANCHES / 2;
// const BMT_BRANCHES: u8 = 128;
// const CHUNK_SIZE: u16 = SECTION_SIZE * BRANCHES;
const HASH_SIZE: usize = 32;
const MAX_PO: u8 = 31;
const EXTENDED_PO: u8 = MAX_PO + 5;
// const MAX_BINS: u8 = MAX_PO + 1;
// const CHUNK_WITH_SPAN_SIZE: u16 = CHUNK_SIZE + SPAN_SIZE;
// const SOC_SIGNATURE_SIZE: u16 = 65;
// const SOC_MIN_CHUNK_SIZE: u16 = HASH_SIZE + SOC_SIGNATURE_SIZE + SPAN_SIZE;
// const SOC_MAX_CHUNK_SIZE: u16 = SOC_MIN_CHUNK_SIZE + CHUNK_SIZE;

// NodeAddress represents the culminated address of a node in Swarm space.
// It consists of:
// - underlay(s) (physical) addresses
// - overlay (topological) address
// - signature
// - nonce

// It consists of a peers underlay (physical) address, overlay (topology) address and signature.
// The signature is used to verify the `Overlay/Underlay` pair, as it is based on `underlay|networkid`,
// signed with the private key of the chain address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAddress {
    underlay: Multiaddr,
    overlay: DistAddr,
    chain: Address,
}

#[derive(Debug, Error)]
pub enum NodeAddressError {
    #[error("signature length mismatch")]
    SignatureLengthMismatch,
    #[error("overlay mismatch")]
    OverlayMismatch,
    #[error("signature mismatch")]
    SignatureMismatch,
    #[error("underlay decode failed")]
    UnderlayDecodeFailed,
}

impl NodeAddress {
    pub async fn new(
        signer: LocalWallet,
        network_id: u64,
        nonce: Nonce,
        underlay: Multiaddr,
    ) -> (Self, Signature) {
        let underlay_binary = underlay.to_vec();

        let overlay = signer.overlay(network_id, Some(nonce));
        let message = generate_sign_data(underlay_binary.as_slice(), &overlay, network_id);
        let signature = signer.sign_message(&message).await;

        match signature {
            Ok(signature) => (
                Self {
                    underlay,
                    overlay,
                    chain: signer.address(),
                },
                signature,
            ),
            Err(_) => panic!("signature error"),
        }
    }

    pub fn parse(
        underlay: &[u8],
        overlay: &[u8],
        signature: &[u8],
        nonce: &[u8],
        validate_overlay: bool,
        network_id: u64,
    ) -> Result<Self, NodeAddressError> {
        let overlay = OverlayAddress::from_slice(overlay);
        let message = generate_sign_data(
            underlay,
            &OverlayAddress::from_slice(overlay.as_slice()),
            network_id,
        );
        let signature = Signature::try_from(signature)
            .map_err(|_| NodeAddressError::SignatureLengthMismatch)?;
        let chain = signature
            .recover_address_from_msg(message)
            .map_err(|_| NodeAddressError::SignatureMismatch)?;

        if validate_overlay {
            let recovered_overlay = chain.overlay(network_id, Some(Nonce::from_slice(nonce)));

            if overlay != recovered_overlay {
                return Err(NodeAddressError::OverlayMismatch);
            }
        }

        let underlay = Multiaddr::try_from(Vec::from(underlay))
            .map_err(|_| NodeAddressError::UnderlayDecodeFailed)?;

        Ok(Self {
            underlay,
            overlay,
            chain,
        })
    }

    pub fn underlay(&self) -> &Multiaddr {
        &self.underlay
    }

    pub fn overlay(&self) -> &DistAddr {
        &self.overlay
    }

    pub fn chain(&self) -> &Address {
        &self.chain
    }
}

fn generate_sign_data(underlay: &[u8], overlay: &OverlayAddress, network_id: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(underlay.len() + HASH_SIZE + 8 + 14);
    data.extend_from_slice("bee-handshake-".as_bytes());
    data.extend_from_slice(underlay);
    data.extend_from_slice(overlay.as_slice());
    data.extend_from_slice(&network_id.to_be_bytes());

    data
}

// A distance address represents an address in Swarm space in the domain of nodes, or chunks, whereby
// the consumer needs to be aware of their topological distance between distance addresses.
type DistAddr = FixedBytes<HASH_SIZE>;

pub static REPLICAS_OWNER: Lazy<Address> = Lazy::new(|| {
    Address::parse_checksummed("0xDC5b20847F43d67928F49Cd4f85D696b5A7617B5", None).unwrap()
});
pub const ZERO_ADDRESS: DistAddr = DistAddr::new([0; 32]);

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{bytes, FixedBytes},
        signers::wallet::LocalWallet,
    };

    use super::*;

    #[tokio::test]
    async fn test_node_address() {
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/1634/p2p/16Uiu2HAkx8ULY8cTXhdVAcMmLcH9AsTKz6uBQ7DPLKRjMLgBVYkA".parse().unwrap();
        let nonce = FixedBytes::<32>::left_padding_from(&bytes!("02"));
        let network_id: u64 = 3;
        let wallet = LocalWallet::random();

        let overlay = wallet.overlay(network_id, Some(nonce));
        let (node_address, signature) = NodeAddress::new(wallet, network_id, nonce, multiaddr.clone()).await;

        let node_address2 = NodeAddress::parse(
            multiaddr.to_vec().as_slice(),
            overlay.as_slice(),
            signature.as_bytes().as_slice(),
            nonce.as_slice(),
            true,
            network_id,
        );

        assert_eq!(node_address, node_address2.unwrap());
    }
}
