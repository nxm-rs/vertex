use std::io::{BufRead, BufReader, Cursor};

use alloy_primitives::keccak256;

use crate::bmt::{
    chunk::{Chunk, Options},
    span::Span,
    SEGMENT_SIZE,
};

pub struct ChunkInclusionProof {
    span: Span,
    sister_segments: Vec<Vec<u8>>,
}

pub struct ChunkedFile {
    payload: Vec<u8>,
    span: Span,
    options: Options,
    // reader: &'a mut BufReader<R>,
}

impl ChunkedFile {
    pub fn new(payload: Vec<u8>, options: Options) -> ChunkedFile {
        let payload_length = payload.len();

        ChunkedFile {
            payload,
            span: Span::new(payload_length as u64),
            options,
        }
    }

    // splitter
    pub fn leaf_chunks(&self) -> Vec<Chunk> {
        let mut reader =
            BufReader::with_capacity(self.options.max_payload_size, Cursor::new(&self.payload));

        let mut chunks: Vec<Chunk> = Vec::<Chunk>::new();

        // dump loop for chunking out the buffer
        loop {
            let chunk_length = {
                match reader.fill_buf() {
                    Ok(t) => {
                        let mut chunk_payload = Vec::from(t);
                        let chunk_payload_length = chunk_payload.len();

                        if chunk_payload_length == 0 {
                            break;
                        }

                        chunks.push(Chunk::new(
                            &mut chunk_payload,
                            None,
                            Options::default(),
                            None,
                        ));

                        chunk_payload_length
                    }
                    Err(_e) => 0,
                }
            };

            reader.consume(chunk_length);
        }

        chunks
    }

    pub fn address(&self) -> [u8; 32] {
        Self::bmt_root_chunk(&mut self.leaf_chunks()).address()
    }

    pub fn file_inclusion_proof_bottom_up(
        &self,
        mut segment_index: usize,
    ) -> Vec<ChunkInclusionProof> {
        if segment_index * SEGMENT_SIZE >= self.span.value().try_into().unwrap() {
            panic!(
                "The given segment index {} is greater than {}",
                segment_index,
                self.span.value() as usize / SEGMENT_SIZE
            )
        }

        let mut level_chunks = self.leaf_chunks();

        let max_chunk_payload = level_chunks[0].max_payload_length();
        let max_segment_count = max_chunk_payload / SEGMENT_SIZE; // default 128
        let chunk_bmt_levels = (max_segment_count as f64).log2() as usize;
        let mut carrier_chunk = Self::pop_carrier_chunk(&mut level_chunks);
        let mut chunk_inclusion_proofs = Vec::<ChunkInclusionProof>::new();

        while level_chunks.len() != 1 || carrier_chunk.is_some() {
            let chunk_segment_index = segment_index % max_segment_count;
            let mut chunk_index_for_proof = segment_index / max_segment_count;

            // edge-case carrier chunk
            if chunk_index_for_proof == level_chunks.len() {
                // carrier chunk has been placed to somewhere else in the bmt tree
                if carrier_chunk.is_none() {
                    panic!("Impossible");
                }

                segment_index >>= chunk_bmt_levels; // log2(128) -> skip this level check now
                loop {
                    let (next_level_chunks, next_level_carrier_chunk) =
                        Self::next_bmt_level(&mut level_chunks, carrier_chunk);
                    level_chunks = next_level_chunks;
                    carrier_chunk = next_level_carrier_chunk;

                    segment_index >>= chunk_bmt_levels;

                    if segment_index % max_segment_count != 0 {
                        break;
                    }
                }
                // the carrier chunk is already placed in the BMT tree
                chunk_index_for_proof = level_chunks.len() - 1;
                // continue the inclusion proofing of the inserted carrier chunk address
            }
            let chunk = &level_chunks[chunk_index_for_proof];
            let sister_segments = chunk.inclusion_proof(chunk_segment_index);
            chunk_inclusion_proofs.push(ChunkInclusionProof {
                sister_segments,
                span: chunk.span().clone(),
            });
            segment_index = chunk_index_for_proof;

            let (next_level_chunks, next_level_carrier_chunk) =
                Self::next_bmt_level(&mut level_chunks, carrier_chunk);
            level_chunks = next_level_chunks;
            carrier_chunk = next_level_carrier_chunk;
        }

        let sister_segments = level_chunks[0].inclusion_proof(segment_index);
        chunk_inclusion_proofs.push(ChunkInclusionProof {
            sister_segments,
            span: level_chunks[0].span().clone(),
        });

        chunk_inclusion_proofs
    }

