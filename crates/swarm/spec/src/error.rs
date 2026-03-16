//! Error types for swarmspec operations.

/// Error type for SwarmSpec file operations.
#[cfg(feature = "std")]
#[derive(Debug, thiserror::Error)]
pub enum SwarmSpecFileError {
    /// IO error reading/writing file.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML deserialization error.
    #[error("TOML parse error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    /// TOML serialization error.
    #[error("TOML serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
}
