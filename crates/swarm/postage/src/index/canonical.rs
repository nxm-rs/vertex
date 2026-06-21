//! Canonical PostageStamp deployment, pinned locally because the nectar address
//! diverges from the live deployment (see FIXME(#315)). Flat consts (not the
//! nested `mainnet`/`testnet` modules the multi-contract domains use) since
//! postage watches one contract.

use alloy_primitives::{Address, address};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Deployment {
    pub(crate) address: Address,
    /// Deployment block; backfill starts here.
    pub(crate) block: u64,
}

/// Gnosis Chain mainnet deployment.
// FIXME(#315): override until nectar fixed; then source from nectar.
pub(crate) const MAINNET: Deployment = Deployment {
    address: address!("45a1502382541Cd610CC9068e88727426b696293"),
    block: 31_305_656,
};

/// Sepolia testnet deployment.
// FIXME(#315): override until nectar fixed; then source from nectar.
pub(crate) const TESTNET: Deployment = Deployment {
    address: address!("cdfdC3752caaA826fE62531E0000C40546eC56A6"),
    block: 6_596_277,
};
