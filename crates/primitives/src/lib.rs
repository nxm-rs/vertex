use nectar_primitives_traits::BRANCHES;

pub mod bmt;
pub mod chunk;
pub mod distance;
pub mod postage;
pub mod proximity;

const ENCRYPTED_BRANCHES: usize = BRANCHES / 2;
pub(crate) const MAX_PO: usize = 31;
pub(crate) const EXTENDED_PO: usize = MAX_PO + 5;
const MAX_BINS: usize = MAX_PO + 1;