    pub fn file_address_from_inclusion_proof(
        prove_chunks: Vec<ChunkInclusionProof>,
        prove_segment: Vec<u8>,
        mut prove_segment_index: usize,
        max_chunk_payload_byte_length: usize,
    ) -> Vec<u8> {
        let max_segment_count = max_chunk_payload_byte_length / SEGMENT_SIZE; // 128 by default
        let chunk_bmt_levels = (max_segment_count as f64).log2() as usize; // 7 by default

        let file_size = prove_chunks[prove_chunks.len() - 1].span.value();
        let mut last_chunk_index = (file_size - 1) as usize / max_chunk_payload_byte_length;
        let mut calculated_hash = prove_segment;

        for prove_chunk in prove_chunks {
            let (parent_chunk_index, level) = Self::get_bmt_index_of_segment(
                prove_segment_index,
                last_chunk_index,
                max_chunk_payload_byte_length,
            );

            for proof_segment in prove_chunk.sister_segments {
                calculated_hash = match prove_segment_index % 2 == 0 {
                    true => keccak256::<Vec<u8>>(
                        calculated_hash
                            .into_iter()
                            .chain(proof_segment.into_iter())
                            .collect(),
                    )
                    .into_iter()
                    .collect(),
                    false => keccak256::<Vec<u8>>(
                        proof_segment
                            .into_iter()
                            .chain(calculated_hash.into_iter())
                            .collect(),
                    )
                    .into_iter()
                    .collect(),
                };
                prove_segment_index /= 2;
            }
            calculated_hash = keccak256::<Vec<u8>>(
                prove_chunk
                    .span
                    .to_bytes()
                    .into_iter()
                    .chain(calculated_hash.into_iter())
                    .collect(),
            ).to_vec();
            // this line is necessary if the prove_segment_index
            // was in a carrier chunk
            prove_segment_index = parent_chunk_index as usize;
            last_chunk_index >>= chunk_bmt_levels + (level as usize) * chunk_bmt_levels;
        }

        calculated_hash
    }

    pub fn get_bmt_index_of_segment(
        mut segment_index: usize,
        last_chunk_index: usize,
        max_payload_byte_length: usize,
    ) -> (u32, u32) {
        let max_segment_count = max_payload_byte_length / SEGMENT_SIZE; // 128 by default
        let chunk_bmt_levels = (max_segment_count as f64).log2() as usize; // 7 by default

        let mut level = 0;
        if segment_index / max_segment_count == last_chunk_index // the segment is subsumed under the last chunk
            && last_chunk_index % max_segment_count == 0 // the last chunk is a carrier chunk
            && last_chunk_index != 0
        {
            // there is only the root chunk
            // segment_index is carrier chunk
            segment_index >>= chunk_bmt_levels;
            while segment_index % SEGMENT_SIZE == 0 {
                level += 1;
                segment_index >>= chunk_bmt_levels;
            }
        } else {
            segment_index >>= chunk_bmt_levels;
        }

        (segment_index.try_into().unwrap(), level)
    }

