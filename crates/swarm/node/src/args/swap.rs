//! SWAP settlement CLI arguments and validated configuration.
//!
//! These are plain knobs (a chequebook address, a beneficiary payout address, and
//! a deploy toggle) that exist in every build. The arguments name no chain types,
//! so the surface is identical whether or not the binary is compiled with the
//! optional `swap` feature; a build without the feature simply ignores them. The
//! builder reads these only under the `swap` feature, and only when the bandwidth
//! mode enables SWAP. The settlement chain and contract addresses come from the
//! network spec; the RPC endpoint is the shared `--chain.rpc-url`.

use alloy_primitives::Address;
use clap::Args;
use serde::{Deserialize, Serialize};

/// SWAP settlement CLI arguments.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Swap")]
#[serde(default)]
pub struct SwapArgs {
    /// Enable SWAP settlement (chequebook payments). Equivalent to selecting a
    /// SWAP-capable bandwidth mode. Has no effect unless the binary was built with
    /// the `swap` feature.
    #[arg(long = "swap")]
    #[serde(default)]
    pub enable: bool,

    /// This node's chequebook contract address (the drawer of cheques we issue).
    /// Required to issue cheques when SWAP is enabled and a chequebook is not
    /// being deployed.
    #[arg(long = "swap.chequebook")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chequebook: Option<Address>,

    /// Payout address that cheques sent to this node must name (where received
    /// funds are paid). Defaults to the node Ethereum address when unset.
    #[arg(long = "swap.beneficiary")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beneficiary: Option<Address>,

    /// Deploy a new chequebook on startup instead of using an existing one.
    #[arg(long = "swap.deploy")]
    #[serde(default)]
    pub deploy: bool,

    /// Per-peer cap on uncashed cheque exposure, in cumulative-payout units.
    /// A received cheque settles debt on structural validation while on-chain
    /// cashing is stubbed for v1, so this bounds how much uncashed value can buy
    /// real service before crediting for the peer stops. Defaults to ten times
    /// the default payment threshold.
    #[arg(long = "swap.bounce-limit", default_value_t = DEFAULT_BOUNCE_LIMIT)]
    #[serde(default = "default_bounce_limit")]
    pub bounce_limit: u128,
}

/// Default per-peer uncashed cheque exposure cap (ten times the default payment
/// threshold of `13_500_000` units).
const DEFAULT_BOUNCE_LIMIT: u128 = 135_000_000;

fn default_bounce_limit() -> u128 {
    DEFAULT_BOUNCE_LIMIT
}

impl Default for SwapArgs {
    fn default() -> Self {
        Self {
            enable: false,
            chequebook: None,
            beneficiary: None,
            deploy: false,
            bounce_limit: DEFAULT_BOUNCE_LIMIT,
        }
    }
}

impl SwapArgs {
    /// Build the validated SWAP configuration.
    pub fn swap_config(&self) -> SwapConfig {
        SwapConfig {
            enable: self.enable,
            chequebook: self.chequebook,
            beneficiary: self.beneficiary,
            deploy: self.deploy,
            bounce_limit: self.bounce_limit,
        }
    }
}

/// Validated SWAP configuration carried on the node configs.
///
/// Plain data with no chain-crate types so it compiles in every build. The
/// builder reads these fields only under the `swap` feature; without it they are
/// inert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapConfig {
    /// Whether SWAP settlement is requested via the dedicated flag.
    pub enable: bool,

    /// This node's chequebook contract address, if configured.
    pub chequebook: Option<Address>,

    /// Payout address cheques sent to us must name. `None` falls back to the node
    /// Ethereum address.
    pub beneficiary: Option<Address>,

    /// Whether to deploy a fresh chequebook on startup.
    pub deploy: bool,

    /// Per-peer cap on uncashed cheque exposure, in cumulative-payout units
    /// (#438).
    pub bounce_limit: u128,
}

impl Default for SwapConfig {
    fn default() -> Self {
        SwapArgs::default().swap_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_unset() {
        let cfg = SwapArgs::default().swap_config();
        assert!(!cfg.enable);
        assert_eq!(cfg.chequebook, None);
        assert_eq!(cfg.beneficiary, None);
        assert!(!cfg.deploy);
        assert_eq!(cfg.bounce_limit, DEFAULT_BOUNCE_LIMIT);
    }

    #[test]
    fn overrides_flow_through() {
        let chequebook = Address::repeat_byte(0x11);
        let beneficiary = Address::repeat_byte(0x22);
        let args = SwapArgs {
            enable: true,
            chequebook: Some(chequebook),
            beneficiary: Some(beneficiary),
            deploy: true,
            bounce_limit: 42,
        };
        let cfg = args.swap_config();
        assert!(cfg.enable);
        assert_eq!(cfg.chequebook, Some(chequebook));
        assert_eq!(cfg.beneficiary, Some(beneficiary));
        assert!(cfg.deploy);
        assert_eq!(cfg.bounce_limit, 42);
    }
}
