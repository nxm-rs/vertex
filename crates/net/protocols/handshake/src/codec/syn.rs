use libp2p::Multiaddr;
use vertex_net_primitives::{arbitrary_multiaddr, deserialize_underlays};

use super::CodecError;

#[derive(Debug, Clone, PartialEq)]
pub struct Syn {
    observed_underlay: Multiaddr,
}

impl Syn {
    pub fn new(observed_underlay: Multiaddr) -> Self {
        Self { observed_underlay }
    }

    pub fn observed_underlay(&self) -> &Multiaddr {
        &self.observed_underlay
    }
}

impl TryFrom<crate::proto::handshake::Syn> for Syn {
    type Error = CodecError;

    fn try_from(value: crate::proto::handshake::Syn) -> Result<Self, Self::Error> {
        // Deserialize underlays (Bee can send multiple addresses)
        let underlays = deserialize_underlays(&value.observed_underlay)
            .map_err(|_| CodecError::InvalidMultiaddr(
                libp2p::multiaddr::Error::InvalidMultiaddr
            ))?;

        // Use the first underlay
        let underlay = underlays.into_iter().next()
            .ok_or_else(|| CodecError::MissingField("observed_underlay"))?;

        Ok(Self::new(underlay))
    }
}

impl From<Syn> for crate::proto::handshake::Syn {
    fn from(syn: Syn) -> crate::proto::handshake::Syn {
        crate::proto::handshake::Syn {
            observed_underlay: syn.observed_underlay.to_vec(),
        }
    }
}

impl<'a> arbitrary::Arbitrary<'a> for Syn {
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

    // Helper function to create a valid Syn instance
    fn create_test_syn() -> Syn {
        Syn::new(Multiaddr::try_from("/ip4/127.0.0.1/tcp/1234").unwrap())
    }

    proptest! {
        #[test]
        fn test_syn_proto_roundtrip(
            syn in arb::<Syn>()
        ) {
            // Convert Syn to proto
            let proto_syn: crate::proto::handshake::Syn = syn.clone().into();

            // Convert proto back to Syn
            let recovered_syn = Syn::try_from(proto_syn);

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

        let result = Syn::try_from(proto_syn);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CodecError::InvalidMultiaddr(_)
        ))
    }
}
