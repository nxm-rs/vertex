use bytes::{Bytes, BytesMut};
use std::cell::OnceCell;
use std::marker::PhantomData;
use std::sync::Arc;
use vertex_network_primitives_traits::{NodeAddress, NodeAddressError};

use alloy::{
    primitives::{Address, Keccak256, PrimitiveSignature, B256},
    signers::{local::PrivateKeySigner, SignerSync},
};
use libp2p::Multiaddr;
use nectar_primitives_traits::SwarmAddress;

mod named;
pub use named::{NamedSwarm, NamedSwarmIter};
mod swarm;
pub use swarm::{Swarm, SwarmKind};

// Marker traits for builder states
pub trait BuilderState {}

#[derive(Default)]
pub struct Initial;
impl BuilderState for Initial {}

pub struct WithNonce;
impl BuilderState for WithNonce {}

pub struct WithUnderlay;
impl BuilderState for WithUnderlay {}

pub struct ReadyToBuild;
impl BuilderState for ReadyToBuild {}

// Local builder configuration
#[derive(Default)]
struct LocalNodeAddressConfig {
    nonce: Option<B256>,
    underlay: Option<Multiaddr>,
    signer: Option<Arc<PrivateKeySigner>>,
}

// Remote builder configuration
#[derive(Default)]
struct RemoteNodeAddressConfig {
    nonce: Option<B256>,
    underlay: Option<Multiaddr>,
    chain_address: Option<Address>,
    signature: Option<PrimitiveSignature>,
    overlay: Option<SwarmAddress>,
}

// Local Node Address Builder
pub struct LocalNodeAddressBuilder<const N: u64, State: BuilderState> {
    config: LocalNodeAddressConfig,
    _state: PhantomData<State>,
}

impl<const N: u64> LocalNodeAddressBuilder<N, Initial> {
    pub fn new() -> Self {
        Self {
            config: LocalNodeAddressConfig::default(),
            _state: PhantomData,
        }
    }

    pub fn with_nonce(self, nonce: B256) -> LocalNodeAddressBuilder<N, WithNonce> {
        LocalNodeAddressBuilder {
            config: LocalNodeAddressConfig {
                nonce: Some(nonce),
                ..self.config
            },
            _state: PhantomData,
        }
    }
}

impl<const N: u64> LocalNodeAddressBuilder<N, WithNonce> {
    pub fn with_underlay(self, underlay: Multiaddr) -> LocalNodeAddressBuilder<N, WithUnderlay> {
        LocalNodeAddressBuilder {
            config: LocalNodeAddressConfig {
                underlay: Some(underlay),
                ..self.config
            },
            _state: PhantomData,
        }
    }
}

impl<const N: u64> LocalNodeAddressBuilder<N, WithUnderlay> {
    pub fn with_signer(
        self,
        signer: Arc<PrivateKeySigner>,
    ) -> Result<LocalNodeAddressBuilder<N, ReadyToBuild>, NodeAddressError> {
        Ok(LocalNodeAddressBuilder {
            config: LocalNodeAddressConfig {
                signer: Some(signer),
                ..self.config
            },
            _state: PhantomData,
        })
    }
}

impl<const N: u64> LocalNodeAddressBuilder<N, ReadyToBuild> {
    pub fn build(self) -> Result<NodeAddressType<N>, NodeAddressError> {
        Ok(NodeAddressType::Local {
            nonce: self.config.nonce.unwrap(),
            underlay: self.config.underlay.unwrap(),
            signer: self.config.signer.unwrap(),
            overlay_cache: Default::default(),
        })
    }
}

// Remote Node Address Builder
pub struct RemoteNodeAddressBuilder<const N: u64, State: BuilderState> {
    config: RemoteNodeAddressConfig,
    _state: PhantomData<State>,
}

impl<const N: u64> RemoteNodeAddressBuilder<N, Initial> {
    pub fn new() -> Self {
        Self {
            config: RemoteNodeAddressConfig::default(),
            _state: PhantomData,
        }
    }

    pub fn with_nonce(self, nonce: B256) -> RemoteNodeAddressBuilder<N, WithNonce> {
        RemoteNodeAddressBuilder {
            config: RemoteNodeAddressConfig {
                nonce: Some(nonce),
                ..self.config
            },
            _state: PhantomData,
        }
    }
}

impl<const N: u64> RemoteNodeAddressBuilder<N, WithNonce> {
    pub fn with_underlay(self, underlay: Multiaddr) -> RemoteNodeAddressBuilder<N, WithUnderlay> {
        RemoteNodeAddressBuilder {
            config: RemoteNodeAddressConfig {
                underlay: Some(underlay),
                ..self.config
            },
            _state: PhantomData,
        }
    }
}

impl<const N: u64> RemoteNodeAddressBuilder<N, WithUnderlay> {
    pub fn with_identity(
        self,
        overlay: SwarmAddress,
        signature: PrimitiveSignature,
    ) -> Result<RemoteNodeAddressBuilder<N, ReadyToBuild>, NodeAddressError> {
        let underlay = self.config.underlay.as_ref().unwrap();
        let nonce = self.config.nonce.as_ref().unwrap();

        // Recover the remote node's signer address
        let recovered_address = recover_signer::<N>(underlay, &overlay, &signature)?;

        // Validate the overlay
        let recovered_overlay = compute_overlay_address::<N>(&recovered_address, nonce);
        if recovered_overlay != overlay {
            return Err(NodeAddressError::InvalidSignature);
        }

        Ok(RemoteNodeAddressBuilder {
            config: RemoteNodeAddressConfig {
                chain_address: Some(recovered_address),
                signature: Some(signature),
                overlay: Some(overlay),
                ..self.config
            },
            _state: PhantomData,
        })
    }
}

