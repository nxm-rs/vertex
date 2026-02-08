//! Log output format configuration.

/// Output format for log messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable terminal format with colors.
    #[default]
    Terminal,
    /// Structured JSON format for log aggregation.
    Json,
}
