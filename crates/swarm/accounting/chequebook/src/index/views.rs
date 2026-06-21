//! Chequebook views: the factory-deployed set by lazy existence fold.
//!
//! A chequebook is deployed once, so [`is_factory_deployed`] and
//! [`deployed_chequebooks`] are a pure existence fold, no supersede logic.

use std::collections::HashSet;

use alloy_primitives::Address;
use alloy_sol_types::SolEvent;
use nectar_contracts::IChequebookFactory;
use vertex_chain_index_framework::fold_events;
use vertex_storage::{Database, DatabaseError};

use crate::index::TAG_CHEQUEBOOK;

/// Fold the factory-deployed chequebook set from the verbatim rows.
fn deployed_set<DB: Database>(db: &DB) -> Result<HashSet<Address>, DatabaseError> {
    fold_events(
        db,
        TAG_CHEQUEBOOK,
        HashSet::new(),
        |set: &mut HashSet<Address>, _key, ev| {
            if ev.topic0 != IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH {
                return;
            }
            if let Ok(e) = IChequebookFactory::SimpleSwapDeployed::decode_log_data(&ev.log_data()) {
                set.insert(e.contractAddress);
            }
        },
    )
}

/// Whether `chequebook` is in the factory-deployed set.
pub fn is_factory_deployed<DB: Database>(
    db: &DB,
    chequebook: Address,
) -> Result<bool, DatabaseError> {
    Ok(deployed_set(db)?.contains(&chequebook))
}

/// Every chequebook the factory has deployed, in unspecified order.
pub fn deployed_chequebooks<DB: Database>(db: &DB) -> Result<Vec<Address>, DatabaseError> {
    Ok(deployed_set(db)?.into_iter().collect())
}
