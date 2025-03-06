//! Storage-specific credential and access control traits.
//!
//! This module defines the storage-specific aspects of access control,
//! particularly focused on postage stamps for storage.

pub mod error;

pub use error::{Result, StorageError};

use auto_impl::auto_impl;
use bytes::Bytes;
use nectar_access_control::CredentialBase;

use crate::chunk::{ChunkAddress, ChunkData};
use crate::constants::*;

/// Trait for storage credentials (e.g., postage stamps)
pub trait StorageCredential: CredentialBase {
    /// Get the amount of storage resources this credential provides
    fn amount(&self) -> u64;

    /// Get the batch ID associated with this credential
    fn batch_id(&self) -> &[u8];

    /// Get the network depth this credential is valid for
    fn depth(&self) -> u8;

    /// Get the owner of this credential
    fn owner(&self) -> &[u8];
}

/// A postage stamp for paying for storage
#[derive(Debug, Clone)]
pub struct PostageStamp {
    /// Unique identifier
    id: [u8; HASH_SIZE],
    /// Batch identifier
    batch_id: [u8; BATCH_ID_SIZE],
    /// Owner address
    owner: [u8; OWNER_SIZE],
    /// Depth in the network
    depth: u8,
    /// Amount allocated
    amount: u64,
    /// Expiration time
    expiration: Option<u64>,
    /// Raw stamp data
    data: Bytes,
}

impl PostageStamp {
    /// Create a new postage stamp
    pub fn new(
        id: [u8; HASH_SIZE],
        batch_id: [u8; BATCH_ID_SIZE],
        owner: [u8; OWNER_SIZE],
        depth: u8,
        amount: u64,
        expiration: Option<u64>,
        data: Bytes,
    ) -> Self {
        Self {
            id,
            batch_id,
            owner,
            depth,
            amount,
            expiration,
            data,
        }
    }
}

impl CredentialBase for PostageStamp {
    fn id(&self) -> &[u8] {
        &self.id
    }

    fn credential_type(&self) -> nectar_access_control::credential::CredentialType {
        nectar_access_control::credential::CredentialType::Payment
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
        // For postage stamps, the owner is effectively the issuer
        &self.owner
    }

    fn subject(&self) -> &[u8] {
        // Postage stamps apply to anyone holding them, so use a constant value
        static ANYONE: [u8; 1] = [0];
        &ANYONE
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn serialize(&self) -> Bytes {
        self.data.clone()
    }
}

impl StorageCredential for PostageStamp {
    fn amount(&self) -> u64 {
        self.amount
    }

    fn batch_id(&self) -> &[u8] {
        &self.batch_id
    }

    fn depth(&self) -> u8 {
        self.depth
    }

    fn owner(&self) -> &[u8] {
        &self.owner
    }
}

/// Factory for creating storage credentials
#[auto_impl(&, Arc)]
pub trait StorageCredentialFactory: Send + Sync + 'static {
    /// Create a new storage credential
    fn create_credential(
        &self,
        params: StorageCredentialParams,
    ) -> error::Result<Box<dyn StorageCredential>>;

    /// Parse a storage credential from serialized data
    fn parse_credential(&self, data: &[u8]) -> error::Result<Box<dyn StorageCredential>>;
}

/// Parameters for creating a storage credential
#[derive(Debug, Clone)]
pub struct StorageCredentialParams {
    /// Amount of storage to allocate
    pub amount: u64,
    /// Batch identifier
    pub batch_id: [u8; BATCH_ID_SIZE],
    /// Network depth
    pub depth: u8,
    /// Owner of the credential
    pub owner: [u8; OWNER_SIZE],
    /// Expiration time (optional)
    pub expiration: Option<u64>,
}

/// Storage usage information
#[derive(Debug, Clone)]
pub struct StorageUsage {
    /// Total bytes stored with this credential
    pub stored_bytes: u64,
    /// Number of chunks stored
    pub chunk_count: u64,
    /// When the credential was first used
    pub first_used: u64,
    /// When the credential was last used
    pub last_used: u64,
    /// Remaining storage capacity
    pub remaining: u64,
}

/// Authentication for storage operations
#[auto_impl(&, Arc)]
pub trait StorageAuthenticator: Send + Sync + 'static {
    /// Authenticate a chunk storage request with the given credential
    fn authenticate_storage(
        &self,
        chunk: &ChunkData,
        credential: &dyn StorageCredential,
    ) -> error::Result<()>;

    /// Verify that a storage credential is valid and properly formed
    fn verify_credential(&self, credential: &dyn StorageCredential) -> error::Result<()>;
}

