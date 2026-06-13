//! The lazy VIEW surface: thin, pure readers over the generic event store.
//!
//! Each view folds the relevant `EventTable` rows for one contract in canonical
//! position order, decodes them with the concrete nectar `sol!` types, and
//! answers a query at the consumer's decision point. None is an eager projection
//! (except the postage value-sorted index, which lives in
//! [`store`](crate::store) as a self-healing ordering hint and is read here via
//! [`postage::eviction_candidates`]). Per `CHAIN_REACTIONS_DESIGN.md`, views
//! fire nothing: a consumer reads them when it decides.
//!
//! A decode failure inside a view is scoped to that read (the offending row is
//! skipped), never wedging the indexer cursor, because the store holds the bytes
//! verbatim and the hot path never parsed them.

pub mod chequebook;
pub mod postage;
pub mod redistribution;
pub mod staking;
pub mod swap;