impl<const N: u64> RemoteNodeAddressBuilder<N, ReadyToBuild> {
    pub fn build(self) -> Result<NodeAddressType<N>, NodeAddressError> {
        Ok(NodeAddressType::Remote {
            nonce: self.config.nonce.unwrap(),
            underlay: self.config.underlay.unwrap(),
            chain_address: self.config.chain_address.unwrap(),
            signature: self.config.signature.unwrap(),
            overlay_cache: OnceCell::from(OverlayCache {
                address: self.config.overlay.unwrap(),
            }),
        })
    }
}

#[derive(Debug, Clone)]
pub struct OverlayCache {
    address: SwarmAddress,
}

#[derive(Debug, Clone)]
pub enum NodeAddressType<const N: u64> {
    Local {
        nonce: B256,
        underlay: Multiaddr,
        signer: Arc<PrivateKeySigner>,
        overlay_cache: OnceCell<OverlayCache>,
    },
    Remote {
        nonce: B256,
        underlay: Multiaddr,
        chain_address: Address,
        signature: PrimitiveSignature,
        overlay_cache: OnceCell<OverlayCache>,
    },
}

impl<const N: u64> NodeAddress<N> for NodeAddressType<N> {
    fn overlay_address(&self) -> SwarmAddress {
        match self {
            NodeAddressType::Local {
                signer,
                nonce,
                overlay_cache,
                ..
            } => {
                overlay_cache
                    .get_or_init(|| OverlayCache {
                        address: compute_overlay_address::<N>(&signer.address(), nonce),
                    })
                    .address
            }
            NodeAddressType::Remote {
                chain_address,
                nonce,
                overlay_cache,
                ..
            } => {
                overlay_cache
                    .get_or_init(|| OverlayCache {
                        address: compute_overlay_address::<N>(chain_address, nonce),
                    })
                    .address
            }
        }
    }

    fn chain_address(&self) -> Address {
        match self {
            NodeAddressType::Local { signer, .. } => signer.address(),
            NodeAddressType::Remote { chain_address, .. } => *chain_address,
        }
    }

    fn nonce(&self) -> &B256 {
        match self {
            NodeAddressType::Local { nonce, .. } => nonce,
            NodeAddressType::Remote { nonce, .. } => nonce,
        }
    }

    fn signature(&self) -> Result<PrimitiveSignature, NodeAddressError> {
        match self {
            NodeAddressType::Local {
                signer, underlay, ..
            } => {
                let msg = generate_sign_message::<N>(underlay, &self.overlay_address());
                signer
                    .sign_message_sync(&msg)
                    .map_err(NodeAddressError::from)
            }
            NodeAddressType::Remote { signature, .. } => Ok(*signature),
        }
    }

    fn underlay_address(&self) -> &Multiaddr {
        match self {
            NodeAddressType::Local { underlay, .. } => underlay,
            NodeAddressType::Remote { underlay, .. } => underlay,
        }
    }
}

fn recover_signer<const N: u64>(
    underlay: &Multiaddr,
    overlay: &SwarmAddress,
    signature: &PrimitiveSignature,
) -> Result<Address, NodeAddressError> {
    let prehash = generate_sign_message::<N>(underlay, overlay);
    Ok(signature.recover_address_from_msg(prehash)?)
}

fn compute_overlay_address<const N: u64>(address: &Address, nonce: &B256) -> SwarmAddress {
    let mut hasher = Keccak256::new();
    hasher.update(address);
    hasher.update(N.to_le_bytes());
    hasher.update(nonce);
    hasher.finalize()
}

fn generate_sign_message<const N: u64>(underlay: &Multiaddr, overlay: &SwarmAddress) -> Bytes {
    let mut message = BytesMut::new();
    message.extend_from_slice(b"bee-handshake-");
    message.extend_from_slice(underlay.as_ref());
    message.extend_from_slice(overlay.as_ref());
    message.extend_from_slice(N.to_be_bytes().as_slice());
    message.freeze()
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_local_builder() {
//         const SWARM: u64 = 1;

//         let signer = Arc::new(PrivateKeySigner::random());
//         let local_address = LocalNodeAddressBuilder::<SWARM, _>::new()
//             .with_nonce(B256::random())
//             .with_underlay("/ip4/127.0.0.1/tcp/1634".parse().unwrap())
//             .with_signer(signer)
//             .unwrap();

//         assert!(local_address.overlay_address().as_ref().len() > 0);
//     }

//     #[test]
//     fn test_remote_builder() {
//         const SWARM: u64 = 1;

//         let remote_address = RemoteNodeAddressBuilder::<SWARM, _>::new()
//             .with_nonce(B256::random())
//             .with_underlay("/ip4/127.0.0.1/tcp/1634".parse().unwrap())
//             .with_identity(SwarmAddress::random(), PrimitiveSignature::default())
//             .expect("Should build successfully");

//         assert!(remote_address.overlay_address().as_ref().len() > 0);
//     }
// }
