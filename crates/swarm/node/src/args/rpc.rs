//! gRPC endpoint CLI arguments.

use clap::Args;
use serde::{Deserialize, Serialize};
use vertex_swarm_api::StampValidation;

/// CLI argument for the chunk-upload stamp-validation policy. Maps to
/// [`StampValidation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StampValidationArg {
    /// Always validate uploads, ignoring the request flag.
    #[default]
    Enforce,
    /// Honour each request's `validate` flag. For trusted/private endpoints.
    PerRequest,
}

impl From<StampValidationArg> for StampValidation {
    fn from(arg: StampValidationArg) -> Self {
        match arg {
            StampValidationArg::Enforce => StampValidation::Enforce,
            StampValidationArg::PerRequest => StampValidation::PerRequest,
        }
    }
}

/// gRPC endpoint CLI arguments.
#[derive(Debug, Default, Args, Clone, Copy, Serialize, Deserialize)]
#[command(next_help_heading = "RPC")]
#[serde(default)]
pub struct RpcArgs {
    /// Stamp-validation policy for gRPC chunk uploads. A public endpoint
    /// enforces validation regardless of the request flag.
    #[arg(
        long = "rpc.stamp-validation",
        value_enum,
        default_value_t = StampValidationArg::Enforce
    )]
    pub stamp_validation: StampValidationArg,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_enforce() {
        assert_eq!(
            StampValidation::from(RpcArgs::default().stamp_validation),
            StampValidation::Enforce
        );
    }

    #[test]
    fn args_map_onto_the_policy() {
        assert_eq!(
            StampValidation::from(StampValidationArg::Enforce),
            StampValidation::Enforce
        );
        assert_eq!(
            StampValidation::from(StampValidationArg::PerRequest),
            StampValidation::PerRequest
        );
    }
}
