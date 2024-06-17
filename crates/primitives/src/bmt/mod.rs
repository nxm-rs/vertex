#![feature(test)]

pub mod chunk;
pub mod file;
pub mod span;

const SEGMENT_SIZE: usize = 32;
const SEGMENT_PAIR_SIZE: usize = 2 * SEGMENT_SIZE;
const HASH_SIZE: usize = 32; // bytes

const DEFAULT_MAX_PAYLOAD_SIZE: usize = 4096; // bytes
const DEFAULT_MIN_PAYLOAD_SIZE: usize = 1;
