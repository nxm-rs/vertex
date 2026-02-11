//! Helper macros for lazy metric initialization.

/// Create a lazily-initialized counter (e.g., `lazy_counter!("requests_total")`).
#[macro_export]
macro_rules! lazy_counter {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::counter!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized gauge (e.g., `lazy_gauge!("connections_active")`).
#[macro_export]
macro_rules! lazy_gauge {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::gauge!($name $(, $label => $value)*))
    };
}

/// Create a lazily-initialized histogram (e.g., `lazy_histogram!("request_duration_seconds")`).
#[macro_export]
macro_rules! lazy_histogram {
    ($name:expr $(, $label:expr => $value:expr)* $(,)?) => {
        std::sync::LazyLock::new(|| ::metrics::histogram!($name $(, $label => $value)*))
    };
}
