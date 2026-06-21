//! Contracts as data: the [`WatchedContract`] / [`EventDescriptor`] model and the
//! [`DomainRegistration`] each domain hands the
//! [`ContractIndexer`](crate::indexer::ContractIndexer). The framework validates
//! uniqueness across registrations and inits the union of their tables.

use alloy_primitives::{Address, B256};

use crate::reducer::Reducer;
use crate::tag::ContractTag;
use vertex_storage::Database;

/// One event a contract emits: its `topic0` (`E::SIGNATURE_HASH`) and a human
/// label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDescriptor {
    /// The event's `topic0` (`E::SIGNATURE_HASH`).
    pub topic0: B256,
    /// A human label for the stored row and metrics.
    pub name: &'static str,
}

/// A contract to watch: its tag, address, deployment block, and event set.
///
/// A value, not a type. The combined
/// [`ContractIndexer`](crate::indexer::ContractIndexer) filter is the union of
/// every watched contract's address and every descriptor's `topic0`.
#[derive(Debug, Clone, Copy)]
pub struct WatchedContract {
    /// The stable, domain-allocated contract tag (the on-disk key prefix).
    pub tag: ContractTag,
    /// The contract address.
    pub address: Address,
    /// The deployment block; backfill starts here.
    pub start_block: u64,
    /// The events this contract emits that the indexer records.
    pub events: &'static [EventDescriptor],
}

impl WatchedContract {
    /// Whether `topic0` is an event this contract declares.
    pub fn declares(&self, topic0: B256) -> bool {
        self.events.iter().any(|e| e.topic0 == topic0)
    }
}

/// The settlement network whose address book a domain's `registration` is built
/// from. The framework carries it only so domains share one selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// Gnosis Chain mainnet.
    Mainnet,
    /// Sepolia testnet.
    Testnet,
}

/// One domain's contribution to the unified indexer: the contracts it watches,
/// the reducers that maintain its eager projections, and the projection table
/// names it persists.
///
/// A domain exposes `registration(network) -> DomainRegistration<DB>`; the node
/// builder collects every domain's registration and hands the vector to
/// [`ContractIndexer::from_registrations`](crate::indexer::ContractIndexer::from_registrations),
/// which validates uniqueness and initializes the union of `tables`.
pub struct DomainRegistration<DB: Database> {
    /// The contracts this domain watches (address-as-authority + event topics).
    pub contracts: Vec<WatchedContract>,
    /// The reducers maintaining this domain's eager projections. A verbatim-only
    /// domain leaves this empty.
    pub reducers: Vec<Box<dyn Reducer<DB>>>,
    /// The projection table names this domain persists, for one-shot init. The
    /// framework adds the shared `EventTable` and the engine cursor table on top.
    pub tables: &'static [&'static str],
}

/// A registration set that failed the
/// [`from_registrations`](crate::indexer::ContractIndexer::from_registrations)
/// startup checks.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    /// Two watched contracts declared the same [`ContractTag`]; the on-disk key
    /// prefix would alias their event streams.
    #[error("duplicate contract tag {0:?} across registrations")]
    DuplicateTag(ContractTag),
    /// Two watched contracts declared the same [`Address`]; `apply` could not
    /// resolve which contract a log belongs to.
    #[error("duplicate watched address {0} across registrations")]
    DuplicateAddress(Address),
    /// Two registrations declared the same projection table name; table `init`
    /// does not dedup, so the second would silently share the first's storage.
    #[error("duplicate projection table name {0:?} across registrations")]
    DuplicateTableName(&'static str),
    /// A reducer's [`tag`](crate::reducer::Reducer::tag) is not among the watched
    /// contracts' tags, so it could never be dispatched.
    #[error("reducer tag {0:?} has no matching watched contract")]
    TagReducerMismatch(ContractTag),
    /// Creating the initial set of tables failed: an unrecoverable storage fault.
    #[error("failed to initialize indexer tables: {0}")]
    TableInit(#[from] vertex_storage::DatabaseError),
}
