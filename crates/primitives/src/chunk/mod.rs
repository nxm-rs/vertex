mod bmt_body;
mod content;
mod single_owner;

pub use content::ContentChunk;
pub use single_owner::SingleOwnerChunk;

#[derive(Debug, Eq, PartialEq)]
pub enum Chunk {
    Content(ContentChunk),
    SingleOwner(SingleOwnerChunk),
}
