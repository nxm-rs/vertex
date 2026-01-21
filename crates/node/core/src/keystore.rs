//! Keystore management for node identity.
//!
//! This module provides encrypted key storage using alloy's LocalSigner,
//! which uses the Ethereum JSON v3 format for compatibility with Bee keystores.

use alloy_signer::k256::ecdsa::SigningKey;
use alloy_signer_local::LocalSigner;
use eyre::{Result, WrapErr};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

/// Keystore service for managing encrypted private keys.
pub trait Keystore: Send + Sync {
    /// Load a key by name, decrypting with the provided password.
    fn load(&self, name: &str, password: &str) -> Result<SigningKey>;

    /// Save a key with encryption.
    fn save(&self, name: &str, key: &SigningKey, password: &str) -> Result<()>;

    /// Check if a key exists.
    fn exists(&self, name: &str) -> bool;
}

/// File-based keystore using alloy's LocalSigner with Ethereum JSON v3 format.
///
/// Keys are stored as encrypted JSON files in the specified directory.
/// Uses AES-128-CTR encryption with scrypt key derivation.
pub struct FileKeystore {
    path: PathBuf,
}

impl FileKeystore {
    /// Create a new file keystore at the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn key_path(&self, name: &str) -> PathBuf {
        // alloy's encrypt_keystore saves files without extension when using a name
        self.path.join(name)
    }
}

impl Keystore for FileKeystore {
    fn load(&self, name: &str, password: &str) -> Result<SigningKey> {
        let path = self.key_path(name);

        // Use alloy's LocalSigner to decrypt the keystore
        let signer = LocalSigner::decrypt_keystore(&path, password)
            .wrap_err_with(|| format!("Failed to decrypt key '{}' from {:?}", name, path))?;

        // Extract the signing key from the signer
        Ok(signer.credential().clone())
    }

    fn save(&self, name: &str, key: &SigningKey, password: &str) -> Result<()> {
        // Ensure directory exists
        fs::create_dir_all(&self.path)
            .wrap_err_with(|| format!("Failed to create keystore directory {:?}", self.path))?;

        let mut rng = rand::thread_rng();

        // Use alloy's LocalSigner to encrypt the keystore
        // The encrypt_keystore function takes the raw key bytes
        let key_bytes = key.to_bytes();
        let (_signer, _uuid) = LocalSigner::encrypt_keystore(
            &self.path,
            &mut rng,
            key_bytes.as_slice(),
            password,
            Some(name),
        )
        .wrap_err_with(|| format!("Failed to encrypt and save key '{}'", name))?;

        // alloy creates the file with the name we specified (no .json extension)
        let path = self.key_path(name);

        // Set restrictive file permissions on Unix
        #[cfg(unix)]
        if path.exists() {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&path, perms)?;
        }

        Ok(())
    }

    fn exists(&self, name: &str) -> bool {
        self.key_path(name).exists()
    }
}

/// In-memory keystore for ephemeral nodes.
///
/// Keys are stored only in memory and not persisted to disk.
/// Suitable for light nodes or testing.
pub struct MemoryKeystore {
    keys: RwLock<HashMap<String, SigningKey>>,
}

impl MemoryKeystore {
    /// Create a new empty memory keystore.
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryKeystore {
    fn default() -> Self {
        Self::new()
    }
}

impl Keystore for MemoryKeystore {
    fn load(&self, name: &str, _password: &str) -> Result<SigningKey> {
        let keys = self.keys.read().map_err(|_| eyre::eyre!("Lock poisoned"))?;
        keys.get(name)
            .cloned()
            .ok_or_else(|| eyre::eyre!("Key '{}' not found in memory keystore", name))
    }

    fn save(&self, name: &str, key: &SigningKey, _password: &str) -> Result<()> {
        let mut keys = self.keys.write().map_err(|_| eyre::eyre!("Lock poisoned"))?;
        keys.insert(name.to_string(), key.clone());
        Ok(())
    }

    fn exists(&self, name: &str) -> bool {
        self.keys
            .read()
            .map(|keys| keys.contains_key(name))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_memory_keystore_roundtrip() {
        let keystore = MemoryKeystore::new();
        let key = SigningKey::random(&mut rand::thread_rng());

        assert!(!keystore.exists("test"));

        keystore.save("test", &key, "password").unwrap();
        assert!(keystore.exists("test"));

        let loaded = keystore.load("test", "password").unwrap();
        assert_eq!(key.to_bytes(), loaded.to_bytes());
    }

    #[test]
    fn test_file_keystore_roundtrip() {
        let dir = tempdir().unwrap();
        let keystore = FileKeystore::new(dir.path());
        let key = SigningKey::random(&mut rand::thread_rng());

        assert!(!keystore.exists("swarm"));

        keystore.save("swarm", &key, "test-password-123").unwrap();
        assert!(keystore.exists("swarm"));

        let loaded = keystore.load("swarm", "test-password-123").unwrap();
        assert_eq!(key.to_bytes(), loaded.to_bytes());
    }

    #[test]
    fn test_file_keystore_wrong_password() {
        let dir = tempdir().unwrap();
        let keystore = FileKeystore::new(dir.path());
        let key = SigningKey::random(&mut rand::thread_rng());

        keystore.save("swarm", &key, "correct-password").unwrap();

        let result = keystore.load("swarm", "wrong-password");
        assert!(result.is_err());
    }
}
