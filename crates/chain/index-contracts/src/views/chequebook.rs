//! Chequebook view: the factory-deployed set, by lazy existence fold.
//!
//! `is_factory_deployed(chequebook)` folds the `SimpleSwapDeployed` rows and
//! tests membership. A chequebook is deployed exactly once, so this is a pure
//! existence fold over a single decode pass; no supersede logic, no second
//! table. A consumer validates a cheque's chequebook at its decision point by
//! reading this; the indexer pushes no reaction.

use std::collections::HashSet;

use alloy_primitives::Address;
use alloy_sol_types::SolEvent;
use nectar_contracts::IChequebookFactory;
use vertex_storage::{Database, DatabaseError};

use crate::registry::ContractId;
use crate::store::events_of;

/// Fold the factory-deployed chequebook set from the verbatim rows.
fn deployed_set<DB: Database>(db: &DB) -> Result<HashSet<Address>, DatabaseError> {
    let mut set = HashSet::new();
    for (_key, ev) in events_of(db, ContractId::ChequebookFactory)? {
        if ev.topic0 != IChequebookFactory::SimpleSwapDeployed::SIGNATURE_HASH {
            continue;
        }
        if let Ok(e) = IChequebookFactory::SimpleSwapDeployed::decode_log_data(&ev.log_data()) {
            set.insert(e.contractAddress);
        }
    }
    Ok(set)
}

/// Whether `chequebook` is in the factory-deployed set.
pub fn is_factory_deployed<DB: Database>(
    db: &DB,
    chequebook: Address,
) -> Result<bool, DatabaseError> {
    Ok(deployed_set(db)?.contains(&chequebook))
}

/// Every chequebook the factory has deployed.
pub fn deployed_chequebooks<DB: Database>(db: &DB) -> Result<Vec<Address>, DatabaseError> {
    Ok(deployed_set(db)?.into_iter().collect())
}
