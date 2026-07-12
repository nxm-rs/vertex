//! Codec implementations for fixed-byte key types.

#[cfg(feature = "nectar")]
crate::impl_fixed_codec!(nectar_primitives::OverlayAddress, 32);
#[cfg(feature = "nectar")]
crate::impl_fixed_codec!(nectar_primitives::ChunkAddress, 32);

#[cfg(feature = "alloy")]
crate::impl_fixed_codec!(alloy_primitives::Address, 20);

#[cfg(test)]
mod tests {
    use crate::{Decode, Encode};

    #[cfg(feature = "nectar")]
    mod nectar {
        use super::*;
        use nectar_primitives::{ChunkAddress, OverlayAddress};

        #[test]
        fn test_overlay_address_roundtrip() {
            let addr = OverlayAddress::from([0x42u8; 32]);
            let encoded = addr.encode();
            let decoded = OverlayAddress::decode(&encoded).unwrap();
            assert_eq!(decoded, addr);
        }

        #[test]
        fn test_overlay_address_zero() {
            let addr = OverlayAddress::from([0u8; 32]);
            let encoded = addr.encode();
            let decoded = OverlayAddress::decode(&encoded).unwrap();
            assert_eq!(decoded, addr);
        }

        #[test]
        fn test_overlay_address_decode_wrong_length() {
            assert!(OverlayAddress::decode(&[0u8; 31]).is_err());
            assert!(OverlayAddress::decode(&[0u8; 33]).is_err());
            assert!(OverlayAddress::decode(&[]).is_err());
        }

        #[test]
        fn test_chunk_address_roundtrip() {
            let addr = ChunkAddress::from([0x42u8; 32]);
            let encoded = addr.encode();
            let decoded = ChunkAddress::decode(&encoded).unwrap();
            assert_eq!(decoded, addr);
        }

        #[test]
        fn test_chunk_address_decode_wrong_length() {
            assert!(ChunkAddress::decode(&[0u8; 31]).is_err());
            assert!(ChunkAddress::decode(&[]).is_err());
        }
    }

    #[cfg(feature = "alloy")]
    mod alloy {
        use super::*;
        use alloy_primitives::Address;

        #[test]
        fn test_address_roundtrip() {
            let addr = Address::repeat_byte(0x42);
            let encoded = addr.encode();
            assert_eq!(encoded.len(), 20);
            let decoded = Address::decode(&encoded).unwrap();
            assert_eq!(decoded, addr);
        }

        #[test]
        fn test_address_zero() {
            let addr = Address::ZERO;
            let encoded = addr.encode();
            let decoded = Address::decode(&encoded).unwrap();
            assert_eq!(decoded, addr);
        }

        #[test]
        fn test_address_decode_wrong_length() {
            assert!(Address::decode(&[0u8; 19]).is_err());
            assert!(Address::decode(&[0u8; 21]).is_err());
            assert!(Address::decode(&[]).is_err());
        }
    }
}
