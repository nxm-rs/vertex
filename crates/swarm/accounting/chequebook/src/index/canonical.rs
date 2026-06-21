//! Canonical chequebook-factory deployment, pinned locally so the domain sources
//! its address from one place; the address agrees with nectar (see FIXME(#315)).

use alloy_primitives::{Address, address};

/// `(address, start_block)` for a canonical deployment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deployment {
    /// The contract address.
    pub(crate) address: Address,
    /// The deployment block; backfill starts here.
    pub(crate) block: u64,
}

/// Gnosis Chain mainnet deployment.
pub(crate) mod mainnet {
    use super::{Deployment, address};

    /// Chequebook factory / SimpleSwapFactory (`MainnetChequebookFactory`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const CHEQUEBOOK_FACTORY: Deployment = Deployment {
        address: address!("c2d5a532cf69aa9a1378737d8ccdef884b6e7420"),
        block: 39_939_970,
    };
}

/// Sepolia testnet deployment.
pub(crate) mod testnet {
    use super::{Deployment, address};

    /// Chequebook factory / SimpleSwapFactory (`TestnetChequebookFactory`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const CHEQUEBOOK_FACTORY: Deployment = Deployment {
        address: address!("0fF044F6bB4F684a5A149B46D7eC03ea659F98A1"),
        block: 4_752_810,
    };
}
