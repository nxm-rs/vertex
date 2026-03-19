//! Database storage abstraction layer.
//!
//! Provides traits for key/value storage with pluggable backends (redb, in-memory, etc.).

mod error;
mod table;
mod traits;

mod codecs;

pub use error::*;
pub use table::*;
pub use traits::*;