/// Authorization for storage operations
#[auto_impl(&, Arc)]
pub trait StorageAuthorizer: Send + Sync + 'static {
    /// Authorize storing a chunk with the given credential
    fn authorize_storage(
        &self,
        chunk: &ChunkData,
        credential: &dyn StorageCredential,
    ) -> error::Result<()>;

    /// Check if the node should store the chunk based on network topology and credential depth
    fn should_store(&self, address: &ChunkAddress, depth: u8) -> bool;
}

/// Accounting for storage operations
#[auto_impl(&, Arc)]
pub trait StorageAccountant: Send + Sync + 'static {
    /// Record chunk storage with the given credential
    fn record_storage(
        &self,
        chunk: &ChunkData,
        credential: &dyn StorageCredential,
    ) -> error::Result<()>;

    /// Check if a storage credential has already been used
    fn is_credential_used(&self, credential: &dyn StorageCredential) -> error::Result<bool>;

    /// Get available storage capacity
    fn available_capacity(&self) -> u64;

    /// Check if storage capacity is available for a chunk of the given size
    fn has_capacity_for(&self, size: usize) -> bool;

    /// Get usage information for a storage credential
    fn credential_usage(&self, credential: &dyn StorageCredential) -> error::Result<StorageUsage>;
}

/// Combined controller for storage-related access control
#[auto_impl(&, Arc)]
pub trait StorageController: Send + Sync + 'static {
    /// Get the storage authenticator
    fn authenticator(&self) -> &dyn StorageAuthenticator;

    /// Get the storage authorizer
    fn authorizer(&self) -> &dyn StorageAuthorizer;

    /// Get the storage accountant
    fn accountant(&self) -> &dyn StorageAccountant;

    /// Process a chunk storage request
    fn process_storage(
        &self,
        chunk: &ChunkData,
        credential: &dyn StorageCredential,
    ) -> error::Result<()> {
        // Authenticate and authorize
        self.authenticator()
            .authenticate_storage(chunk, credential)?;
        self.authorizer().authorize_storage(chunk, credential)?;

        // Record storage
        self.accountant().record_storage(chunk, credential)?;

        Ok(())
    }

    /// Check if a chunk should be stored by this node
    fn should_store_chunk(&self, chunk: &ChunkData, credential: &dyn StorageCredential) -> bool {
        self.authorizer()
            .should_store(&chunk.address(), credential.depth())
    }
}

/// A simple implementation of StorageController with no-op behavior
pub struct NoopStorageController;

impl StorageController for NoopStorageController {
    fn authenticator(&self) -> &dyn StorageAuthenticator {
        static AUTHENTICATOR: NoopStorageAuthenticator = NoopStorageAuthenticator;
        &AUTHENTICATOR
    }

    fn authorizer(&self) -> &dyn StorageAuthorizer {
        static AUTHORIZER: NoopStorageAuthorizer = NoopStorageAuthorizer;
        &AUTHORIZER
    }

    fn accountant(&self) -> &dyn StorageAccountant {
        static ACCOUNTANT: NoopStorageAccountant = NoopStorageAccountant;
        &ACCOUNTANT
    }
}

/// A no-op implementation of StorageAuthenticator
pub struct NoopStorageAuthenticator;

impl StorageAuthenticator for NoopStorageAuthenticator {
    fn authenticate_storage(
        &self,
        _chunk: &ChunkData,
        _credential: &dyn StorageCredential,
    ) -> error::Result<()> {
        Ok(())
    }

    fn verify_credential(&self, _credential: &dyn StorageCredential) -> error::Result<()> {
        Ok(())
    }
}

/// A no-op implementation of StorageAuthorizer
pub struct NoopStorageAuthorizer;

impl StorageAuthorizer for NoopStorageAuthorizer {
    fn authorize_storage(
        &self,
        _chunk: &ChunkData,
        _credential: &dyn StorageCredential,
    ) -> error::Result<()> {
        Ok(())
    }

    fn should_store(&self, _address: &ChunkAddress, _depth: u8) -> bool {
        true
    }
}

/// A no-op implementation of StorageAccountant
pub struct NoopStorageAccountant;

impl StorageAccountant for NoopStorageAccountant {
    fn record_storage(
        &self,
        _chunk: &ChunkData,
        _credential: &dyn StorageCredential,
    ) -> error::Result<()> {
        Ok(())
    }

    fn is_credential_used(&self, _credential: &dyn StorageCredential) -> error::Result<bool> {
        Ok(false)
    }

    fn available_capacity(&self) -> u64 {
        u64::MAX
    }

    fn has_capacity_for(&self, _size: usize) -> bool {
        true
    }

    fn credential_usage(&self, _credential: &dyn StorageCredential) -> error::Result<StorageUsage> {
        Ok(StorageUsage {
            stored_bytes: 0,
            chunk_count: 0,
            first_used: 0,
            last_used: 0,
            remaining: u64::MAX,
        })
    }
}
