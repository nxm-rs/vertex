//! The [`Reducer`] trait: a domain's per-contract handler the
//! [`ContractIndexer`](crate::indexer::ContractIndexer) dispatches by tag to
//! maintain eager projection(s). Held as `Box<dyn Reducer<DB>>`, object-safe
//! because it takes the concrete `&DB::TXMut`. A contract needing no eager
//! structure registers no reducer.

use vertex_storage::{Database, DatabaseError};

use crate::store::{EventKey, StoredEvent};
use crate::tag::ContractTag;

/// A domain's per-contract reducer, dispatched by [`ContractTag`], maintaining
/// its eager projection(s).
pub trait Reducer<DB: Database>: Send + Sync {
    /// The contract tag this reducer maintains the projection for; the indexer's
    /// dispatch key. Must match a watched contract in the same registration.
    fn tag(&self) -> ContractTag;

    /// Decode `ev` and fold it into this reducer's typed projection(s).
    ///
    /// Runs in the verbatim `put_event` transaction. A decode miss is a skip
    /// (`Ok(())`); the only error returned is a storage fault.
    fn reduce(&self, tx: &DB::TXMut, key: EventKey, ev: &StoredEvent) -> Result<(), DatabaseError>;

    /// Clear the projection(s) and replay the fold over the surviving rows so the
    /// projection cannot drift from the raw store. `surviving` is this contract's
    /// remaining rows in canonical order.
    fn rebuild(
        &self,
        tx: &DB::TXMut,
        surviving: &[(EventKey, StoredEvent)],
    ) -> Result<(), DatabaseError>;

    /// Forward hook for a per-block conservative prune. Not yet invoked by the
    /// indexer (#317); do not rely on it firing. Default is a no-op.
    fn on_block(&self, _tx: &DB::TXMut, _block: u64) -> Result<(), DatabaseError> {
        Ok(())
    }
}
