//! Helper macros for lazy metric initialization.

/// Create a lazily-initialized counter.
///
/// Uses `LazyLock` to ensure the counter is registered after the metrics recorder is installed.
///
/// # Example
///
/// ```ignore
/// use std::sync::LazyLock;
/// use metrics::Counter;
/// use vertex_tasks::lazy_counter;
///
/// static REQUESTS: LazyLock<Counter> = lazy_counter!("http_requests_total");
/// static ERRORS: LazyLock<Counter> = lazy_counter!("http_errors_total", "code" => "500");
/// ```
#[macro_export]
macro_rules! lazy_counter {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::counter!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized gauge.
///
/// Uses `LazyLock` to ensure the gauge is registered after the metrics recorder is installed.
///
/// # Example
///
/// ```ignore
/// use std::sync::LazyLock;
/// use metrics::Gauge;
/// use vertex_tasks::lazy_gauge;
///
/// static CONNECTIONS: LazyLock<Gauge> = lazy_gauge!("active_connections");
/// static TASKS: LazyLock<Gauge> = lazy_gauge!("tasks_running", "type" => "critical");
/// ```
#[macro_export]
macro_rules! lazy_gauge {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::gauge!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized histogram.
///
/// Uses `LazyLock` to ensure the histogram is registered after the metrics recorder is installed.
///
/// # Example
///
/// ```ignore
/// use std::sync::LazyLock;
/// use metrics::Histogram;
/// use vertex_tasks::lazy_histogram;
///
/// static LATENCY: LazyLock<Histogram> = lazy_histogram!("request_duration_seconds");
/// static DB_LATENCY: LazyLock<Histogram> = lazy_histogram!("db_query_seconds", "table" => "users");
/// ```
#[macro_export]
macro_rules! lazy_histogram {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::histogram!($name $(, $label => $value)*))
    };
}
