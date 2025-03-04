use libp2p::Multiaddr;
use vertex_network_primitives::arbitrary_multiaddr;

use super::CodecError;

#[derive(Debug, Clone, PartialEq)]
pub struct Syn<const N: u64> {
    observed_underlay: Multiaddr,
}

impl<const N: u64> Syn<N> {
    pub fn new(observed_underlay: Multiaddr) -> Self {
        Self { observed_underlay }
    }

    pub fn observed_underlay(&self) -> &Multiaddr {
        &self.observed_underlay
    }
}

impl<const N: u64> TryFrom<crate::proto::handshake::Syn> for Syn<N> {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::Syn) -> Result<Self, Self::Error> {
        Ok(Self::new(Multiaddr::try_from(value.observed_underlay)?))
    }
}

impl<const N: u64> Into<crate::proto::handshake::Syn> for Syn<N> {
    fn into(self) -> crate::proto::handshake::Syn {
        crate::proto::handshake::Syn {
            observed_underlay: self.observed_underlay.to_vec(),
        }
    }
}

impl<'a, const N: u64> arbitrary::Arbitrary<'a> for Syn<N> {
    fn arbitrary(u: &mut arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let observed_underlay = arbitrary_multiaddr(u)?;
        Ok(Self::new(observed_underlay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;

    const TEST_NETWORK_ID: u64 = 1234567890;

    // Helper function to create a valid Syn instance
    fn create_test_syn() -> Syn<1> {
        Syn::new(Multiaddr::try_from("/ip4/127.0.0.1/tcp/1234").unwrap())
    }

    proptest! {
        #[test]
        fn test_syn_proto_roundtrip(
            syn in arb::<Syn<TEST_NETWORK_ID>>()
        ) {
            // Convert Syn to proto
            let proto_syn: crate::proto::handshake::Syn = syn.clone().into();

            // Convert proto back to Syn
            let recovered_syn = Syn::<TEST_NETWORK_ID>::try_from(proto_syn);

            prop_assert!(recovered_syn.is_ok());
            let recovered_syn = recovered_syn.unwrap();

            // Verify equality
            prop_assert_eq!(&syn, &recovered_syn);

            // Verify fields using accessors
            prop_assert_eq!(syn.observed_underlay(), recovered_syn.observed_underlay());
        }
    }

    #[test]
    fn test_syn_err_on_malformed_proto() {
        let mut proto_syn: crate::proto::handshake::Syn = create_test_syn().into();
        proto_syn.observed_underlay = vec![0x01, 0x02, 0x03];

        let result = Syn::<TEST_NETWORK_ID>::try_from(proto_syn);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CodecError::InvalidMultiaddr(_)
        ))
    }
}
