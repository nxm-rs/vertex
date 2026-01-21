use super::{ack_from_proto, Ack, CodecError, Syn};

#[derive(Debug, Clone, PartialEq)]
pub struct SynAck {
    syn: Syn,
    ack: Ack,
}

impl SynAck {
    pub fn new(syn: Syn, ack: Ack) -> Self {
        Self { syn, ack }
    }

    pub fn syn(&self) -> &Syn {
        &self.syn
    }

    pub fn ack(&self) -> &Ack {
        &self.ack
    }

    pub fn into_parts(self) -> (Syn, Ack) {
        (self.syn, self.ack)
    }
}

/// Convert from protobuf SynAck to our SynAck, validating the network_id matches.
pub fn synack_from_proto(
    value: crate::proto::handshake::SynAck,
    expected_network_id: u64,
) -> Result<SynAck, CodecError> {
    let syn = value
        .syn
        .ok_or_else(|| CodecError::MissingField("syn"))?
        .try_into()?;
    let ack = ack_from_proto(
        value
            .ack
            .ok_or_else(|| CodecError::MissingField("ack"))?,
        expected_network_id,
    )?;
    Ok(SynAck::new(syn, ack))
}

impl From<SynAck> for crate::proto::handshake::SynAck {
    fn from(value: SynAck) -> Self {
        let (syn, ack) = value.into_parts();
        crate::proto::handshake::SynAck {
            syn: Some(syn.into()),
            ack: Some(ack.into()),
        }
    }
}

impl<'a> arbitrary::Arbitrary<'a> for SynAck {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            syn: Syn::arbitrary(u)?,
            ack: Ack::arbitrary(u)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbitrary::Arbitrary;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use vertex_net_primitives_traits::NodeAddress as NodeAddressTrait;

    // Helper function to create a test SynAck
    fn create_test_synack() -> SynAck {
        let syn = Syn::arbitrary(&mut arbitrary::Unstructured::new(&[0u8; 256])).unwrap();
        let ack = Ack::arbitrary(&mut arbitrary::Unstructured::new(&[0u8; 256])).unwrap();
        SynAck::new(syn, ack)
    }

    proptest! {
        #[test]
        fn test_synack_proto_roundtrip(
            synack in arb::<SynAck>()
        ) {
            // Get the network_id from the ack's node_address for validation
            let network_id = synack.ack().node_address().network_id();

            // Convert SynAck to proto
            let proto_synack: crate::proto::handshake::SynAck = synack.clone().into();

            // Convert proto back to SynAck
            let recovered_synack = synack_from_proto(proto_synack, network_id);

            prop_assert!(recovered_synack.is_ok());
            let recovered_synack = recovered_synack.unwrap();

            // Verify equality
            prop_assert_eq!(&synack, &recovered_synack);

            // Verify fields using accessors
            prop_assert_eq!(synack.syn(), recovered_synack.syn());
            prop_assert_eq!(synack.ack(), recovered_synack.ack());
        }
    }

    #[test]
    fn test_synack_err_on_malformed_proto() {
        let synack = create_test_synack();
        let network_id = synack.ack().node_address().network_id();
        let proto_synack: crate::proto::handshake::SynAck = synack.into();

        type SynAckModifier =
            Box<dyn Fn(crate::proto::handshake::SynAck) -> crate::proto::handshake::SynAck>;

        let test_cases: Vec<(SynAckModifier, Box<dyn Fn(CodecError) -> bool>)> = vec![
            (
                Box::new(|mut synack| {
                    synack.syn = None;
                    synack
                }),
                Box::new(|e| matches!(e, CodecError::MissingField("syn"))),
            ),
            (
                Box::new(|mut synack| {
                    synack.ack = None;
                    synack
                }),
                Box::new(|e| matches!(e, CodecError::MissingField("ack"))),
            ),
        ];

        for (modify_synack, check_error) in test_cases {
            let modified_synack = modify_synack(proto_synack.clone());
            let result = synack_from_proto(modified_synack, network_id);
            assert!(result.is_err());
            assert!(check_error(result.unwrap_err()));
        }
    }
}