    pub fn bmt(&self) -> Vec<Vec<Chunk>> {
        let leaf_chunks = &mut self.leaf_chunks();

        if leaf_chunks.is_empty() {
            panic!("The given chunk vector is empty");
        }

        // data level assign
        let mut level_chunks: Vec<Vec<Chunk>> = Vec::<Vec<Chunk>>::new();
        let mut carrier_chunk = Self::pop_carrier_chunk(leaf_chunks);
        level_chunks.push(leaf_chunks.to_vec());

        while level_chunks[level_chunks.len() - 1].len() != 1 {
            eprintln!("Passing level {}", level_chunks.len());
            let level_chunks_length = level_chunks.len();
            let (next_level_chunks, next_level_carrier_chunk) =
                Self::next_bmt_level(&mut level_chunks[level_chunks_length - 1], carrier_chunk);

            carrier_chunk = next_level_carrier_chunk;
            level_chunks.push(next_level_chunks);
        }

        level_chunks
    }

    pub fn bmt_root_chunk(chunks: &mut Vec<Chunk>) -> Chunk {
        let chunks_length = chunks.len();

        if chunks_length == 0 {
            panic!("The given chunk vector is empty");
        }

        // zero level assign
        let level_chunks = chunks;
        let mut carrier_chunk = Self::pop_carrier_chunk(level_chunks);

        while level_chunks.len() != 1 || carrier_chunk.is_some() {
            (*level_chunks, carrier_chunk) =
                Self::next_bmt_level(level_chunks, carrier_chunk.clone());
        }

        level_chunks[0].clone()
    }

    pub fn next_bmt_level(
        chunks: &mut Vec<Chunk>,
        carrier_chunk: Option<Chunk>,
    ) -> (Vec<Chunk>, Option<Chunk>) {
        let chunks_length = chunks.len();

        if chunks_length == 0 {
            panic!("The given chunk vector is empty");
        }

        let max_payload_length = chunks[0].max_payload_length();
        // max segment count in one chunk. the segment size have to be equal to the chunk addresses
        let max_segment_count = max_payload_length / SEGMENT_SIZE; // 128 by default
        let mut next_level_chunks = Vec::<Chunk>::new();

        let mut offset = 0;

        while offset < chunks_length {
            let end = if (offset + max_segment_count) > chunks.len() {
                chunks.len()
            } else {
                offset + max_segment_count
            };
            let mut children_chunks: Vec<Chunk> = chunks[offset..end].to_vec();
            next_level_chunks.push(Self::create_intermediate_chunk(
                &mut children_chunks,
                Options::default(),
            ));
            offset += max_segment_count;
        }

        // edge case handling when there is carrier_chunk
        let next_level_carrier_chunk = match carrier_chunk {
            Some(chunk) => {
                // try to merge carrier chunk if it first to its parent's payload
                if next_level_chunks.len() % max_segment_count != 0 {
                    next_level_chunks.push(chunk);
                    None
                } else {
                    Some(chunk)
                }
            }
            None => Self::pop_carrier_chunk(&mut next_level_chunks),
        };

        (next_level_chunks, next_level_carrier_chunk)
    }

    pub fn create_intermediate_chunk(chunks: &mut [Chunk], options: Options) -> Chunk {
        // use a fold instead of a map to avoid cloning the chunk addresses
        let n = chunks.len();
        let (mut chunk_addresses, chunk_span_sum_values) = chunks
            .iter_mut()
            .map(|f| (f.address(), f.span().value()))
            .fold(
                (Vec::<u8>::with_capacity(n * 32), 0),
                |(mut prev_addresses, prev_span_sum), (curr_address, curr_span_sum)| {
                    prev_addresses.extend(curr_address);
                    (prev_addresses, prev_span_sum + curr_span_sum)
                },
            );

        Chunk::new(
            &mut chunk_addresses,
            Some(chunk_span_sum_values),
            options,
            None,
        )
    }

    pub fn pop_carrier_chunk(chunks: &mut Vec<Chunk>) -> Option<Chunk> {
        if chunks.len() <= 1 {
            return None;
        }

        let max_segment_count = chunks[0].max_payload_length() / SEGMENT_SIZE;

        match chunks.len() % max_segment_count {
            1 => chunks.pop(),
            _ => None,
        }
    }

    // pub fn new(reader: &mut BufReader<R>, payload_length: u64, options: ChunkOptions) -> ChunkedFile<R>
    // where
    //     R: Read,
    // {
    //     // panic if buffer capacity is misconfigured
    //     if reader.capacity() > options.max_payload_size {
    //         panic!("Invalid buffer size: {} capacity vs {} max payload size", reader.capacity(), options.max_payload_size);
    //     }

