//! Authentication and authorization traits.
//!
//! This module defines traits for verifying identity and checking permissions.

use auto_impl::auto_impl;

use crate::credential::CredentialBase;
use crate::error::Result;

/// Generic resource identifier type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId(pub Vec<u8>);

impl ResourceId {
    /// Create a new resource identifier
    pub fn new(id: Vec<u8>) -> Self {
        Self(id)
    }

    /// Get the raw bytes of this resource ID
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Action that can be performed on a resource
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    /// Create a resource
    Create,
    /// Read a resource
    Read,
    /// Update a resource
    Update,
    /// Delete a resource
    Delete,
    /// List resources
    List,
    /// Manage resources (administrative access)
    Manage,
    /// Custom action
    Custom(String),
}

/// Authentication trait for verifying credentials
#[auto_impl(&, Arc)]
pub trait Authenticator: Send + Sync + 'static {
    /// Authenticate a credential
    ///
    /// Verifies that the credential is valid, properly formed, and not expired.
    fn authenticate(&self, credential: &dyn CredentialBase) -> Result<()>;

    /// Verify a specific claim within a credential
    fn verify_claim(&self, credential: &dyn CredentialBase, claim: &str) -> Result<bool>;

    /// Check if a credential has been issued by a trusted authority
    fn is_trusted_issuer(&self, credential: &dyn CredentialBase) -> bool;
}

/// Authorization trait for checking permissions
#[auto_impl(&, Arc)]
pub trait Authorizer: Send + Sync + 'static {
    /// Authorize an action on a resource with the given credential
    fn authorize(
        &self,
        credential: &dyn CredentialBase,
        resource: &ResourceId,
        action: &Action,
    ) -> Result<()>;

    /// Check if a credential has a specific permission
    fn has_permission(&self, credential: &dyn CredentialBase, permission: &str) -> Result<bool>;

    /// Get all resources a credential has access to
    fn accessible_resources(&self, credential: &dyn CredentialBase) -> Result<Vec<ResourceId>>;

    /// Get all actions a credential can perform on a resource
    fn allowed_actions(
        &self,
        credential: &dyn CredentialBase,
        resource: &ResourceId,
    ) -> Result<Vec<Action>>;
}
