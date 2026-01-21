//! Node commands
//!
//! This module contains the command implementations for the vertex node CLI.
//! Each command is responsible for:
//!
//! 1. Parsing and validating arguments
//! 2. Initializing the appropriate SwarmSpec (mainnet, testnet, dev)
//! 3. Setting up node components
//! 4. Running the node or performing the requested operation

pub mod config;
pub mod dev;
pub mod info;
pub mod node;
