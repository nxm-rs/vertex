use alloy_primitives::{keccak256, Address, FixedBytes};
use alloy_signer_wallet::LocalWallet;

use crate::{distaddr::DistAddr, HASH_SIZE};

pub type OverlayAddress = DistAddr;
pub(crate) type Nonce = FixedBytes<HASH_SIZE>;

pub trait Overlay {
    fn overlay(&self, network_id: u64, nonce: Option<Nonce>) -> OverlayAddress;
}

impl Overlay for LocalWallet {
    // Generates the overlay address for the signer.
    fn overlay(&self, network_id: u64, nonce: Option<Nonce>) -> OverlayAddress {
        calc_overlay(self.address(), network_id, nonce)
    }
}

impl Overlay for Address {
    // Generates the overlay address for the address.
    fn overlay(&self, network_id: u64, nonce: Option<Nonce>) -> OverlayAddress {
        calc_overlay(*self, network_id, nonce)
    }
}

fn calc_overlay(address: Address, network_id: u64, nonce: Option<Nonce>) -> OverlayAddress {
    let mut data = [0u8; 20 + 8 + 32];
    data[..20].copy_from_slice(address.0.as_slice());
    data[20..28].copy_from_slice(&network_id.to_le_bytes());
    if let Some(nonce) = nonce {
        data[28..60].copy_from_slice(nonce.as_slice());
    }

    keccak256(data)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{bytes, FixedBytes};
    use alloy_signer_wallet::LocalWallet;

    use super::*;

    #[test]
    fn test_new_ethereum_address() {
        let signer: LocalWallet =
            "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"
                .parse()
                .unwrap();
        assert_eq!(
            signer.address(),
            "0x2f63cbeb054ce76050827e42dd75268f6b9d87c5"
                .parse::<Address>()
                .unwrap(),
            "address mismatch"
        );
        assert_eq!(
            signer.overlay(1, None),
            "0x9b109ce1ec1a7134e3ef27146e56a8b5566b8397f993fa40bc4c66f36043392f"
                .parse::<OverlayAddress>()
                .unwrap(),
            "overlay mismatch"
        );
    }

    #[test]
    fn test_new_overlay_from_ethereum_address() {
        let test_cases = vec![
            (
                "1815cac638d1525b47f848daf02b7953e4edd15c",
                1,
                bytes!("01"),
                "0xa38f7a814d4b249ae9d3821e9b898019c78ac9abe248fff171782c32a3849a17",
            ),
            (
                "1815cac638d1525b47f848daf02b7953e4edd15c",
                1,
                bytes!("02"),
                "0xc63c10b1728dfc463c64c264f71a621fe640196979375840be42dc496b702610",
            ),
            (
                "d26bc1715e933bd5f8fad16310042f13abc16159",
                2,
                bytes!("01"),
                "0x9f421f9149b8e31e238cfbdc6e5e833bacf1e42f77f60874d49291292858968e",
            ),
            (
                "ac485e3c63dcf9b4cda9f007628bb0b6fed1c063",
                1,
                bytes!("00"),
                "0xfe3a6d582c577404fb19df64a44e00d3a3b71230a8464c0dd34af3f0791b45f2",
            ),
        ];

        for (addr, network_id, nonce, expected_address) in test_cases {
            let nonce = FixedBytes::<32>::left_padding_from(&nonce);
            let addr: Address = addr.parse().unwrap();

            let got_address: OverlayAddress = addr.overlay(network_id, Some(nonce));
            let expected_address: OverlayAddress = expected_address.parse().unwrap();

            assert_eq!(
                got_address, expected_address,
                "Expected {:x?}, but got {:x?}",
                expected_address, got_address
            );
        }
    }
}
