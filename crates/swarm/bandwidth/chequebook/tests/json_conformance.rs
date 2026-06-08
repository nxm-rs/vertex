//! Wire-conformance vectors for the swap cheque JSON encoding.
//!
//! A `SignedCheque` travels on the swap protocol as a JSON object embedded in a
//! protobuf bytes field. These vectors pin the exact bytes against the live
//! network format so the encoder and decoder cannot drift. The expected strings
//! were produced by the reference encoder and must not be edited to match a code
//! change: if the codec output diverges from these, the codec is wrong.
#![allow(clippy::unwrap_used)]

use alloy_primitives::{Address, U256};
use bytes::Bytes;
use vertex_swarm_bandwidth_chequebook::{ChequeExt, SignedCheque, cheque::Cheque};

struct Vector {
    chequebook: &'static str,
    beneficiary: &'static str,
    payout: &'static str,
    signature: Vec<u8>,
    json: &'static str,
}

fn vectors() -> Vec<Vector> {
    vec![
        // Sequential signature bytes 0..=64, simple payout.
        Vector {
            chequebook: "0x0101010101010101010101010101010101010101",
            beneficiary: "0x0202020202020202020202020202020202020202",
            payout: "1000000",
            signature: (0u8..=64).collect(),
            json: "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":1000000,\"Signature\":\"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0A=\"}",
        },
        // Maximum U256 payout, four-byte signature exercising base64 padding.
        Vector {
            chequebook: "0xcafebabecafebabecafebabecafebabecafebabe",
            beneficiary: "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            payout: "115792089237316195423570985008687907853269984665640564039457584007913129639935",
            signature: vec![0xde, 0xad, 0xbe, 0xef],
            json: "{\"Chequebook\":\"0xcafebabecafebabecafebabecafebabecafebabe\",\"Beneficiary\":\"0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\"CumulativePayout\":115792089237316195423570985008687907853269984665640564039457584007913129639935,\"Signature\":\"3q2+7w==\"}",
        },
        // Zero payout and an empty signature.
        Vector {
            chequebook: "0x0000000000000000000000000000000000000000",
            beneficiary: "0x0000000000000000000000000000000000000001",
            payout: "0",
            signature: vec![],
            json: "{\"Chequebook\":\"0x0000000000000000000000000000000000000000\",\"Beneficiary\":\"0x0000000000000000000000000000000000000001\",\"CumulativePayout\":0,\"Signature\":\"\"}",
        },
    ]
}

fn build(v: &Vector) -> SignedCheque {
    SignedCheque::new(
        Cheque::new(
            v.chequebook.parse::<Address>().unwrap(),
            v.beneficiary.parse::<Address>().unwrap(),
            v.payout.parse::<U256>().unwrap(),
        ),
        Bytes::from(v.signature.clone()),
    )
}

#[test]
fn encode_matches_reference_bytes() {
    for v in vectors() {
        let signed = build(&v);
        let encoded = signed.to_json();
        assert_eq!(
            core::str::from_utf8(&encoded).unwrap(),
            v.json,
            "encoded bytes diverged from the pinned wire format"
        );
    }
}

#[test]
fn decode_reference_bytes() {
    for v in vectors() {
        let decoded = SignedCheque::from_json(v.json.as_bytes()).unwrap();
        assert_eq!(
            decoded,
            build(&v),
            "decoded cheque did not match the vector"
        );
    }
}

#[test]
fn round_trip_through_wire_bytes() {
    for v in vectors() {
        let signed = build(&v);
        let bytes = signed.to_json();
        let back = SignedCheque::from_json(&bytes).unwrap();
        assert_eq!(signed, back);
    }
}

#[test]
fn decode_rejects_missing_field() {
    let json = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"Signature\":\"\"}";
    assert!(SignedCheque::from_json(json.as_bytes()).is_err());
}

#[test]
fn decode_rejects_bad_base64() {
    let json = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":1,\"Signature\":\"@@@\"}";
    assert!(SignedCheque::from_json(json.as_bytes()).is_err());
}

#[test]
fn decode_rejects_non_decimal_payout() {
    let json = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":\"123\",\"Signature\":\"\"}";
    assert!(SignedCheque::from_json(json.as_bytes()).is_err());
}

#[test]
fn decode_rejects_quoted_payout_string() {
    // Payout must be a bare JSON number, never a quoted string.
    let json = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":notanumber,\"Signature\":\"\"}";
    assert!(SignedCheque::from_json(json.as_bytes()).is_err());
}
