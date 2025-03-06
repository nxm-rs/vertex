//! Combined access controller for authentication, authorization, and accounting.
//!
//! This module provides a unified interface for managing all aspects of
//! access control in one place.

use auto_impl::auto_impl;

use crate::accounting::{Accountant, ResourceType};
use crate::auth::{Action, Authenticator, Authorizer, ResourceId};
use crate::credential::CredentialBase;
use crate::error::Result;

/// Combined controller for all access control operations
#[auto_impl(&, Arc)]
pub trait AccessController: Send + Sync + 'static {
    /// Get the authenticator component
    fn authenticator(&self) -> &dyn Authenticator;

    /// Get the authorizer component
    fn authorizer(&self) -> &dyn Authorizer;

    /// Get the accountant component
    fn accountant(&self) -> &dyn Accountant;

    /// Process an access request, handling authentication, authorization and accounting
    fn process_access(
        &self,
        credential: &dyn CredentialBase,
        resource: &ResourceId,
        action: &Action,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<()> {
        // First authenticate the credential
        self.authenticator().authenticate(credential)?;

        // Then authorize the action
        self.authorizer().authorize(credential, resource, action)?;

        // Reserve resources
        let reservation = self
            .accountant()
            .reserve_resources(credential, resource_type, amount)?;

        // Record usage and commit the reservation
        match self
            .accountant()
            .record_usage(credential, resource_type, amount)
        {
            Ok(()) => {
                self.accountant().commit_reservation(&reservation)?;
                Ok(())
            }
            Err(e) => {
                // Release the reservation on failure
                self.accountant().release_reservation(&reservation)?;
                Err(e)
            }
        }
    }

    /// Authenticate and authorize without accounting
    fn check_access(
        &self,
        credential: &dyn CredentialBase,
        resource: &ResourceId,
        action: &Action,
    ) -> Result<()> {
        self.authenticator().authenticate(credential)?;
        self.authorizer().authorize(credential, resource, action)?;
        Ok(())
    }
}
