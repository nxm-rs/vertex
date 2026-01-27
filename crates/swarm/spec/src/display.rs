//! Display and logging traits for Swarm types.
//!
//! This module provides:
//!
//! - [`Loggable`] - Generic trait for types that can log themselves via tracing
//! - [`DisplaySwarmSpec`] - Wrapper for [`Display`] trait integration of SwarmSpec
//!
//! # Example
//!
//! ```ignore
//! use vertex_swarmspec::{init_mainnet, Loggable};
//!
//! let spec = init_mainnet();
//!
//! // Log via tracing
//! spec.log();
//! ```

use alloc::string::String;
use core::fmt::{self, Display, Formatter};

use humansize::{BINARY, format_size};
use nectar_primitives::ChunkTypeSet;
use vertex_swarm_forks::ForkCondition;

use crate::SwarmSpec;

/// Trait for types that can log themselves via tracing.
///
/// This provides a standard way for types to output their state to the
/// tracing infrastructure. Implementations should use `info!()` for
/// normal output, with each logical line as a separate log call to
/// ensure proper formatting with log prefixes.
///
/// # Example
///
/// ```ignore
/// use vertex_swarmspec::Loggable;
///
/// struct MyConfig { /* ... */ }
///
/// impl Loggable for MyConfig {
///     fn log(&self) {
///         use tracing::info;
///         info!("MyConfig:");
///         info!("  field1: {}", self.field1);
///         info!("  field2: {}", self.field2);
///     }
/// }
/// ```
#[cfg(feature = "std")]
pub trait Loggable {
    /// Log this value using tracing.
    ///
    /// Each configuration line should get its own `info!()` call so log
    /// prefixes appear correctly.
    fn log(&self);
}

/// Blanket implementation of [`Loggable`] for all [`SwarmSpec`] types.
#[cfg(feature = "std")]
impl<S: SwarmSpec> Loggable for S {
    fn log(&self) {
        use tracing::info;

        info!("Swarm specification:");
        info!(
            "  Network: {} (ID: {})",
            self.network_name(),
            self.network_id()
        );
        info!(
            "  Chain: {} (chain ID: {})",
            self.chain(),
            self.chain().id()
        );
        info!("  Token: {} @ {}", self.token().symbol, self.token().address);
        info!("  Chunk size: {} bytes", self.chunk_size());
        info!("  Chunks: {}", S::ChunkSet::format_supported_types());
        info!(
            "  Reserve capacity: {} chunks ({})",
            self.reserve_capacity(),
            format_reserve_size(self.reserve_capacity(), self.chunk_size())
        );

        info!("  Hardforks:");
        for (fork, condition) in self.hardforks().forks_iter() {
            if let ForkCondition::Timestamp(ts) = condition {
                info!("    {:32} @{}", fork.name(), ts);
            }
        }
    }
}

/// Extension trait for SwarmSpec that provides a display wrapper.
///
/// This trait is automatically implemented for all types that implement [`SwarmSpec`].
pub trait SwarmSpecExt: SwarmSpec {
    /// Create a wrapper for [`Display`] trait integration.
    ///
    /// Use this when you need to format the spec as a string or use it
    /// with `println!()`, `format!()`, etc.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use vertex_swarmspec::{init_mainnet, SwarmSpecExt};
    ///
    /// let spec = init_mainnet();
    /// println!("{}", spec.display());
    /// let s = format!("{}", spec.display());
    /// ```
    fn display(&self) -> DisplaySwarmSpec<'_, Self>
    where
        Self: Sized,
    {
        DisplaySwarmSpec::new(self)
    }
}

/// Blanket implementation for all SwarmSpec types.
impl<S: SwarmSpec> SwarmSpecExt for S {}

/// Calculate storage size in human-readable format.
fn format_reserve_size(reserve_capacity: u64, chunk_size: usize) -> String {
    let bytes = reserve_capacity * chunk_size as u64;
    format_size(bytes, BINARY)
}

