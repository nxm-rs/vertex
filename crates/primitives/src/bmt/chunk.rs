use std::fmt::{Display, Formatter};

use alloy_primitives::{hex::ToHexExt, keccak256};

use crate::bmt::{DEFAULT_MAX_PAYLOAD_SIZE, DEFAULT_MIN_PAYLOAD_SIZE, HASH_SIZE, SEGMENT_PAIR_SIZE, SEGMENT_SIZE, span::Span};

#[derive(Debug, Clone)]
pub struct Options {
    pub max_payload_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_payload_size: DEFAULT_MAX_PAYLOAD_SIZE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    data: Vec<u8>,
    pub payload_length: usize,
    span: Span,
    options: Options,
    stamp: Option<[u8; 113]>,
}

impl Chunk {
    pub fn new(
        data: &mut Vec<u8>,
        starting_span_value: Option<u64>,
        options: Options,
        stamp: Option<[u8; 113]>,
    ) -> Chunk {
        let payload_length: usize = data.len();

        let data: Vec<u8> = match payload_length {
            DEFAULT_MIN_PAYLOAD_SIZE..=DEFAULT_MAX_PAYLOAD_SIZE => {
                // resize to max payload size (zero-padding)
                if data.len() != options.max_payload_size {
                    data.resize(options.max_payload_size, 0);
                }
                data[..].to_vec()
            }
            _ => panic!(
                "Payload must be a minimum of {} bytes, and a maximum of {} bytes",
                DEFAULT_MIN_PAYLOAD_SIZE, DEFAULT_MAX_PAYLOAD_SIZE
            ),
        };

        Chunk {
            data,
            payload_length,
            options,
            span: Span::new(match starting_span_value {
                Some(value) => value,
                None => payload_length as u64,
            }),
            stamp,
        }
    }

    pub fn data(&self) -> &Vec<u8> {
        &self.data
    }

    pub fn max_payload_length(&self) -> usize {
        self.options.max_payload_size
    }

    pub fn span(&self) -> &Span {
        &self.span
    }

    pub fn stamp(&self) -> Option<[u8; 113]> {
        self.stamp
    }

    pub fn add_stamp(&mut self, stamp: [u8; 113]) -> &mut Chunk {
        self.stamp = Some(stamp);

        self
    }

    pub fn address(&self) -> [u8; 32] {
        let mut hash_input: Vec<u8> = self.span().to_bytes().into_iter().collect();
        hash_input.extend(&self.bmt_root_hash());

        *keccak256(hash_input)
    }

    pub fn inclusion_proof(&self, mut segment_index: usize) -> Vec<Vec<u8>> {
        let payload_length = self.data().len();

        if segment_index * SEGMENT_SIZE >= payload_length {
            panic!(
                "The given segment index {} is greater than {}",
                segment_index,
                (payload_length / SEGMENT_SIZE) - 1
            )
        }

        let tree = self.bmt();
        let mut sister_segments: Vec<Vec<u8>> = Vec::new();

        for level in tree.iter().take(tree.len() - 1) {
            let sister_segment_index = match segment_index % 2 == 0 {
                true => segment_index + 1,
                false => segment_index - 1,
            };

            sister_segments.push(
                level[sister_segment_index * SEGMENT_SIZE
                    ..(sister_segment_index + 1) * SEGMENT_SIZE]
                    .to_vec(),
            );

            segment_index >>= 1;
        }

        sister_segments
    }

    pub fn root_hash_from_inclusion_proof(
        &self,
        proof_segments: Vec<Vec<u8>>,
        prove_segment: Vec<u8>,
        mut prove_segment_index: u32,
    ) -> Vec<u8> {
        let mut calculated_hash = prove_segment;
        for mut proof_segment in proof_segments {
            calculated_hash = match prove_segment_index % 2 == 0 {
                true => {
                    calculated_hash.extend(proof_segment);
                    keccak256(calculated_hash).to_vec()
                }
                false => {
                    proof_segment.extend(calculated_hash);
                    keccak256(proof_segment).to_vec()
                }
            };

            prove_segment_index >>= 1;
        }

        calculated_hash
    }

    pub fn bmt(&self) -> Vec<Vec<u8>> {
        let mut input: Vec<u8> = self.data.to_vec();
        let mut tree: Vec<Vec<u8>> = Vec::new();
        loop {
            tree.push(input.clone());
            let num_pairs = input.len() / SEGMENT_PAIR_SIZE;
            let mut output: Vec<u8> = Vec::<u8>::with_capacity(num_pairs);

            // in each round we hash the segment pairs together
            for pair in 0..num_pairs {
                let mut hash: Vec<u8> =
                    keccak256(&input[pair * SEGMENT_PAIR_SIZE..(pair + 1) * SEGMENT_PAIR_SIZE])
                        .into_iter()
                        .collect();
                output.append(&mut hash);
            }

            input = output;

            if input.len() == HASH_SIZE {
                break;
            }
        }

        // add the last "input" that is the bmt root hash of the application
        tree.push(input);

        tree
    }

