//! CLI argument structs for Swarm protocol configuration.
//!
//! These args serve dual purposes:
//! - CLI parsing via clap (`#[derive(Args)]`)
//! - Configuration serialization via serde (`#[derive(Serialize, Deserialize)]`)
//!
//! The structs implement config traits from `vertex_swarm_api`, allowing them
//! to be passed directly to component builders.
//!
//! # Configuration Hierarchy
//!
//! Use Figment to merge configuration sources with this priority
//! (highest wins):
//!
//! 1. CLI arguments
//! 2. Config file (TOML)
//! 3. Environment variables (`VERTEX_` prefix)
//! 4. Defaults (from `impl Default`)
//!
//! # Example
//!
//! ```ignore
//! use figment::{Figment, providers::{Env, Format, Toml, Serialized}};
//!
//! let config: NetworkArgs = Figment::new()
//!     .merge(Serialized::defaults(NetworkArgs::default()))
//!     .merge(Env::prefixed("VERTEX_NETWORK_"))
//!     .merge(Toml::file("config.toml").nested())
//!     .merge(Serialized::globals(&cli_args.network))
//!     .extract()?;
//! ```

mod identity;
mod network;
mod storage;
mod swarm;

pub use vertex_swarm_bandwidth::{BandwidthArgs, BandwidthModeArg};
pub use vertex_swarm_bandwidth_pricing::PricingArgs;
pub use identity::IdentityArgs;
pub use network::NetworkArgs;
pub use storage::StorageIncentiveArgs;
pub use swarm::{NodeTypeArg, SwarmArgs};
