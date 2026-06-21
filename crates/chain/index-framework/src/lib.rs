//! Configuration-driven contract-indexing mechanism. Names no contract, ABI,
//! address, or projection of its own; each domain registers its watched
//! contracts, `sol!` events, optional [`Reducer`], and projection tables, and the
//! node builder composes them into one [`ContractIndexer`] (one filter, one
//! cursor, one position-keyed store). Native, runs behind a `chain` feature,
//! stays out of the wasm cone.

mod indexer;
mod store;

pub mod projection;
pub mod reducer;
pub mod registration;
pub mod tag;

/// Stable re-export the `projection!` / `secondary_index!` macros expand through.
#[doc(hidden)]
pub use vertex_storage as __vertex_storage;

pub use indexer::{ContractIndexer, INDEXER_NAME};
pub use projection::{
    IndexedProjection, Projection, contains, fold_events, get_via_index, last_event, list_all,
    list_by, point_get, range_head, scalar,
};
pub use reducer::Reducer;
pub use registration::{
    DomainRegistration, EventDescriptor, Network, RegistrationError, WatchedContract,
};
pub use store::{EventKey, EventTable, MAX_EVENT_DATA, StoredEvent, events_of};
pub use tag::ContractTag;

#[cfg(test)]
mod tests;