/// Pretty-print wrapper for SwarmSpec configuration.
///
/// This wrapper implements [`Display`] for use with `println!()`, `format!()`,
/// and other formatting macros.
///
/// Prefer using [`Loggable::log()`] for tracing output, as it formats
/// each line with proper log prefixes.
///
/// # Example
///
/// ```ignore
/// use vertex_swarmspec::{init_mainnet, SwarmSpecExt};
///
/// let spec = init_mainnet();
/// println!("{}", spec.display());
/// ```
///
/// # Output Format
///
/// ```text
/// Swarm specification:
///   Network: mainnet (ID: 1)
///   Chain: xdai (chain ID: 100)
///   Token: xBZZ @ 0x2aC3c1d3e24b45c6C310534Bc2Dd84B5ed576335
///   Chunk size: 4096 bytes
///   Chunks: CAC (0x00), SOC (0x01)
///   Reserve capacity: 4194304 chunks (16 GiB)
///   Hardforks:
///     Accord                           @1623255587
/// ```
pub struct DisplaySwarmSpec<'a, S: SwarmSpec> {
    spec: &'a S,
}

impl<'a, S: SwarmSpec> DisplaySwarmSpec<'a, S> {
    /// Create a new display wrapper for the given spec.
    pub fn new(spec: &'a S) -> Self {
        Self { spec }
    }
}

impl<S: SwarmSpec> Display for DisplaySwarmSpec<'_, S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "Swarm specification:")?;
        writeln!(
            f,
            "  Network: {} (ID: {})",
            self.spec.network_name(),
            self.spec.network_id()
        )?;
        writeln!(
            f,
            "  Chain: {} (chain ID: {})",
            self.spec.chain(),
            self.spec.chain().id()
        )?;
        writeln!(
            f,
            "  Token: {} @ {}",
            self.spec.token().symbol,
            self.spec.token().address
        )?;
        writeln!(f, "  Chunk size: {} bytes", self.spec.chunk_size())?;
        writeln!(f, "  Chunks: {}", S::ChunkSet::format_supported_types())?;
        writeln!(
            f,
            "  Reserve capacity: {} chunks ({})",
            self.spec.reserve_capacity(),
            format_reserve_size(self.spec.reserve_capacity(), self.spec.chunk_size())
        )?;

        // Hardforks section
        writeln!(f, "  Hardforks:")?;
        for (fork, condition) in self.spec.hardforks().forks_iter() {
            if let ForkCondition::Timestamp(ts) = condition {
                writeln!(f, "    {:32} @{}", fork.name(), ts)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{init_mainnet, init_testnet};

    #[test]
    fn test_display_mainnet() {
        let spec = init_mainnet();
        let output = alloc::format!("{}", spec.display());

        assert!(output.contains("Swarm specification:"));
        assert!(output.contains("mainnet"));
        assert!(output.contains("ID: 1"));
        // Chain ID 100 for Gnosis (displayed as xdai)
        assert!(
            output.contains("chain ID: 100"),
            "Expected chain ID 100, got: {}",
            output
        );
        assert!(output.contains("xBZZ"));
        assert!(output.contains("4096 bytes"));
        // Chunk types with abbreviations and hex codes
        assert!(output.contains("CAC (0x00)"));
        assert!(output.contains("SOC (0x01)"));
        assert!(output.contains("Accord"));
    }

    #[test]
    fn test_display_testnet() {
        let spec = init_testnet();
        let output = alloc::format!("{}", spec.display());

        assert!(output.contains("Swarm specification:"));
        assert!(output.contains("testnet"));
        assert!(output.contains("ID: 10"));
        // Chain ID 11155111 for Sepolia
        assert!(
            output.contains("chain ID: 11155111"),
            "Expected chain ID 11155111, got: {}",
            output
        );
        assert!(output.contains("sBZZ"));
    }

    #[test]
    fn test_format_reserve_size() {
        // 4194304 chunks * 4096 bytes = 16 GiB
        let size = format_reserve_size(4194304, 4096usize);
        assert_eq!(size, "16 GiB");
    }
}
