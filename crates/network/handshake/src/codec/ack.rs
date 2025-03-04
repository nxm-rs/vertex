use libp2p::Multiaddr;
use vertex_network_primitives::NodeAddress;
use vertex_network_primitives_traits::NodeAddress as NodeAddressTrait;

use crate::codec::CodecError;
use crate::MAX_WELCOME_MESSAGE_CHARS;

#[derive(Debug, Clone, PartialEq)]
pub struct Ack<const N: u64> {
    node_address: NodeAddress<N>,
    full_node: bool,
    welcome_message: String,
}

impl<const N: u64> Ack<N> {
    pub fn new(
        node_address: NodeAddress<N>,
        full_node: bool,
        welcome_message: String,
    ) -> Result<Self, CodecError> {
        if welcome_message.chars().count() > MAX_WELCOME_MESSAGE_CHARS {
            return Err(CodecError::FieldLengthLimitExceeded(
                "welcome_message",
                MAX_WELCOME_MESSAGE_CHARS,
                welcome_message.chars().count(),
            ));
        }

        Ok(Self {
            node_address,
            full_node,
            welcome_message,
        })
    }

    pub fn node_address(&self) -> &NodeAddress<N> {
        &self.node_address
    }

    pub fn full_node(&self) -> bool {
        self.full_node
    }

    pub fn welcome_message(&self) -> &str {
        &self.welcome_message
    }
}

impl<const N: u64> TryFrom<crate::proto::handshake::Ack> for Ack<N> {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::Ack) -> Result<Self, Self::Error> {
        if value.network_id != N {
            return Err(CodecError::NetworkIDMismatch);
        }

        let protobuf_address = value
            .address
            .as_ref()
            .ok_or_else(|| CodecError::MissingField("address"))?;
        let remote_address = NodeAddress::builder()
            .with_nonce(value.nonce.as_slice().try_into()?)
            .with_underlay(Multiaddr::try_from(protobuf_address.underlay.clone())?)
            .with_signature(
                protobuf_address.overlay.as_slice().try_into()?,
                protobuf_address.signature.as_slice().try_into()?,
                // Validate signatures at the codec level
                true,
            )?
            .build();
        Ok(Ack::new(
            remote_address,
            value.full_node,
            value.welcome_message,
        )?)
    }
}

impl<const N: u64> Into<crate::proto::handshake::Ack> for Ack<N> {
    fn into(self) -> crate::proto::handshake::Ack {
        crate::proto::handshake::Ack {
            address: Some(crate::proto::handshake::BzzAddress {
                underlay: self.node_address.underlay_address().to_vec(),
                signature: self.node_address.signature().unwrap().as_bytes().to_vec(),
                overlay: self.node_address.overlay_address().to_vec(),
            }),
            network_id: N,
            full_node: self.full_node,
            nonce: self.node_address.nonce().to_vec(),
            welcome_message: self.welcome_message,
        }
    }
}

impl<'a, const N: u64> arbitrary::Arbitrary<'a> for Ack<N> {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let node_address = NodeAddress::arbitrary(u)?;
        let full_node = u.arbitrary()?;

        // Generate welcome message with valid length
        let max_len = std::cmp::min(u.len(), MAX_WELCOME_MESSAGE_CHARS);
        let welcome_len = u.int_in_range(0..=max_len)?;
        let welcome_bytes: Vec<u8> = (0..welcome_len)
            .map(|_| u.arbitrary())
            .collect::<arbitrary::Result<Vec<u8>>>()?;

        let welcome_message = String::from_utf8_lossy(&welcome_bytes)
            .chars()
            .take(MAX_WELCOME_MESSAGE_CHARS)
            .collect::<String>();

