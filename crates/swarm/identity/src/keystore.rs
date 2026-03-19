//! Keystore utilities for identity management.

use alloy_signer_local::PrivateKeySigner;
use eyre::{Result, WrapErr};
use std::fs;
use std::path::Path;
use tracing::info;

/// Resolve password from direct value or file.
pub fn resolve_password(password: Option<&str>, password_file: Option<&str>) -> Result<String> {
    if let Some(pw) = password {
        return Ok(pw.to_string());
    }

    if let Some(path) = password_file {
        let password = fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read password file: {}", path))?
            .trim()
            .to_string();
        return Ok(password);
    }

    // Default empty password (ephemeral nodes)
    Ok(String::new())
}

/// Load a signer from an Ethereum keystore file.
pub fn load_signer_from_keystore(keystore_path: &Path, password: &str) -> Result<PrivateKeySigner> {
    info!(
        "Loading signing key from keystore: {}",
        keystore_path.display()
    );
    PrivateKeySigner::decrypt_keystore(keystore_path, password)
        .wrap_err_with(|| format!("Failed to decrypt keystore at {}", keystore_path.display()))
}

/// Create a new random signer and save it to a keystore.
pub fn create_and_save_signer(keystore_path: &Path, password: &str) -> Result<PrivateKeySigner> {
    info!("Generating new signing key");

    if let Some(parent) = keystore_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let name = keystore_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("swarm");

    let dir = keystore_path.parent().unwrap_or(Path::new("."));

    // Use rand 0.8 for alloy keystore compatibility
    let (signer, _uuid) =
        PrivateKeySigner::new_keystore(dir, &mut rand_08::thread_rng(), password, Some(name))
            .wrap_err("Failed to create keystore")?;

    #[cfg(unix)]
    if keystore_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(keystore_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(keystore_path, perms)?;
    }

    Ok(signer)
}
