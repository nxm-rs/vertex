//! Lightweight metric primitives: RAII guards, lazy macros, and label utilities.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

pub mod guards;
mod label_value;
pub mod labels;
mod macros;
pub mod protocol;

pub use guards::{
    CounterGuard, GaugeGuard, OperationGuard, TimingGuard, timed_lock, timed_read, timed_write,
};
pub use label_value::LabelValue;
pub use protocol::StreamGuard;

/// Re-export for macro hygiene (`lazy_counter!` etc. expand to `::metrics::*`).
pub use ::metrics as metrics_crate;

/// Re-export for deriving [`LabelValue`] via `strum::IntoStaticStr`.
pub use ::strum;

/// Generate a `record()` method on an error type that emits a counter with a `reason` label.
///
/// Requires the type to derive `strum::IntoStaticStr`.
///
/// # Example
///
/// ```ignore
/// impl MyError {
///     vertex_metrics::impl_record_error!("my_errors_total");
/// }
/// ```
#[macro_export]
macro_rules! impl_record_error {
    ($counter_name:expr) => {
        /// Record this error in metrics.
        pub fn record(&self) {
            let reason: &'static str = self.into();
            $crate::metrics_crate::counter!($counter_name, "reason" => reason).increment(1);
        }
    };
}
