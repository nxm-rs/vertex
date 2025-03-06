//! Logging system for Vertex Swarm

use crate::LoggingConfig;
use eyre::Context;
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    sync::Arc,
};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

/// Initialize the logging system
pub fn initialize_logging(config: &LoggingConfig) -> eyre::Result<()> {
    // Create a registry for multiple layers
    let registry = tracing_subscriber::registry();

    // Create an environment filter based on the configured level or env var
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!(
            "info,vertex={}",
            config.level.to_string().to_lowercase()
        ))
    });

    // Always add a stdout layer
    let stdout_layer = fmt::Layer::new()
        .with_span_events(FmtSpan::CLOSE)
        .with_ansi(true);

    let stdout_layer = if config.json {
        stdout_layer.json().boxed()
    } else {
        stdout_layer.boxed()
    };

    let registry = registry.with(stdout_layer);

    // Add a file logger if configured
    let registry = if let Some(log_dir) = &config.log_dir {
        let log_file = setup_log_file(log_dir, config.max_file_size_mb, config.max_files)?;

        let file_layer = fmt::Layer::new()
            .with_span_events(FmtSpan::CLOSE)
            .with_ansi(false)
            .with_writer(Arc::new(log_file));

        let file_layer = if config.json {
            file_layer.json().boxed()
        } else {
            file_layer.boxed()
        };

        registry.with(file_layer)
    } else {
        registry
    };

    // Set the global subscriber
    registry.with(env_filter).try_init()?;

    Ok(())
}

/// Set up a log file with rotation based on size
fn setup_log_file(
    dir: impl AsRef<Path>,
    max_size_mb: u64,
    max_files: usize,
) -> eyre::Result<RotatingFile> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir).context("Failed to create log directory")?;

    let log_path = dir.join("vertex-swarm.log");
    let file = File::options()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("Failed to open log file")?;

    Ok(RotatingFile {
        file,
        path: log_path,
        max_size: max_size_mb * 1024 * 1024,
        max_files,
        current_size: 0,
    })
}

/// A writer that rotates the underlying file when it reaches a certain size
pub struct RotatingFile {
    /// The current log file
    file: File,
    /// The path to the log file
    path: std::path::PathBuf,
    /// Maximum file size in bytes
    max_size: u64,
    /// Maximum number of log files
    max_files: usize,
    /// Current file size in bytes
    current_size: u64,
}

impl RotatingFile {
    /// Rotate the file if it's too large
    fn maybe_rotate(&mut self) -> io::Result<()> {
        // Get the current file size
        let metadata = self.file.metadata()?;
        self.current_size = metadata.len();

        if self.current_size >= self.max_size {
            // Rotate the files
            self.rotate()?;
        }

        Ok(())
    }

    /// Rotate log files
    fn rotate(&mut self) -> io::Result<()> {
        // Close the current file
        self.file.flush()?;
        drop(&mut self.file);

        // Rotate the existing files
        for i in (1..self.max_files).rev() {
            let src = self.path.with_extension(format!("{}.log", i - 1));
            let dst = self.path.with_extension(format!("{}.log", i));
            if src.exists() {
                std::fs::rename(&src, &dst)?;
            }
        }

        // Rename the current file
        let backup = self.path.with_extension("0.log");
        std::fs::rename(&self.path, &backup)?;

        // Create a new file
        self.file = File::options().create(true).append(true).open(&self.path)?;
        self.current_size = 0;

        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Check if we need to rotate
        self.maybe_rotate()?;

        // Write to the file
        let bytes_written = self.file.write(buf)?;
        self.current_size += bytes_written as u64;

        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
