//! Core access control primitives for decentralized systems.
//!
//! This crate provides traits and types for managing authentication,
//! authorization, and accounting (AAA) in decentralized systems.
//! It offers a flexible framework for identity verification, permission
//! management, and resource tracking.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

// Re-export dependencies that are part of our public API
pub use bytes;

pub mod accounting;
pub mod auth;
pub mod controller;
pub mod credential;
pub mod error;

// Re-exports of primary types
pub use accounting::{Accountant, Reservation, ResourceType, UsageInfo};
pub use auth::{Authenticator, Authorizer};
pub use controller::AccessController;
pub use credential::{Credential, CredentialBase, CredentialFactory, CredentialType};
pub use error::{Error, Result};

/// Constants used throughout the crate
pub mod constants {
    /// Default reservation timeout in seconds
    pub const DEFAULT_RESERVATION_TIMEOUT: u64 = 60;

    /// Default credential ID size in bytes
    pub const CREDENTIAL_ID_SIZE: usize = 32;

    /// Default resource ID size in bytes
    pub const RESOURCE_ID_SIZE: usize = 32;
}
