//! Collection of metrics utilities.
//!
//! ## Feature Flags
//!
//! - `common`: Common metrics utilities, such as wrappers around tokio senders and receivers. Pulls
//!   in `tokio`.

#![doc(
    issue_tracker_base_url = "https://github.com/rndlabs/beers/issues/"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

/// Metrics derive macro.
pub use beers_metrics_derive::Metrics;

/// Implementation of common metric utilities.
#[cfg(feature = "common")]
pub mod common;

/// Re-export core metrics crate.
pub use metrics;
