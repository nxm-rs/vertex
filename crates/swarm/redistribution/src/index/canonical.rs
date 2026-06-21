//! Canonical Redistribution + StakeRegistry deployments, pinned locally because
//! the nectar addresses diverge from the live deployment (see FIXME(#315)).

use alloy_primitives::{Address, address};

/// `(address, start_block)` for a canonical deployment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deployment {
    /// The contract address.
    pub(crate) address: Address,
    /// The deployment block; backfill starts here.
    pub(crate) block: u64,
}

/// Gnosis Chain mainnet deployments.
pub(crate) mod mainnet {
    use super::{Deployment, address};

    /// StakeRegistry (`MainnetStakingAddress`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const STAKING: Deployment = Deployment {
        address: address!("da2a16EE889E7F04980A8d597b48c8D51B9518F4"),
        block: 40_430_237,
    };

    /// Redistribution (`MainnetRedistributionAddress`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const REDISTRIBUTION: Deployment = Deployment {
        address: address!("5069cdfB3D9E56d23B1cAeE83CE6109A7E4fd62d"),
        block: 41_105_199,
    };
}

/// Sepolia testnet deployments.
pub(crate) mod testnet {
    use super::{Deployment, address};

    /// StakeRegistry (`TestnetStakingAddress`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const STAKING: Deployment = Deployment {
        address: address!("EEF13Ef9eD9cDD169701eeF3cd832df298dD1bB4"),
        block: 8_262_529,
    };

    /// Redistribution (`TestnetRedistributionAddress`).
    // FIXME(#315): override until nectar fixed; then source from nectar.
    pub(crate) const REDISTRIBUTION: Deployment = Deployment {
        address: address!("5b718E36F5Ce2F2F7e25A397040436Ce6af3e89e"),
        block: 8_646_721,
    };
}
