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
#[derive(Debug, Default, Args, Clone, Serialize, Deserialize)]
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
}

impl SwapArgs {
    /// Build the validated SWAP configuration.
    pub fn swap_config(&self) -> SwapConfig {
        SwapConfig {
            enable: self.enable,
            chequebook: self.chequebook,
            beneficiary: self.beneficiary,
            deploy: self.deploy,
        }
    }
}

/// Validated SWAP configuration carried on the node configs.
///
/// Plain data with no chain-crate types so it compiles in every build. The
/// builder reads these fields only under the `swap` feature; without it they are
/// inert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        };
        let cfg = args.swap_config();
        assert!(cfg.enable);
        assert_eq!(cfg.chequebook, Some(chequebook));
        assert_eq!(cfg.beneficiary, Some(beneficiary));
        assert!(cfg.deploy);
    }
}
