use libp2p::Multiaddr;
use vertex_network_primitives::NodeAddress;
use vertex_network_primitives_traits::NodeAddress as NodeAddressTrait;

use crate::codec::CodecError;
use crate::MAX_WELCOME_MESSAGE_LENGTH;

#[derive(Debug, Clone)]
pub struct Ack<const N: u64> {
    pub(crate) node_address: NodeAddress<N>,
    pub(crate) full_node: bool,
    pub(crate) welcome_message: String,
}

impl<const N: u64> TryFrom<crate::proto::handshake::Ack> for Ack<N> {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::Ack) -> Result<Self, Self::Error> {
        if value.NetworkID != N {
            return Err(CodecError::NetworkIDMismatch);
        }

        if value.WelcomeMessage.len() > MAX_WELCOME_MESSAGE_LENGTH {
            return Err(CodecError::FieldLengthLimitExceeded(
                "welcome_message",
                MAX_WELCOME_MESSAGE_LENGTH,
                value.WelcomeMessage.len(),
            ));
        }

        let protobuf_address = value
            .Address
            .as_ref()
            .ok_or_else(|| CodecError::MissingField("address"))?;
        let remote_address = NodeAddress::builder()
            .with_nonce(value.Nonce.as_slice().try_into()?)
            .with_underlay(Multiaddr::try_from(protobuf_address.Underlay.clone())?)
            .with_signature(
                protobuf_address.Overlay.as_slice().try_into()?,
                protobuf_address.Signature.as_slice().try_into()?,
                // Validate signatures at the codec level
                true,
            )?
            .build();
        Ok(Self {
            node_address: remote_address,
            full_node: value.FullNode,
            welcome_message: value.WelcomeMessage,
        })
    }
}

impl<const N: u64> Into<crate::proto::handshake::Ack> for Ack<N> {
    fn into(self) -> crate::proto::handshake::Ack {
        crate::proto::handshake::Ack {
            Address: Some(crate::proto::handshake::BzzAddress {
                Underlay: self.node_address.underlay_address().to_vec(),
                Signature: self.node_address.signature().unwrap().as_bytes().to_vec(),
                Overlay: self.node_address.overlay_address().to_vec(),
            }),
            NetworkID: N,
            FullNode: self.full_node,
            Nonce: self.node_address.nonce().to_vec(),
            WelcomeMessage: self.welcome_message,
        }
    }
}
