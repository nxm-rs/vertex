//! Utility functions for the Vertex Swarm node.

use crate::dirs::parse_path;
use eyre::{eyre, Result};
use rand::{rngs::OsRng, RngCore};
use std::{fs, path::Path};

/// Generate a new random secret key for P2P identity
pub fn generate_p2p_secret() -> Result<[u8; 32]> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    Ok(secret)
}

/// Generate a new JWT secret for API authentication
pub fn generate_jwt_secret() -> Result<[u8; 32]> {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    Ok(secret)
}

/// Load or generate a P2P secret key
pub fn load_or_generate_p2p_secret(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();

    if path.exists() {
        // Load existing secret
        let secret_hex = fs::read_to_string(path)?;
        let secret_hex = secret_hex.trim();

        if secret_hex.len() != 64 {
            return Err(eyre!("Invalid P2P secret key length"));
        }

        let mut secret = [0u8; 32];
        hex::decode_to_slice(secret_hex, &mut secret)
            .map_err(|_| eyre!("Invalid P2P secret key format"))?;

        Ok(secret)
    } else {
        // Generate new secret
        let secret = generate_p2p_secret()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Save the secret
        fs::write(path, hex::encode(&secret))?;

        Ok(secret)
    }
}

/// Load or generate a JWT secret
pub fn load_or_generate_jwt_secret(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();

    if path.exists() {
        // Load existing secret
        let secret_hex = fs::read_to_string(path)?;
        let secret_hex = secret_hex.trim();

        if secret_hex.len() != 64 {
            return Err(eyre!("Invalid JWT secret key length"));
        }

        let mut secret = [0u8; 32];
        hex::decode_to_slice(secret_hex, &mut secret)
            .map_err(|_| eyre!("Invalid JWT secret key format"))?;

        Ok(secret)
    } else {
        // Generate new secret
        let secret = generate_jwt_secret()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Save the secret
        fs::write(path, hex::encode(&secret))?;

        Ok(secret)
    }
}

/// Format a size in bytes to a human-readable string
pub fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if size >= TB {
        format!("{:.2} TB", size as f64 / TB as f64)
    } else if size >= GB {
        format!("{:.2} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.2} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.2} KB", size as f64 / KB as f64)
    } else {
        format!("{} bytes", size)
    }
}

/// Format a timestamp as a human-readable date/time
pub fn format_timestamp(timestamp: u64) -> String {
    let datetime = chrono::NaiveDateTime::from_timestamp_opt(timestamp as i64, 0)
        .unwrap_or_else(|| chrono::NaiveDateTime::from_timestamp_opt(0, 0).unwrap());

    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}
