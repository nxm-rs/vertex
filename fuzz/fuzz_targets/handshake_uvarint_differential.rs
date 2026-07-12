//! Differential fuzz of the handshake frame-sizing arithmetic.
//!
//! The advertised-set bound predicts the serialized size of the multiaddr
//! block with hand-rolled uvarint arithmetic (`uvarint_len`, `entry_len`)
//! before the record is signed. This target checks that arithmetic against
//! the codec's actual encoded bytes: the predicted block size must equal the
//! encoded length (exactly for the list forms; the single-address form drops
//! both prefixes, so there the model is an upper bound), the codec must
//! accept its own encoding up to the record count cap, and `bound_advertised`
//! must keep the frame inside `MAX_HANDSHAKE_BUFFER_SIZE` for every welcome
//! size a conformant peer can send, without the budget math ever overflowing
//! (release overflow-checks are the oracle). Any disagreement is a sizing bug
//! that either signs an undecodable record or rejects a conformant one.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use vertex_swarm_net_handshake::fuzz::{
    FRAME_FIXED_BUDGET, MAX_HANDSHAKE_BUFFER_SIZE, MAX_MULTIADDRS_PER_PEER,
    MAX_WELCOME_MESSAGE_CHARS, Multiaddr, arbitrary_multiaddr, bound_advertised,
    deserialize_multiaddrs, entry_len, serialize_multiaddrs, uvarint_len,
};

/// Widest wire size of a maximal welcome message (4 UTF-8 bytes per scalar).
const MAX_WELCOME_WIRE_BYTES: usize = MAX_WELCOME_MESSAGE_CHARS * 4;

/// One sizing probe: a multiaddr block (possibly over the record count cap),
/// a welcome size (usually wire-representable, occasionally absurd so the
/// budget math is exercised at the integer edges), and raw uvarint values.
#[derive(Debug)]
struct SizingInput {
    addrs: Vec<Multiaddr>,
    welcome_bytes: usize,
    values: Vec<u64>,
}

impl<'a> Arbitrary<'a> for SizingInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let count = u.int_in_range(0..=MAX_MULTIADDRS_PER_PEER + 12)?;
        let addrs = (0..count)
            .map(|_| arbitrary_multiaddr(u))
            .collect::<arbitrary::Result<Vec<_>>>()?;
        let welcome_bytes = if u.arbitrary()? {
            u.int_in_range(0..=MAX_WELCOME_WIRE_BYTES)?
        } else {
            u.arbitrary()?
        };
        let values = u.arbitrary()?;
        Ok(Self {
            addrs,
            welcome_bytes,
            values,
        })
    }
}

/// Reference uvarint encoding, independent of the arithmetic under test:
/// emit 7-bit groups until the value is exhausted.
fn reference_uvarint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fuzz_target!(|input: SizingInput| {
    let SizingInput {
        addrs,
        welcome_bytes,
        values,
    } = input;

    for value in values.iter().copied().chain([0, 127, 128, u64::MAX]) {
        assert_eq!(
            uvarint_len(value),
            reference_uvarint(value).len(),
            "uvarint_len({value}) disagrees with the encoding"
        );
    }

    let predicted = 1usize + addrs.iter().map(entry_len).sum::<usize>();
    let encoded = serialize_multiaddrs(&addrs);
    if addrs.len() == 1 {
        assert!(
            encoded.len() <= predicted,
            "single-address encoding larger than the list-form prediction"
        );
    } else {
        assert_eq!(
            predicted,
            encoded.len(),
            "predicted block size disagrees with the encoded length"
        );
    }

    match deserialize_multiaddrs(&encoded) {
        Ok(decoded) => {
            assert!(addrs.len() <= MAX_MULTIADDRS_PER_PEER);
            assert_eq!(decoded, addrs, "codec must accept its own encoding");
        }
        Err(_) => assert!(
            addrs.len() > MAX_MULTIADDRS_PER_PEER,
            "codec rejected an encoding inside the count cap"
        ),
    }

    // Deterministic prefix truncation under the budget, and no overflow even
    // for an absurd welcome size (the subtraction must saturate).
    let bounded = bound_advertised(addrs.clone(), welcome_bytes);
    let extreme = bound_advertised(addrs.clone(), usize::MAX);
    assert_eq!(extreme.is_empty(), addrs.is_empty());

    assert!(bounded.len() <= MAX_MULTIADDRS_PER_PEER);
    assert!(
        bounded.iter().zip(&addrs).all(|(b, a)| b == a) && bounded.len() <= addrs.len(),
        "bounding must be prefix truncation"
    );
    assert_eq!(bounded.is_empty(), addrs.is_empty());

    // Frame-budget admission: for every wire-representable welcome size the
    // bounded block plus the fixed fields fits the frame cap. The one escape
    // is the single guaranteed entry a non-empty record must keep, which is
    // still far inside the cap on its own.
    let block = serialize_multiaddrs(&bounded).len();
    if welcome_bytes <= MAX_WELCOME_WIRE_BYTES {
        assert!(
            block + FRAME_FIXED_BUDGET + welcome_bytes <= MAX_HANDSHAKE_BUFFER_SIZE
                || bounded.len() == 1,
            "bounded block busts the frame budget: {block} bytes with {welcome_bytes} welcome"
        );
    }
    assert!(
        block + FRAME_FIXED_BUDGET <= MAX_HANDSHAKE_BUFFER_SIZE,
        "bounded block alone busts the frame cap: {block} bytes"
    );
});
