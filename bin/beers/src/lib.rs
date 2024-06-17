pub mod cli;
pub mod commands;

/// Re-exported from `beers_node_core`.
pub mod core {
    pub use beers_node_core::*;
}

/// Re-exported from `beers_node_core`.
pub mod prometheus_exporter {
    pub use beers_node_core::prometheus_exporter::*;
}

/// Re-export of the `beers_node_core` types specifically in the `args` module.
pub mod args {
    pub use beers_node_core::args::*;
}

/// Re-exported from `beers_node_core`.
pub mod version {
    pub use beers_node_core::version::*;
}

// Re-exported from `beers_node_builder`
// pub mod builder {
//     pub use beers_node_builder::*;
// }


/// Re-exported from `reth_tasks`.
pub mod tasks {
    pub use beers_tasks::*;
}

// re-export for convenience
#[doc(inline)]
pub use beers_cli_runner::{tokio_runtime, CliContext, CliRunner};

#[cfg(all(unix, any(target_env = "gnu", target_os = "macos")))]
pub mod sigsegv_handler;

/// Signal handler to extract a backtrace from stack overflow.
///
/// This is a no-op because this platform doesn't support our signal handler's requirements.
#[cfg(not(all(unix, any(target_env = "gnu", target_os = "macos"))))]
pub mod sigsegv_handler {
    /// No-op function.
    pub fn install() {}
}
