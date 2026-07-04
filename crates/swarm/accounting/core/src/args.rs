//! CLI arguments for bandwidth accounting configuration.

use clap::Args;
use serde::{Deserialize, Serialize};

pub use vertex_swarm_accounting_pricing::FixedPricingArgs;

use crate::constants::*;

/// Bandwidth accounting CLI arguments.
///
/// This struct is for CLI parsing and serialization only.
/// Convert to `AccountingConfig` for runtime use.
#[derive(Debug, Args, Clone, Serialize, Deserialize)]
#[command(next_help_heading = "Accounting")]
#[serde(default)]
pub struct AccountingArgs {
    /// Payment threshold (triggers settlement when exceeded).
    #[arg(long = "accounting.threshold", default_value_t = DEFAULT_PAYMENT_THRESHOLD)]
    pub payment_threshold: u64,

    /// Payment tolerance percent for disconnect threshold.
    #[arg(long = "accounting.tolerance-percent", default_value_t = DEFAULT_PAYMENT_TOLERANCE_PERCENT)]
    pub payment_tolerance_percent: u64,

    /// Pseudosettle refresh rate per second.
    #[arg(long = "accounting.refresh-rate", default_value_t = DEFAULT_REFRESH_RATE)]
    pub refresh_rate: u64,

    /// Early payment trigger percent (for SWAP).
    #[arg(long = "accounting.early-percent", default_value_t = DEFAULT_EARLY_PAYMENT_PERCENT)]
    pub early_payment_percent: u64,

    /// Scaling factor for client-only nodes (divides thresholds).
    #[arg(long = "accounting.client-only-factor", default_value_t = DEFAULT_CLIENT_ONLY_FACTOR)]
    pub client_only_factor: u64,

    /// Chunk pricing configuration.
    #[command(flatten)]
    #[serde(default)]
    pub pricing: FixedPricingArgs,
}

impl Default for AccountingArgs {
    fn default() -> Self {
        Self {
            payment_threshold: DEFAULT_PAYMENT_THRESHOLD,
            payment_tolerance_percent: DEFAULT_PAYMENT_TOLERANCE_PERCENT,
            refresh_rate: DEFAULT_REFRESH_RATE,
            early_payment_percent: DEFAULT_EARLY_PAYMENT_PERCENT,
            client_only_factor: DEFAULT_CLIENT_ONLY_FACTOR,
            pricing: FixedPricingArgs::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: AccountingArgs,
    }

    /// The renamed `--accounting.*` flags parse into the matching fields, pricing
    /// included via the flattened `--accounting.base-price`.
    #[test]
    fn accounting_flags_parse() {
        let cli = TestCli::try_parse_from([
            "test",
            "--accounting.threshold",
            "42",
            "--accounting.tolerance-percent",
            "7",
            "--accounting.refresh-rate",
            "11",
            "--accounting.base-price",
            "9",
        ])
        .expect("accounting flags parse");
        assert_eq!(cli.args.payment_threshold, 42);
        assert_eq!(cli.args.payment_tolerance_percent, 7);
        assert_eq!(cli.args.refresh_rate, 11);
        assert_eq!(cli.args.pricing.base_price, 9);
    }
}