    //     ChunkedFile {
    //         reader,
    //         span: Span::new(payload_length),
    //         options,
    //     }
    // }

    // pub fn leaf_chunks(self: &mut Self) -> Vec<Chunk> {
    //     self.collect()
    // }
}

// impl<'a, R> Iterator for ChunkedFile<'a, R>
// where
//     R: Read,
// {
//     type Item = Chunk;

//     fn next(&mut self) -> Option<Self::Item> {
//         let mut buffer = match self.reader.fill_buf() {
//             Ok(buf) => buf.to_vec(),
//             Err(_) => return None,
//         };

//         let length = buffer.len();

//         if length == 0 {
//             None
//         } else {
//             let chunk = Chunk::new(&mut buffer, ChunkOptions::default());
//             self.reader.consume(length);
//             Some(chunk)
//         }
//     }
// }

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Read};

    use alloy_primitives::hex::ToHexExt;
    use super::*;
    // use test::Bencher;

    const EXPECTED_SPAN: [u8; 8] = [3, 0, 0, 0, 0, 0, 0, 0];

    fn setup_carrier_chunk_file() -> (Vec<u8>, usize) {
        // edge case - carrier chunk
        let mut f = File::open("./test-files/carrier-chunk-blob").unwrap();
        let mut payload = Vec::<u8>::new();

        f.read_to_end(&mut payload).ok();

        let file_length = payload.len();

        (payload, file_length)
    }

    fn setup_carrier_chunk_file_2() -> (Vec<u8>, usize) {
        // edge case - carrier chunk - intermediate level
        let mut f = File::open("./test-files/carrier-chunk-blob-2").unwrap();
        let mut payload = Vec::<u8>::new();

        f.read_to_end(&mut payload).ok();

        let file_length = payload.len();

        (payload, file_length)
    }

    fn setup_bos_chunk_file() -> (Vec<u8>, usize) {
        let mut f = File::open("./test-files/bos.pdf").unwrap();
        let mut payload = Vec::<u8>::new();

        f.read_to_end(&mut payload).ok();

        let file_length = payload.len();

        (payload, file_length)
    }

    #[test]
    fn small_file() {
        let payload = vec![1, 2, 3];
        let comp_payload = payload.clone();

        let chunked_file = ChunkedFile::new(payload, Options::default());

        let mut leaf_chunks: Vec<Chunk> = chunked_file.leaf_chunks();

        assert_eq!(leaf_chunks.len(), 1);

        let only_chunk = leaf_chunks.pop().unwrap();

        assert_eq!(
            &comp_payload,
            &(only_chunk.data())[0..only_chunk.span().value() as usize]
        );
        assert_eq!(only_chunk.span().to_bytes(), EXPECTED_SPAN);
        assert_eq!(only_chunk.span().value(), chunked_file.span.value());
        assert_eq!(only_chunk.span().to_bytes(), chunked_file.span.to_bytes());
        assert_eq!(only_chunk.address(), chunked_file.address());
    }

    #[test]
    fn big_file() {
        let (payload, _file_length) = setup_bos_chunk_file();

        let chunked_file = ChunkedFile::new(payload, Options::default());

        let mut leaf_chunks: Vec<Chunk> = chunked_file.leaf_chunks();

        assert_eq!(chunked_file.span.value(), 15726634);
        assert_eq!(chunked_file.span.to_bytes(), [42, 248, 239, 0, 0, 0, 0, 0]);

        let tree = chunked_file.bmt();
        assert_eq!(tree.len(), 3);
        // last level only contains the root chunk
        assert_eq!(tree[2].len(), 1);
        let root_chunk = &tree[2][0];

        let second_level_first_chunk = &tree[1][0]; // first intermediate chunk on the first intermediate chunk level
        assert_eq!(
            second_level_first_chunk.span().value() as usize,
            4096 * (4096 / SEGMENT_SIZE)
        ); // 524288

        assert_eq!(root_chunk.data()[0..32], second_level_first_chunk.address());
        assert_eq!(second_level_first_chunk.data().len(), 4096);

        // encapsulated address has to be the same to the corresponding children chunk's address
        assert_eq!(second_level_first_chunk.data()[0..32], tree[0][0].address());

        // last rootchunk data
        assert_eq!(
            ChunkedFile::bmt_root_chunk(&mut leaf_chunks).payload_length,
            960
        );

        assert_eq!(
            chunked_file.address().encode_hex(),
            "b8d17f296190ccc09a2c36b7a59d0f23c4479a3958c3bb02dc669466ec919c5d"
        );
    }

    #[test]
    fn find_bmt_position_of_payload_segment_index() {
        let (payload, file_length) = setup_carrier_chunk_file();

        let chunked_file = ChunkedFile::new(payload, Options::default());
        let mut leaf_chunks = chunked_file.leaf_chunks();
        let tree = chunked_file.bmt();

        // check whether the last chunk is not present in the BMT tree 0 level -> carrier chunk
        assert_eq!(tree[0].len(), leaf_chunks.len() - 1);

        let carrier_chunk = leaf_chunks.pop().unwrap();
        let segment_index = (file_length - 1) / 32; // last segment index as well
        let last_chunk_index = (file_length - 1) / 4096;
        let segment_id_in_tree =
            ChunkedFile::get_bmt_index_of_segment(segment_index, last_chunk_index, 4096);

        assert_eq!(segment_id_in_tree.0, 1);
        assert_eq!(segment_id_in_tree.1, 1);
        assert_eq!(
            tree[segment_id_in_tree.0 as usize][segment_id_in_tree.1 as usize].address(),
            carrier_chunk.address()
        );
    }

    #[test]
    fn should_collect_required_segments_for_inclusion_proof() {
        let (payload, file_length) = setup_carrier_chunk_file();

        let chunked_file = ChunkedFile::new(payload, Options::default());
        let file_hash = chunked_file.address();

        // segment to prove
        let segment_index = (file_length - 1) / 32;

        // check segment array length for carrier chunk inclusion proof
        let proof_chunks =
            ChunkedFile::file_inclusion_proof_bottom_up(&chunked_file, segment_index);
        assert_eq!(proof_chunks.len(), 2); // 1 level is skipped because the segment was in a carrier chunk

        // gives back the file hash calculated from the inclusion proof method
        let test_get_file_hash = |idx: usize, payload: &Vec<u8>| -> Vec<u8> {
            let proof_chunks = chunked_file.file_inclusion_proof_bottom_up(idx);
            let end = if ((idx * SEGMENT_SIZE) + SEGMENT_SIZE) > payload.len() {
                payload.len()
            } else {
                idx * SEGMENT_SIZE + SEGMENT_SIZE
            };
            let mut prove_segment: Vec<u8> = payload[idx * SEGMENT_SIZE..end].to_vec();

            // padding
            prove_segment.resize(SEGMENT_SIZE, 0);

            // check the last segment has the correct span value
            let file_size_from_proof = proof_chunks[proof_chunks.len() - 1].span.value();
            assert_eq!(file_size_from_proof, file_length as u64);

            ChunkedFile::file_address_from_inclusion_proof(proof_chunks, prove_segment, idx, 4096)
        };

        // edge case
        assert_eq!(
            test_get_file_hash(segment_index, &chunked_file.payload),
            file_hash
        );
        assert_eq!(test_get_file_hash(1000, &chunked_file.payload), file_hash);
    }

    #[test]
    fn should_collect_required_segments_for_inclusion_proof_2() {
        let (payload, file_length) = setup_bos_chunk_file();

        let chunked_file = ChunkedFile::new(payload, Options::default());
        let file_hash = chunked_file.address();

        // segment to prove
        let last_segment_index = (file_length - 1) / 32;

        // gives back the file hash calculated from the inclusion proof method
        let test_get_file_hash = |idx: usize, payload: &Vec<u8>| -> Vec<u8> {
            let proof_chunks = chunked_file.file_inclusion_proof_bottom_up(idx);
            let end = if ((idx * SEGMENT_SIZE) + SEGMENT_SIZE) > payload.len() {
                payload.len()
            } else {
                idx * SEGMENT_SIZE + SEGMENT_SIZE
            };
            let mut prove_segment: Vec<u8> = payload[idx * SEGMENT_SIZE..end].to_vec();

            // padding
            prove_segment.resize(SEGMENT_SIZE, 0);

            // check the last segment has the correct span value
            let file_size_from_proof = proof_chunks[proof_chunks.len() - 1].span.value();
            assert_eq!(file_size_from_proof, file_length as u64);

            ChunkedFile::file_address_from_inclusion_proof(proof_chunks, prove_segment, idx, 4096)
        };

        // edge case
        assert_eq!(
            test_get_file_hash(last_segment_index, &chunked_file.payload),
            file_hash
        );
        assert_eq!(test_get_file_hash(1000, &chunked_file.payload), file_hash);
    }

    #[test]
    fn should_collect_required_segments_for_inclusion_proof_3() {
        let (payload, file_length) = setup_carrier_chunk_file_2();

        assert_eq!(file_length, 67117056);
        let chunked_file = ChunkedFile::new(payload, Options::default());
        let file_hash = chunked_file.address();
        // segment to prove
        let last_segment_index = (file_length - 1) / 32;

        // gives back the file hash calculated from the inclusion proof method
        let test_get_file_hash = |idx: usize, payload: &Vec<u8>| -> Vec<u8> {
            let proof_chunks = chunked_file.file_inclusion_proof_bottom_up(idx);
            let end = if ((idx * SEGMENT_SIZE) + SEGMENT_SIZE) > payload.len() {
                payload.len()
            } else {
                idx * SEGMENT_SIZE + SEGMENT_SIZE
            };
            let mut prove_segment: Vec<u8> = payload[idx * SEGMENT_SIZE..end].to_vec();

            // padding
            prove_segment.resize(SEGMENT_SIZE, 0);

            // check the last segment has the correct span value
            let file_size_from_proof = proof_chunks[proof_chunks.len() - 1].span.value();
            assert_eq!(file_size_from_proof, file_length as u64);

            ChunkedFile::file_address_from_inclusion_proof(proof_chunks, prove_segment, idx, 4096)
        };
        // edge case
        assert_eq!(
            test_get_file_hash(last_segment_index, &chunked_file.payload),
            file_hash
        );
        assert_eq!(test_get_file_hash(1000, &chunked_file.payload), file_hash);
        // expect(() => testGetFileHash(lastSegmentIndex + 1)).toThrowError(/^The given segment index/)
    }

    // #[bench]
    // fn find_bmt_position_of_payload_segment_index_bench(b: &mut Bencher) {
    //     let (payload, file_length) = setup_carrier_chunk_file();

    //     let chunked_file = ChunkedFile::new(payload, Options::default());

    //     b.iter(|| {
    //         let mut leaf_chunks = chunked_file.leaf_chunks();
    //         let tree = chunked_file.bmt();

    //         // check whether the last chunk is not present in the BMT tree 0 level -> carrier chunk
    //         assert_eq!(tree[0].len(), leaf_chunks.len() - 1);

    //         let carrier_chunk = leaf_chunks.pop().unwrap();
    //         let segment_index = (file_length - 1) / 32; // last segment index as well
    //         let last_chunk_index = (file_length - 1) / 4096;
    //         let segment_id_in_tree =
    //             ChunkedFile::get_bmt_index_of_segment(segment_index, last_chunk_index, 4096);

    //         assert_eq!(segment_id_in_tree.0, 1);
    //         assert_eq!(segment_id_in_tree.1, 1);
    //         assert_eq!(
    //             tree[segment_id_in_tree.0 as usize][segment_id_in_tree.1 as usize].address(),
    //             carrier_chunk.address()
    //         );
    //     })
    // }
}
