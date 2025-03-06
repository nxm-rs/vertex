//! Accounting traits for tracking resource usage.
//!
//! This module provides traits for tracking and managing resource usage.

use auto_impl::auto_impl;

use crate::credential::CredentialBase;
use crate::error::Result;

/// Types of resources that can be tracked
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    /// Storage space
    Storage,
    /// Network bandwidth
    Bandwidth,
    /// Computation time
    Computation,
    /// Request quota
    Requests,
    /// Custom resource type
    Custom(u8),
}

/// Information about resource usage
#[derive(Debug, Clone)]
pub struct UsageInfo {
    /// Resource type
    pub resource_type: ResourceType,
    /// Total usage amount
    pub total: u64,
    /// Available (remaining) amount
    pub available: u64,
    /// Used amount
    pub used: u64,
    /// First usage timestamp
    pub first_used: Option<u64>,
    /// Last usage timestamp
    pub last_used: Option<u64>,
}

/// Reservation of resources
#[derive(Debug, Clone)]
pub struct Reservation {
    /// Unique identifier for this reservation
    pub id: [u8; 32],
    /// Credential used for the reservation
    pub credential_id: Vec<u8>,
    /// Resource being reserved
    pub resource_type: ResourceType,
    /// Amount reserved
    pub amount: u64,
    /// When the reservation was made
    pub timestamp: u64,
    /// When the reservation expires
    pub expiration: u64,
}

/// Accounting trait for tracking resource usage
#[auto_impl(&, Arc)]
pub trait Accountant: Send + Sync + 'static {
    /// Record usage of a resource
    fn record_usage(
        &self,
        credential: &dyn CredentialBase,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<()>;

    /// Check if a credential has sufficient resources
    fn has_sufficient_resources(
        &self,
        credential: &dyn CredentialBase,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<bool>;

    /// Get usage information for a credential
    fn get_usage(
        &self,
        credential: &dyn CredentialBase,
        resource_type: ResourceType,
    ) -> Result<UsageInfo>;

    /// Reserve resources for future use
    fn reserve_resources(
        &self,
        credential: &dyn CredentialBase,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<Reservation>;

    /// Commit a previously created reservation
    fn commit_reservation(&self, reservation: &Reservation) -> Result<()>;

    /// Release a reservation without using it
    fn release_reservation(&self, reservation: &Reservation) -> Result<()>;
}
