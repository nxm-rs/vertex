//! Helper macros for metrics and observability.
//!
//! These macros reduce boilerplate for common patterns like lazy-initialized
//! static metrics.

/// Create a lazily-initialized counter.
///
/// Uses `std::sync::LazyLock` to ensure the counter is registered after
/// the metrics recorder is installed.
///
/// # Example
///
/// ```rust
/// use std::sync::LazyLock;
/// use metrics::Counter;
/// use vertex_observability::lazy_counter;
///
/// static REQUESTS: LazyLock<Counter> = lazy_counter!("http_requests_total");
/// static ERRORS: LazyLock<Counter> = lazy_counter!("http_errors_total", "code" => "500");
///
/// fn handle_request() {
///     REQUESTS.increment(1);
/// }
/// ```
#[macro_export]
macro_rules! lazy_counter {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| $crate::metrics_crate::counter!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized gauge.
///
/// Uses `std::sync::LazyLock` to ensure the gauge is registered after
/// the metrics recorder is installed.
///
/// # Example
///
/// ```rust
/// use std::sync::LazyLock;
/// use metrics::Gauge;
/// use vertex_observability::lazy_gauge;
///
/// static CONNECTIONS: LazyLock<Gauge> = lazy_gauge!("active_connections");
/// static TASKS: LazyLock<Gauge> = lazy_gauge!("tasks_running", "type" => "critical");
/// ```
#[macro_export]
macro_rules! lazy_gauge {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| $crate::metrics_crate::gauge!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized histogram.
///
/// Uses `std::sync::LazyLock` to ensure the histogram is registered after
/// the metrics recorder is installed.
///
/// # Example
///
/// ```rust
/// use std::sync::LazyLock;
/// use metrics::Histogram;
/// use vertex_observability::lazy_histogram;
///
/// static LATENCY: LazyLock<Histogram> = lazy_histogram!("request_duration_seconds");
/// static DB_LATENCY: LazyLock<Histogram> = lazy_histogram!("db_query_seconds", "table" => "users");
/// ```
#[macro_export]
macro_rules! lazy_histogram {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| $crate::metrics_crate::histogram!($name $(, $label => $value)*))
    };
}
