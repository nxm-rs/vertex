pub enum ChunkType {
    ContentAddress,
    SingleOwnerChunk,
}

//impl Display for ChunkType {
//
//}

//pub trait Chunk {
//    fn address(&self) -> Address;
//    fn data(&self) -> &[u8; CHUNK_SIZE as usize];
//    fn tag_id(&self) -> u32;
//    fn with_tag_id(&mut self, id: u32) -> Self;
//    fn stamp(&self) -> Stamp;
//    fn with_stamp(&mut self, s: Stamp) -> Self;
//    fn depth(&self) -> u8;
//    fn bucket_depth(&self) -> u8;
//    fn immutable(&self) -> bool;
//    fn with_batch(&mut self, bucket_depth: u8, immutable: bool) -> Self;
//    fn equal(&self, c: &Self) -> bool;
//}
