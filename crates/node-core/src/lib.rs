//! The core of the Bee(rs) node. Collection of utilties and libraries that are used by the node.

#![doc(
    issue_tracker_base_url = "https://github.com/rndlabs/bee-rs"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod args;
pub mod cli;
pub mod dirs;
pub mod exit;
pub mod utils;
pub mod metrics;
pub mod node_config;
pub mod version;

// Re-export for backwards compatibility
pub use metrics::prometheus_exporter;