        Ok(Self {
            node_address,
            full_node,
            welcome_message,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use alloy_primitives::U256;
    use alloy_signer::k256::ecdsa::SigningKey;
    use alloy_signer_local::{LocalSigner, PrivateKeySigner};
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    const TEST_NETWORK_ID: u64 = 1234567890;

    // Helper function to create a valid Ack instance
    fn create_test_ack<const N: u64>(
        signer: Arc<LocalSigner<SigningKey>>,
        welcome_message: String,
        full_node: bool,
    ) -> Result<Ack<N>, CodecError> {
        let underlay: Multiaddr = "/ip4/127.0.0.1/tcp/1234".parse().unwrap();
        let node_address = NodeAddress::builder()
            .with_nonce(Default::default())
            .with_underlay(underlay)
            .with_signer(signer)
            .unwrap()
            .build();

        Ok(Ack {
            node_address,
            full_node,
            welcome_message,
        })
    }

    proptest! {
        #[test]
        fn test_ack_proto_roundtrip(
            ack in arb::<Ack<TEST_NETWORK_ID>>()
        ) {
            // Convert Ack to proto
            let proto_ack: crate::proto::handshake::Ack = ack.clone().into();

            // Convert proto back to Ack
            let recovered_ack = Ack::try_from(proto_ack);

            prop_assert!(recovered_ack.is_ok());
            let recovered_ack = recovered_ack.unwrap();

            // Verify equality
            prop_assert_eq!(&ack, &recovered_ack);

            // Verify fields using accessors
            prop_assert_eq!(ack.node_address(), recovered_ack.node_address());
            prop_assert_eq!(ack.full_node(), recovered_ack.full_node());
            prop_assert_eq!(ack.welcome_message(), recovered_ack.welcome_message());
        }

        #[test]
        fn test_ack_welcome_message_validation(
            full_node in any::<bool>(),
        ) {
            let signer = Arc::new(PrivateKeySigner::random());

            let base_char = "x";

            // Test with message at max length
            let max_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS);
            let ack = create_test_ack::<TEST_NETWORK_ID>(signer.clone(), max_message, full_node);
            prop_assert!(ack.is_ok());

            // Test with message exceeding max length
            let too_long_message = base_char.repeat(MAX_WELCOME_MESSAGE_CHARS + 1);
            let mut proto_ack: crate::proto::handshake::Ack = ack.unwrap().into();
            proto_ack.welcome_message = too_long_message;

            let result = Ack::<TEST_NETWORK_ID>::try_from(proto_ack);
            prop_assert!(result.is_err());
            prop_assert!(matches!(result.unwrap_err(), CodecError::FieldLengthLimitExceeded(_, _, _)))
        }

        #[test]
        fn test_ack_network_id_validation(
            ack in arb::<Ack<TEST_NETWORK_ID>>(),
            wrong_network_id in (TEST_NETWORK_ID + 1..u64::MAX),
        ) {
            let mut proto_ack: crate::proto::handshake::Ack = ack.into();
            proto_ack.network_id = wrong_network_id;

            let result = Ack::<TEST_NETWORK_ID>::try_from(proto_ack);
            prop_assert!(result.is_err());
            prop_assert!(matches!(
                result.unwrap_err(),
                CodecError::NetworkIDMismatch
            ));
        }
    }

    #[test]
    fn test_ack_err_on_malformed_proto() {
        let proto_ack: crate::proto::handshake::Ack = create_test_ack::<TEST_NETWORK_ID>(
            Arc::new(PrivateKeySigner::random()),
            "test".to_string(),
            false,
        )
        .unwrap()
        .into();

        type AckModifier =
            Box<dyn Fn(crate::proto::handshake::Ack) -> crate::proto::handshake::Ack>;

        let test_cases: Vec<(AckModifier, Box<dyn Fn(CodecError) -> bool>)> =
            vec![
                (
                    Box::new(|mut ack| {
                        ack.address = None;
                        ack
                    }),
                    Box::new(|e| matches!(e, CodecError::MissingField("address"))),
                ),
                (
                    Box::new(|mut ack| {
                        let mut addr = ack.address.unwrap();
                        addr.signature = vec![0u8; 65];
                        ack.address = Some(addr);
                        ack
                    }),
                    Box::new(|e| {
                        matches!(e, CodecError::InvalidNodeAddress(
                    vertex_network_primitives_traits::NodeAddressError::InvalidSignature(_)
                ))
                    }),
                ),
                (
                    Box::new(|mut ack| {
                        let mut addr = ack.address.unwrap();
                        addr.underlay = vec![0u8; 32];
                        ack.address = Some(addr);
                        ack
                    }),
                    Box::new(|e| matches!(e, CodecError::InvalidMultiaddr(_))),
                ),
                (
                    Box::new(|mut ack| {
                        let mut addr = ack.address.unwrap();
                        addr.overlay = vec![0u8; 32];
                        ack.address = Some(addr);
                        ack
                    }),
                    Box::new(|e| {
                        matches!(
                            e,
                            CodecError::InvalidNodeAddress(
                                vertex_network_primitives_traits::NodeAddressError::InvalidOverlay
                            )
                        )
                    }),
                ),
                (
                    Box::new(|mut ack| {
                        ack.nonce = vec![0u8; 16];
                        ack
                    }),
                    Box::new(|e| matches!(e, CodecError::InvalidData(_))),
                ),
            ];

        for (modify_ack, check_error) in test_cases {
            let modified_ack = modify_ack(proto_ack.clone());
            let result = Ack::<TEST_NETWORK_ID>::try_from(modified_ack);
            assert!(result.is_err());
            assert!(check_error(result.unwrap_err()));
        }
    }
}
