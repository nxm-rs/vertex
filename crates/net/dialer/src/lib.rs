//! Generic dial request tracking with bounded queue and in-flight management.

mod backoff;
mod config;
pub mod error;
mod prepare;
mod tracker;
mod types;

pub use config::DialTrackerConfig;
pub use prepare::prepare_dial_opts;
pub use tracker::DialTracker;
pub use types::{CleanupResult, DialDispatch, DialRequest, EnqueueError};
