use super::{Ack, CodecError, Syn};

#[derive(Debug, Clone, PartialEq)]
pub struct SynAck<const N: u64> {
    syn: Syn<N>,
    ack: Ack<N>,
}

impl<const N: u64> SynAck<N> {
    pub fn new(syn: Syn<N>, ack: Ack<N>) -> Self {
        Self { syn, ack }
    }

    pub fn syn(&self) -> &Syn<N> {
        &self.syn
    }

    pub fn ack(&self) -> &Ack<N> {
        &self.ack
    }

    pub fn into_parts(self) -> (Syn<N>, Ack<N>) {
        (self.syn, self.ack)
    }
}

impl<const N: u64> TryFrom<crate::proto::handshake::SynAck> for SynAck<N> {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::SynAck) -> Result<Self, Self::Error> {
        Ok(Self::new(
            value
                .syn
                .ok_or_else(|| CodecError::MissingField("syn"))?
                .try_into()?,
            value
                .ack
                .ok_or_else(|| CodecError::MissingField("ack"))?
                .try_into()?,
        ))
    }
}

impl<const N: u64> From<SynAck<N>> for crate::proto::handshake::SynAck {
    fn from(value: SynAck<N>) -> Self {
        let (syn, ack) = value.into_parts();
        crate::proto::handshake::SynAck {
            syn: Some(syn.into()),
            ack: Some(ack.into()),
        }
    }
}

impl<'a, const N: u64> arbitrary::Arbitrary<'a> for SynAck<N> {
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

    const TEST_NETWORK_ID: u64 = 1234567890;

    // Helper function to create a test SynAck
    fn create_test_synack<const N: u64>() -> Result<SynAck<N>, CodecError> {
        let syn = Syn::arbitrary(&mut arbitrary::Unstructured::new(&[0u8; 32])).unwrap();
        let ack = Ack::arbitrary(&mut arbitrary::Unstructured::new(&[0u8; 32])).unwrap();
        Ok(SynAck::new(syn, ack))
    }

    proptest! {
        #[test]
        fn test_synack_proto_roundtrip(
            synack in arb::<SynAck<TEST_NETWORK_ID>>()
        ) {
            // Convert SynAck to proto
            let proto_synack: crate::proto::handshake::SynAck = synack.clone().into();

            // Convert proto back to SynAck
            let recovered_synack = SynAck::try_from(proto_synack);

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
        let proto_synack: crate::proto::handshake::SynAck =
            create_test_synack::<TEST_NETWORK_ID>().unwrap().into();

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
            let result = SynAck::<TEST_NETWORK_ID>::try_from(modified_synack);
            assert!(result.is_err());
            assert!(check_error(result.unwrap_err()));
        }
    }
}
