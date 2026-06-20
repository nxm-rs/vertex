//! SWAP settlement CLI arguments and validated configuration.
//!
//! Plain knobs (chequebook, beneficiary, deploy toggle) present in every build; read by the builder
//! only under the `swap` feature when the bandwidth mode enables SWAP. On the CLI/config path SWAP
//! is selected solely by `--bandwidth.mode`; these only parameterise it. (The `with_swap` embedder
//! seam diverges and selects swap by chequebook presence; that is documented on the method.) Chain
//! and contract addresses come from the spec; the RPC endpoint is the shared `--chain.rpc-url`.

use alloy_primitives::Address;
use clap::Args;
use serde::{Deserialize, Serialize};

/// SWAP settlement CLI arguments.
#[derive(Debug, Default, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Swap")]
#[serde(default)]
pub struct SwapArgs {
    /// This node's chequebook contract address (the drawer of cheques we issue).
    /// Required to issue cheques when SWAP is enabled and a chequebook is not
    /// being deployed. Select a swap-capable `--bandwidth.mode` to enable SWAP.
    #[arg(long = "swap.chequebook")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chequebook: Option<Address>,

    /// Payout address that cheques sent to this node must name (where received
    /// funds are paid). Defaults to the node Ethereum address when unset. Select
    /// a swap-capable `--bandwidth.mode` to enable SWAP.
    #[arg(long = "swap.beneficiary")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beneficiary: Option<Address>,

    /// Deploy a new chequebook on startup instead of using an existing one.
    /// Select a swap-capable `--bandwidth.mode` to enable SWAP.
    #[arg(long = "swap.deploy")]
    #[serde(default)]
    pub deploy: bool,
}

impl SwapArgs {
    /// Build the validated SWAP configuration.
    pub fn swap_config(&self) -> SwapConfig {
        SwapConfig {
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
        assert_eq!(cfg.chequebook, None);
        assert_eq!(cfg.beneficiary, None);
        assert!(!cfg.deploy);
    }

    #[test]
    fn overrides_flow_through() {
        let chequebook = Address::repeat_byte(0x11);
        let beneficiary = Address::repeat_byte(0x22);
        let args = SwapArgs {
            chequebook: Some(chequebook),
            beneficiary: Some(beneficiary),
            deploy: true,
        };
        let cfg = args.swap_config();
        assert_eq!(cfg.chequebook, Some(chequebook));
        assert_eq!(cfg.beneficiary, Some(beneficiary));
        assert!(cfg.deploy);
    }
}
