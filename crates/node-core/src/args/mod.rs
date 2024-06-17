//! Parameters for configuring the node more granularly via CLI
//! 
//! /// LogArgs struct for configuring the logger
mod log;
pub use log::{ColorMode, LogArgs};

/// DatadirArgs for configuring data storage paths
mod datadir_args;
pub use datadir_args::DatadirArgs;

pub mod utils;