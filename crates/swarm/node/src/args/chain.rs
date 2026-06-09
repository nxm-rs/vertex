//! Ethereum chain CLI arguments and validated configuration.
//!
//! These are plain knobs (an RPC URL and two tuning durations) that exist in
//! every build. The arguments name no chain types, so the surface is identical
//! whether or not the binary is compiled with the optional `chain` feature; a
//! build without the feature simply ignores them. The builder consults
//! [`SwarmNodeType::needs_chain`](vertex_swarm_primitives::SwarmNodeType::needs_chain)
//! and a configured [`ChainConfig::rpc_url`] before constructing a chain
//! service.

use std::time::Duration;

use clap::Args;
use serde::{Deserialize, Serialize};

/// Default replacement-fee tip boost, as a percentage over the current
/// estimate, for a resend or cancel of a stuck transaction.
const DEFAULT_TIP_BOOST_PERCENT: u16 = 25;

/// Default maximum head-to-wall-clock delay the chain health poll treats as
/// synced, in seconds.
const DEFAULT_STALL_TIMEOUT_SECS: u64 = 60;

/// Ethereum chain CLI arguments.
#[derive(Debug, Default, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Chain")]
#[serde(default)]
pub struct ChainArgs {
    /// Ethereum RPC endpoint for chain interaction (staking, settlement, price
    /// oracle). Required for a storer and for a client with SWAP enabled; unused
    /// by a bootnode or a chain-free client. Has no effect unless the binary was
    /// built with the `chain` feature.
    #[arg(long = "chain.rpc-url")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,

    /// Replacement-fee tip boost percentage applied when resending or cancelling
    /// a stuck transaction.
    #[arg(long = "chain.tip-boost-percent")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_boost_percent: Option<u16>,

    /// Maximum head-to-wall-clock delay (seconds) the chain health poll treats
    /// as synced.
    #[arg(long = "chain.stall-timeout")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_secs: Option<u64>,
}

impl ChainArgs {
    /// Build the validated chain configuration.
    pub fn chain_config(&self) -> ChainConfig {
        ChainConfig {
            rpc_url: self.rpc_url.clone(),
            tip_boost_percent: self.tip_boost_percent.unwrap_or(DEFAULT_TIP_BOOST_PERCENT),
            stall_timeout: Duration::from_secs(
                self.stall_timeout_secs
                    .unwrap_or(DEFAULT_STALL_TIMEOUT_SECS),
            ),
        }
    }
}

/// Validated chain configuration carried on the node configs.
///
/// Plain data with no chain-crate types so it compiles in every build. The
/// builder reads [`rpc_url`](ChainConfig::rpc_url) only under the `chain`
/// feature; without it the fields are inert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainConfig {
    /// Ethereum RPC endpoint, if configured.
    pub rpc_url: Option<String>,

    /// Replacement-fee tip boost percentage for resends and cancels.
    pub tip_boost_percent: u16,

    /// Maximum head-to-wall-clock delay treated as synced.
    pub stall_timeout: Duration,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            rpc_url: None,
            tip_boost_percent: DEFAULT_TIP_BOOST_PERCENT,
            stall_timeout: Duration::from_secs(DEFAULT_STALL_TIMEOUT_SECS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_unset() {
        let cfg = ChainArgs::default().chain_config();
        assert_eq!(cfg.rpc_url, None);
        assert_eq!(cfg.tip_boost_percent, DEFAULT_TIP_BOOST_PERCENT);
        assert_eq!(
            cfg.stall_timeout,
            Duration::from_secs(DEFAULT_STALL_TIMEOUT_SECS)
        );
    }

    #[test]
    fn overrides_flow_through() {
        let args = ChainArgs {
            rpc_url: Some("https://rpc.example".to_string()),
            tip_boost_percent: Some(40),
            stall_timeout_secs: Some(120),
        };
        let cfg = args.chain_config();
        assert_eq!(cfg.rpc_url.as_deref(), Some("https://rpc.example"));
        assert_eq!(cfg.tip_boost_percent, 40);
        assert_eq!(cfg.stall_timeout, Duration::from_secs(120));
    }
}
