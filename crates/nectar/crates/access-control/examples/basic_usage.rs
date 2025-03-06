//! A basic example of using the access control system

use bytes::Bytes;
use nectar_access_control::{
    accounting::{Reservation, ResourceType, UsageInfo},
    auth::{Action, ResourceId},
    error::{Error, Result},
    AccessController, Accountant, Authenticator, Authorizer, Credential, CredentialType,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Example implementation of a simple credential
#[derive(Debug, Clone)]
struct SimpleCredential {
    id: Vec<u8>,
    issuer: Vec<u8>,
    subject: Vec<u8>,
    expiration: Option<u64>,
    permissions: Vec<String>,
}

impl Credential for SimpleCredential {
    fn id(&self) -> &[u8] {
        &self.id
    }

    fn credential_type(&self) -> CredentialType {
        CredentialType::AccessToken
    }

    fn is_expired(&self) -> bool {
        if let Some(exp) = self.expiration {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            exp < now
        } else {
            false
        }
    }

    fn expiration(&self) -> Option<u64> {
        self.expiration
    }

    fn issuer(&self) -> &[u8] {
        &self.issuer
    }

    fn subject(&self) -> &[u8] {
        &self.subject
    }

    fn data(&self) -> &[u8] {
        &self.id // Use ID as data for simplicity
    }

    fn serialize(&self) -> Bytes {
        Bytes::copy_from_slice(&self.id)
    }
}

// Simple implementation of the authenticator
struct SimpleAuthenticator;

impl Authenticator for SimpleAuthenticator {
    fn authenticate(&self, credential: &dyn Credential) -> Result<()> {
        if credential.is_expired() {
            return Err(Error::ExpiredCredential);
        }

        // Check if issuer is trusted (simplified)
        if !self.is_trusted_issuer(credential) {
            return Err(Error::authentication("Untrusted issuer"));
        }

        Ok(())
    }

    fn verify_claim(&self, _credential: &dyn Credential, _claim: &str) -> Result<bool> {
        // Simplified implementation
        Ok(true)
    }

    fn is_trusted_issuer(&self, credential: &dyn Credential) -> bool {
        // In a real system, we would check against a list of trusted issuers
        let issuer = credential.issuer();
        !issuer.is_empty()
    }
}

// Simple implementation of the authorizer
struct SimpleAuthorizer;

impl Authorizer for SimpleAuthorizer {
    fn authorize(
        &self,
        credential: &dyn Credential,
        resource: &ResourceId,
        action: &Action,
    ) -> Result<()> {
        // In a real system, we would check permissions against a policy store
        if let Ok(has_permission) = self.has_permission(credential, &format!("{:?}", action)) {
            if has_permission {
                return Ok(());
            }
        }

        Err(Error::authorization(format!(
            "Not authorized to {:?} on resource {:?}",
            action,
            hex::encode(resource.as_bytes())
        )))
    }

    fn has_permission(&self, credential: &dyn Credential, permission: &str) -> Result<bool> {
        // In a real impl, we'd check the credential for this permission
        // Here we just simulate with some static permissions
        match credential.credential_type() {
            CredentialType::AccessToken => Ok(true),
            _ => Ok(false),
        }
    }

    fn accessible_resources(&self, _credential: &dyn Credential) -> Result<Vec<ResourceId>> {
        // Simplified implementation
        Ok(vec![])
    }

    fn allowed_actions(
        &self,
        _credential: &dyn Credential,
        _resource: &ResourceId,
    ) -> Result<Vec<Action>> {
        // Simplified implementation
        Ok(vec![])
    }
}

// Simple implementation of the accountant
struct SimpleAccountant {
    usage: Mutex<HashMap<Vec<u8>, HashMap<ResourceType, UsageInfo>>>,
    reservations: Mutex<HashMap<[u8; 32], Reservation>>,
}

impl SimpleAccountant {
    fn new() -> Self {
        Self {
            usage: Mutex::new(HashMap::new()),
            reservations: Mutex::new(HashMap::new()),
        }
    }
}

impl Accountant for SimpleAccountant {
    fn record_usage(
        &self,
        credential: &dyn Credential,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<()> {
        let mut usage = self.usage.lock().unwrap();
        let cred_usage = usage
            .entry(credential.id().to_vec())
            .or_insert_with(HashMap::new);

        let info = cred_usage
            .entry(resource_type)
            .or_insert_with(|| UsageInfo {
                resource_type,
                total: 1000, // Example quota
                available: 1000,
                used: 0,
                first_used: None,
                last_used: None,
            });

        if info.available < amount {
            return Err(Error::InsufficientResources {
                required: amount,
                available: info.available,
            });
        }

        info.used += amount;
        info.available -= amount;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if info.first_used.is_none() {
            info.first_used = Some(now);
        }
        info.last_used = Some(now);

        Ok(())
    }

    fn has_sufficient_resources(
        &self,
        credential: &dyn Credential,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<bool> {
        let usage = self.usage.lock().unwrap();

        if let Some(cred_usage) = usage.get(credential.id()) {
            if let Some(info) = cred_usage.get(&resource_type) {
                return Ok(info.available >= amount);
            }
        }

        // No usage info means full quota available
        Ok(true)
    }

    fn get_usage(
        &self,
        credential: &dyn Credential,
        resource_type: ResourceType,
    ) -> Result<UsageInfo> {
        let usage = self.usage.lock().unwrap();

        if let Some(cred_usage) = usage.get(credential.id()) {
            if let Some(info) = cred_usage.get(&resource_type) {
                return Ok(info.clone());
            }
        }

        // Return default usage info if not found
        Ok(UsageInfo {
            resource_type,
            total: 1000, // Example quota
            available: 1000,
            used: 0,
            first_used: None,
            last_used: None,
        })
    }

    fn reserve_resources(
        &self,
        credential: &dyn Credential,
        resource_type: ResourceType,
        amount: u64,
    ) -> Result<Reservation> {
        if !self.has_sufficient_resources(credential, resource_type, amount)? {
            let usage = self.get_usage(credential, resource_type)?;
            return Err(Error::InsufficientResources {
                required: amount,
                available: usage.available,
            });
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let reservation_id = [0u8; 32]; // Would generate a random ID in practice

        let reservation = Reservation {
            id: reservation_id,
            credential_id: credential.id().to_vec(),
            resource_type,
            amount,
            timestamp: now,
            expiration: now + 60, // 60 second expiration
        };

        let mut reservations = self.reservations.lock().unwrap();
        reservations.insert(reservation_id, reservation.clone());

        Ok(reservation)
    }

    fn commit_reservation(&self, reservation: &Reservation) -> Result<()> {
        let mut reservations = self.reservations.lock().unwrap();

        if reservations.remove(&reservation.id).is_none() {
            return Err(Error::reservation("Reservation not found"));
        }

        // In a real implementation, we would apply the reservation to the usage
        // We're skipping that here as it was already handled in record_usage

        Ok(())
    }

    fn release_reservation(&self, reservation: &Reservation) -> Result<()> {
        let mut reservations = self.reservations.lock().unwrap();

        if reservations.remove(&reservation.id).is_none() {
            return Err(Error::reservation("Reservation not found"));
        }

        Ok(())
    }
}

// Simple access controller that combines the components
struct SimpleAccessController {
    authenticator: SimpleAuthenticator,
    authorizer: SimpleAuthorizer,
    accountant: SimpleAccountant,
}

impl SimpleAccessController {
    fn new() -> Self {
        Self {
            authenticator: SimpleAuthenticator,
            authorizer: SimpleAuthorizer,
            accountant: SimpleAccountant::new(),
        }
    }
}

impl AccessController for SimpleAccessController {
    fn authenticator(&self) -> &dyn Authenticator {
        &self.authenticator
    }

    fn authorizer(&self) -> &dyn Authorizer {
        &self.authorizer
    }

    fn accountant(&self) -> &dyn Accountant {
        &self.accountant
    }
}

fn main() -> Result<()> {
    // Create a simple credential
    let credential = SimpleCredential {
        id: vec![1, 2, 3, 4],
        issuer: vec![5, 6, 7, 8],
        subject: vec![9, 10, 11, 12],
        expiration: None, // Never expires
        permissions: vec!["Read".to_string(), "Write".to_string()],
    };

    // Create a resource ID
    let resource = ResourceId::new(vec![20, 21, 22, 23]);

    // Create an access controller
    let controller = SimpleAccessController::new();

    // Process an access request
    println!("Processing access request...");
    controller.process_access(
        &credential,
        &resource,
        &Action::Read,
        ResourceType::Storage,
        100, // Request 100 units of storage
    )?;
    println!("Access granted!");

    // Get usage information
    let usage = controller
        .accountant()
        .get_usage(&credential, ResourceType::Storage)?;
    println!("Resource usage: {}/{} units used", usage.used, usage.total);

    // Try to use more resources than available
    println!("Attempting to use more resources than available...");
    match controller.process_access(
        &credential,
        &resource,
        &Action::Write,
        ResourceType::Storage,
        1000, // Request 1000 units (more than available)
    ) {
        Ok(()) => println!("Access granted (unexpected)"),
        Err(e) => println!("Access denied as expected: {}", e),
    }

    Ok(())
}
