//! Credential types and traits for authentication.
//!
//! Credentials are used to prove identity and authorization for
//! actions within the system.

use auto_impl::auto_impl;
use bytes::Bytes;
use std::fmt::Debug;

use crate::error::Result;

/// Base trait for all access control credentials - object safe version
pub trait CredentialBase: Send + Sync + 'static {
    /// Get the unique identifier for this credential
    fn id(&self) -> &[u8];

    /// Get the type of this credential
    fn credential_type(&self) -> CredentialType;

    /// Check if this credential has expired
    fn is_expired(&self) -> bool;

    /// Get the expiration time of this credential, if any
    fn expiration(&self) -> Option<u64>;

    /// Get the issuer of this credential
    fn issuer(&self) -> &[u8];

    /// Get the subject (user/entity) this credential applies to
    fn subject(&self) -> &[u8];

    /// Get the raw data of this credential
    fn data(&self) -> &[u8];

    /// Serialize this credential to bytes
    fn serialize(&self) -> Bytes;
}

/// Extended trait that adds Clone and Debug - for concrete types
pub trait Credential: CredentialBase + Clone + std::fmt::Debug {}

// Automatically implement Credential for any type that implements CredentialBase + Clone + Debug
impl<T> Credential for T where T: CredentialBase + Clone + std::fmt::Debug {}

/// Types of credentials for access control
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialType {
    /// An access token credential
    AccessToken,
    /// A capability credential
    Capability,
    /// A payment-based credential
    Payment,
    /// A membership credential
    Membership,
    /// Custom credential type with identifier
    Custom(u8),
}

impl CredentialType {
    /// Convert to byte representation
    pub fn to_byte(&self) -> u8 {
        match self {
            Self::AccessToken => 0,
            Self::Capability => 1,
            Self::Payment => 2,
            Self::Membership => 3,
            Self::Custom(id) => *id,
        }
    }

    /// Create from byte representation
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::AccessToken,
            1 => Self::Capability,
            2 => Self::Payment,
            3 => Self::Membership,
            id => Self::Custom(id),
        }
    }
}

/// Factory for creating credentials
#[auto_impl(&, Arc)]
pub trait CredentialFactory: Send + Sync + 'static {
    /// Create a credential with the given parameters
    fn create_credential(&self, params: CredentialParams) -> Result<Box<dyn CredentialBase>>;

    /// Parse a credential from its serialized form
    fn parse_credential(&self, data: &[u8]) -> Result<Box<dyn CredentialBase>>;

    /// Get the supported credential types
    fn supported_types(&self) -> Vec<CredentialType>;
}

/// Parameters for creating a credential
#[derive(Debug, Clone)]
pub struct CredentialParams {
    /// Type of credential to create
    pub credential_type: CredentialType,
    /// Subject (user/entity) for the credential
    pub subject: Vec<u8>,
    /// Issuer identity
    pub issuer: Vec<u8>,
    /// Expiration time (seconds since epoch)
    pub expiration: Option<u64>,
    /// Resource identifiers this credential applies to
    pub resources: Vec<Vec<u8>>,
    /// Permissions granted by this credential
    pub permissions: Vec<String>,
    /// Additional attributes
    pub attributes: Vec<(String, Vec<u8>)>,
}

/// A generic credential implementation
#[derive(Debug, Clone)]
pub struct GenericCredential {
    /// Unique identifier
    id: Vec<u8>,
    /// Type of credential
    credential_type: CredentialType,
    /// Issuer of the credential
    issuer: Vec<u8>,
    /// Subject of the credential
    subject: Vec<u8>,
    /// Expiration time
    expiration: Option<u64>,
    /// Raw credential data
    data: Bytes,
}

impl GenericCredential {
    /// Create a new generic credential
    pub fn new(
        id: Vec<u8>,
        credential_type: CredentialType,
        issuer: Vec<u8>,
        subject: Vec<u8>,
        expiration: Option<u64>,
        data: Bytes,
    ) -> Self {
        Self {
            id,
            credential_type,
            issuer,
            subject,
            expiration,
            data,
        }
    }
}

impl CredentialBase for GenericCredential {
    fn id(&self) -> &[u8] {
        &self.id
    }

    fn credential_type(&self) -> CredentialType {
        self.credential_type
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
        &self.data
    }

    fn serialize(&self) -> Bytes {
        self.data.clone()
    }
}