    pub fn bmt_root_hash(&self) -> Vec<u8> {
        let mut input: Vec<u8> = self.data.to_vec();
        loop {
            let num_pairs = input.len() / SEGMENT_PAIR_SIZE;
            let mut output: Vec<u8> = Vec::<u8>::with_capacity(num_pairs);

            // in each round we hash the segment pairs together
            for pair in 0..num_pairs {
                let mut hash: Vec<u8> =
                    keccak256(&input[pair * SEGMENT_PAIR_SIZE..(pair + 1) * SEGMENT_PAIR_SIZE])
                        .into_iter()
                        .collect();
                output.append(&mut hash);
            }

            input = output;

            if input.len() == HASH_SIZE {
                return input;
            }
        }
    }
}

impl Display for Chunk {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunk address {}", self.address().encode_hex())
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::hex::ToHexExt;

    use super::*;

    const EXPECTED_SPAN: [u8; 8] = [3, 0, 0, 0, 0, 0, 0, 0];

    fn setup() -> Chunk {
        let mut payload: Vec<u8> = vec![1, 2, 3];

        Chunk::new(&mut payload, None, Options::default(), None)
    }

    #[test]
    fn bmt_hash() {
        let chunk = setup();
        assert_eq!(chunk.span().to_bytes(), EXPECTED_SPAN);
        assert_eq!(
            chunk.address().encode_hex(),
            "ca6357a08e317d15ec560fef34e4c45f8f19f01c372aa70f1da72bfa7f1a4338"
        );
    }

    #[test]
    fn bmt_tree() {
        let chunk = setup();
        let tree = chunk.bmt();

        let mut to_hash = Vec::from(chunk.span.to_bytes());
        to_hash.extend(tree[tree.len() - 1].clone().into_iter());

        assert_eq!(tree.len(), 8);
        assert_eq!(keccak256(to_hash).to_vec(), chunk.address());
    }

    #[test]
    fn inclusion_proof() {
        let chunk = setup();
        let tree = chunk.bmt();

        assert_eq!(tree.len(), 8);

        let test_get_root_hash = |segment_index: u32| -> Vec<u8> {
            let inclusion_proof_segments = chunk.inclusion_proof(segment_index as usize);
            let idx = segment_index as usize * SEGMENT_SIZE;
            chunk
                .root_hash_from_inclusion_proof(
                    inclusion_proof_segments,
                    chunk.data()[idx..idx + SEGMENT_SIZE].to_vec(),
                    segment_index,
                )
                .to_vec()
        };

        let root_hash = test_get_root_hash(0);
        let mut to_hash = Vec::from(chunk.span().to_bytes());
        to_hash.extend(&root_hash);

        assert_eq!(keccak256(to_hash).to_vec(), chunk.address());

        assert_eq!(root_hash, test_get_root_hash(101));
        assert_eq!(root_hash, test_get_root_hash(127));
    }

    #[test]
    #[should_panic(expected = "The given segment index 128 is greater than 127")]
    fn inclusive_proof_invalid_segment_index() {
        let chunk = setup();
        chunk.inclusion_proof(128);
    }

    #[test]
    fn bee_inclusion_proofs() {
        let mut payload = Vec::from(String::from("hello world").as_bytes());

        let chunk = Chunk::new(&mut payload, None, Options::default(), None);
        let inclusion_proof_segments: Vec<String> = chunk
            .inclusion_proof(0)
            .into_iter()
            .map(|x| x.encode_hex())
            .collect();

        assert_eq!(
            inclusion_proof_segments,
            vec![
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "887c22bd8750d34016ac3c66b5ff102dacdd73f6b014e710b51e8022af9a1968",
            ]
        );

        let inclusion_proof_segments: Vec<String> = chunk
            .inclusion_proof(127)
            .into_iter()
            .map(|x| x.encode_hex())
            .collect();

        assert_eq!(
            inclusion_proof_segments,
            vec![
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "745bae095b6ff5416b4a351a167f731db6d6f5924f30cd88d48e74261795d27b",
            ]
        );

        let inclusion_proof_segments: Vec<String> = chunk
            .inclusion_proof(64)
            .into_iter()
            .map(|x| x.encode_hex())
            .collect();

        assert_eq!(
            inclusion_proof_segments,
            vec![
                "0000000000000000000000000000000000000000000000000000000000000000",
                "ad3228b676f7d3cd4284a5443f17f1962b36e491b30a40b2405849e597ba5fb5",
                "b4c11951957c6f8f642c4af61cd6b24640fec6dc7fc607ee8206a99e92410d30",
                "21ddb9a356815c3fac1026b6dec5df3124afbadb485c9ba5a3e3398a04b7ba85",
                "e58769b32a1beaf1ea27375a44095a0d1fb664ce2dd358e7fcbfb78c26a19344",
                "0eb01ebfc9ed27500cd4dfc979272d1f0913cc9f66540d7e8005811109e1cf2d",
                "745bae095b6ff5416b4a351a167f731db6d6f5924f30cd88d48e74261795d27b",
            ]
        );
    }
}
