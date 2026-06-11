//! Keystore utilities for identity management.
//!
//! Keystore creation is native-only: it relies on the alloy eth-keystore
//! encoder, which does not build for `wasm32-unknown-unknown`. This is not a
//! gap, because only bootnodes and storers persist a keystore and those node
//! types never run in the browser; the wasm client uses an ephemeral
//! [`crate::Identity::random`] identity instead. The wasm sibling of
//! [`create_and_save_signer`] therefore returns [`KeystoreError::WasmUnsupported`].

use alloy_signer_local::PrivateKeySigner;
use eyre::{Result, WrapErr};
use std::fs;
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
use tracing::info;

/// Errors specific to keystore handling.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum KeystoreError {
    /// Keystore creation was requested on `wasm32`, where the eth-keystore
    /// encoder is unavailable. Persistent identities are native-only.
    #[error("keystore creation is not supported on wasm32; use an ephemeral identity")]
    WasmUnsupported,
}

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
///
/// Native-only: keystore decryption goes through the alloy eth-keystore decoder,
/// which is unavailable on wasm. The wasm sibling returns
/// [`KeystoreError::WasmUnsupported`].
#[cfg(not(target_arch = "wasm32"))]
pub fn load_signer_from_keystore(keystore_path: &Path, password: &str) -> Result<PrivateKeySigner> {
    info!(
        "Loading signing key from keystore: {}",
        keystore_path.display()
    );
    PrivateKeySigner::decrypt_keystore(keystore_path, password)
        .wrap_err_with(|| format!("Failed to decrypt keystore at {}", keystore_path.display()))
}

/// Wasm sibling of [`load_signer_from_keystore`].
///
/// Keystore decryption is native-only, so this always fails with
/// [`KeystoreError::WasmUnsupported`].
#[cfg(target_arch = "wasm32")]
pub fn load_signer_from_keystore(
    _keystore_path: &Path,
    _password: &str,
) -> Result<PrivateKeySigner> {
    Err(KeystoreError::WasmUnsupported.into())
}

/// Create a new random signer and save it to a keystore.
///
/// Native-only: see the module docs for why keystore creation is unavailable on
/// wasm. The wasm sibling returns [`KeystoreError::WasmUnsupported`].
#[cfg(not(target_arch = "wasm32"))]
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

    // The alloy eth-keystore API pins rand 0.8, so this one call site uses a
    // rand 0.8 RNG rather than the workspace facade (which is rand 0.9). OsRng
    // is getrandom-backed; the version pin here tracks the alloy dependency.
    let (signer, _uuid) =
        PrivateKeySigner::new_keystore(dir, &mut rand_08::rngs::OsRng, password, Some(name))
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

/// Wasm sibling of [`create_and_save_signer`].
///
/// Keystore creation is native-only, so this always fails with
/// [`KeystoreError::WasmUnsupported`]. The wasm client uses an ephemeral
/// identity and never reaches this path.
#[cfg(target_arch = "wasm32")]
pub fn create_and_save_signer(_keystore_path: &Path, _password: &str) -> Result<PrivateKeySigner> {
    Err(KeystoreError::WasmUnsupported.into())
}
