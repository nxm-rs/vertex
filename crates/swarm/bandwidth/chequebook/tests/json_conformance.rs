//! Cross-implementation conformance tests for the swap cheque JSON encoding.
//!
//! A `SignedCheque` travels on the swap protocol as a JSON object embedded in a
//! protobuf bytes field. The JSON is transport-only: the cheque signature is
//! EIP-712 over the cheque fields, not over the JSON bytes, so the encoding does
//! not need to be byte-identical to any peer. These tests validate the encoding
//! semantically: a sign/serialize/deserialize/recover round-trip, parsing a
//! realistic peer-format sample, and a structural assertion on the emitted shape
//! (keys and JSON value types), never a fixed byte string.
#![allow(clippy::unwrap_used)]

use alloy_primitives::{Address, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use bytes::Bytes;
use serde_json::Value;
use vertex_swarm_bandwidth_chequebook::{ChequeExt, SignedCheque, cheque::Cheque};
use vertex_swarm_spec::init_mainnet;

fn sample_cheque(payout: U256, signature: Vec<u8>) -> SignedCheque {
    SignedCheque::new(
        Cheque::new(
            Address::repeat_byte(0x01),
            Address::repeat_byte(0x02),
            payout,
        ),
        Bytes::from(signature),
    )
}

#[test]
fn sign_serialize_deserialize_recovers_signer() {
    let spec = init_mainnet();
    let signer = PrivateKeySigner::random();

    let cheque = Cheque::new(
        Address::repeat_byte(0xaa),
        Address::repeat_byte(0xbb),
        U256::from(123_456_789u64),
    );
    let hash = cheque.signing_hash(&*spec);
    let sig = signer.sign_hash_sync(&hash).unwrap();
    let signed = SignedCheque::from_signature(cheque.clone(), sig);

    let bytes = serde_json::to_vec(&signed).unwrap();
    let decoded: SignedCheque = serde_json::from_slice(&bytes).unwrap();

    // Field values survive the round-trip.
    assert_eq!(decoded.cheque, cheque);
    assert_eq!(decoded.signature, signed.signature);

    // The signature still recovers to the original signer after the round-trip.
    assert_eq!(decoded.recover_signer(&*spec).unwrap(), signer.address());
    decoded.verify(signer.address(), &*spec).unwrap();
}

#[test]
fn parses_peer_format_sample() {
    // A realistic sample in the field-value conventions other Swarm nodes emit:
    // PascalCase keys, lowercase 0x addresses, a bare-number payout spanning the
    // full U256 range, and a standard base64 signature.
    let sample = "{\
        \"Chequebook\":\"0xcafebabecafebabecafebabecafebabecafebabe\",\
        \"Beneficiary\":\"0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\
        \"CumulativePayout\":115792089237316195423570985008687907853269984665640564039457584007913129639935,\
        \"Signature\":\"3q2+7w==\"}";

    let decoded: SignedCheque = serde_json::from_slice(sample.as_bytes()).unwrap();

    assert_eq!(
        decoded.cheque.chequebook,
        "0xcafebabecafebabecafebabecafebabecafebabe"
            .parse::<Address>()
            .unwrap()
    );
    assert_eq!(
        decoded.cheque.beneficiary,
        "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            .parse::<Address>()
            .unwrap()
    );
    assert_eq!(decoded.cheque.cumulative_payout(), U256::MAX);
    assert_eq!(decoded.signature.as_ref(), &[0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn output_has_expected_keys_and_value_types() {
    // Structural assertion: the emitted JSON has the expected keys and value
    // types, not a fixed byte string. Addresses are lowercase 0x strings, the
    // payout is a JSON number, and the signature is a base64 string.
    let signed = sample_cheque(U256::from(1_000_000u64), (0u8..=64).collect());
    let value: Value = serde_json::from_slice(&serde_json::to_vec(&signed).unwrap()).unwrap();
    let obj = value.as_object().unwrap();

    let chequebook = obj.get("Chequebook").unwrap().as_str().unwrap();
    assert!(chequebook.starts_with("0x"));
    assert_eq!(chequebook, chequebook.to_ascii_lowercase());

    let beneficiary = obj.get("Beneficiary").unwrap().as_str().unwrap();
    assert!(beneficiary.starts_with("0x"));
    assert_eq!(beneficiary, beneficiary.to_ascii_lowercase());

    // The payout is a bare JSON number, never a quoted string.
    let payout = obj.get("CumulativePayout").unwrap();
    assert!(payout.is_number(), "payout must be a JSON number");
    assert_eq!(payout.to_string(), "1000000");

    // The signature is a base64 string, never a number array.
    assert!(obj.get("Signature").unwrap().is_string());
}

#[test]
fn payout_spans_full_u256_as_bare_number() {
    // The payout exceeds u64, so it must emit as a bare JSON number, not a quoted
    // string, and round-trip exactly.
    let signed = sample_cheque(U256::MAX, vec![0x01]);
    let json = String::from_utf8(serde_json::to_vec(&signed).unwrap()).unwrap();
    assert!(json.contains(&format!("\"CumulativePayout\":{}", U256::MAX)));

    let decoded: SignedCheque = serde_json::from_slice(json.as_bytes()).unwrap();
    assert_eq!(decoded.cheque.cumulative_payout(), U256::MAX);
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
fn decode_rejects_quoted_payout_string() {
    // The payout must be a bare JSON number, never a quoted string.
    let json = "{\"Chequebook\":\"0x0101010101010101010101010101010101010101\",\"Beneficiary\":\"0x0202020202020202020202020202020202020202\",\"CumulativePayout\":\"123\",\"Signature\":\"\"}";
    assert!(SignedCheque::from_json(json.as_bytes()).is_err());
}
