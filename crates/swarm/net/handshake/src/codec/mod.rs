//! Encoding and decoding functions for handshake protocol messages.

mod ack;
#[path = "syn.rs"]
mod syn_msg;
mod synack;

#[cfg(feature = "arbitrary")]
pub(crate) use ack::encode_swarm_peer;
pub(crate) use ack::{decode_ack, encode_ack};
pub(crate) use syn_msg::{decode_syn, encode_syn};
pub(crate) use synack::{decode_synack, encode_synack};

#[cfg(test)]
mod seed_replay {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use libp2p::Multiaddr;
    use quick_protobuf::{BytesReader, MessageRead};
    use vertex_swarm_peer::{ARBITRARY_NETWORK_ID, SwarmNodeType, SwarmPeer};

    use super::*;
    use crate::MAX_HANDSHAKE_BUFFER_SIZE;

    /// Raw-bytes decode of one frame kind; the same generated-reader plus
    /// domain-decode path the `handshake_decode` fuzz target drives.
    fn decode_seed<W, T>(
        data: &[u8],
        decode: impl FnOnce(W) -> Result<T, crate::HandshakeError>,
    ) -> Option<T>
    where
        W: for<'a> MessageRead<'a>,
    {
        let mut reader = BytesReader::from_bytes(data);
        let wire = W::from_reader(&mut reader, data).ok()?;
        decode(wire).ok()
    }

    fn decode_seed_syn(data: &[u8]) -> Option<Multiaddr> {
        decode_seed(data, decode_syn)
    }

    fn decode_seed_ack(data: &[u8]) -> Option<(SwarmPeer, SwarmNodeType, String)> {
        decode_seed(data, |wire| decode_ack(wire, ARBITRARY_NETWORK_ID))
    }

    fn decode_seed_synack(data: &[u8]) -> Option<(Multiaddr, SwarmPeer, SwarmNodeType, String)> {
        decode_seed(data, |wire| decode_synack(wire, ARBITRARY_NETWORK_ID))
    }

    /// Replays the committed fuzz seeds through the exact decode path the
    /// `handshake_decode` fuzz target drives, so the stable test gate proves
    /// the seeds stay panic-free without the fuzzer. The `budget` seeds pin
    /// the frame-budget boundary: the domain decode accepts all three sizes,
    /// so only the framed codec's `MAX_HANDSHAKE_BUFFER_SIZE` cap rejects the
    /// over-budget frame in production.
    #[test]
    fn seed_replay_handshake_decode() {
        let seed_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../fuzz/seeds/handshake_decode");
        let mut replayed = 0usize;
        for entry in std::fs::read_dir(&seed_dir)
            .unwrap_or_else(|e| panic!("seed dir {} must exist: {e}", seed_dir.display()))
        {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let data = std::fs::read(&path).unwrap();

            let syn = decode_seed_syn(&data);
            let ack = decode_seed_ack(&data);
            let synack = decode_seed_synack(&data);

            if name.starts_with("valid-syn-") {
                assert!(syn.is_some(), "seed {name} must decode as a syn");
            } else if name.starts_with("invalid-syn-") {
                assert!(syn.is_none(), "seed {name} must stay a syn Err");
            } else if name.starts_with("valid-ack-") || name.starts_with("edge-ack-") {
                assert!(ack.is_some(), "seed {name} must decode as an ack");
            } else if name.starts_with("invalid-ack-") {
                assert!(ack.is_none(), "seed {name} must stay an ack Err");
            } else if name.starts_with("valid-synack-") {
                assert!(synack.is_some(), "seed {name} must decode as a synack");
            } else {
                panic!("seed {name} matches no known prefix");
            }

            // The boundary seeds carry their frame size in the name; pin it
            // against the framed codec's cap so the trio keeps straddling it.
            if name.contains("-budget-under-") {
                assert_eq!(data.len(), MAX_HANDSHAKE_BUFFER_SIZE - 1, "seed {name}");
            } else if name.contains("-budget-at-") {
                assert_eq!(data.len(), MAX_HANDSHAKE_BUFFER_SIZE, "seed {name}");
            } else if name.contains("-budget-over-") {
                assert_eq!(data.len(), MAX_HANDSHAKE_BUFFER_SIZE + 1, "seed {name}");
            }
            replayed += 1;
        }
        assert!(
            replayed >= 10,
            "expected at least the 10 curated seeds, found {replayed}"
        );
    }
}
