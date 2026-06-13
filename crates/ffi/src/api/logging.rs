//! Structured logging surface for embedding hosts.
//!
//! An embedded Vertex client emits its diagnostics through the `tracing`
//! facade. Without a subscriber installed those events hit the global no-op
//! dispatcher and the host sees nothing. [`init_logging`] installs a subscriber
//! that filters by an `EnvFilter`-style directive and forwards every event to
//! the host as a typed [`LogLine`] stream, so a Dart or other native host can
//! surface node logs in its own UI or log sink.
//!
//! The stream carries [`LogLine`] values, not JSON: a flat struct of the
//! timestamp, level, target, message, and a small set of flattened key-value
//! fields. The host receives them as a Dart `Stream` (or the equivalent in its
//! binding language) via flutter_rust_bridge's `StreamSink`.

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use flutter_rust_bridge::frb;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::error::FfiError;
use crate::frb_generated::StreamSink;

/// Guards against installing the global subscriber more than once.
///
/// `tracing` allows exactly one global subscriber per process. A second
/// [`init_logging`] call is a no-op error rather than a panic, so a host that
/// double-initializes (a hot reload, a retry) does not crash the node.
static INIT: OnceLock<()> = OnceLock::new();

/// Severity of a forwarded log event.
///
/// A typed mirror of `tracing`'s levels so the host matches on an enum instead
/// of parsing a string.
#[frb(non_opaque)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Most verbose: fine-grained developer tracing.
    Trace,
    /// Developer-facing detail.
    Debug,
    /// Operator-facing state changes.
    Info,
    /// Operator-facing, informational but noteworthy.
    Warn,
    /// Operator-facing, requires attention.
    Error,
}

impl From<&Level> for LogLevel {
    fn from(level: &Level) -> Self {
        match *level {
            Level::TRACE => LogLevel::Trace,
            Level::DEBUG => LogLevel::Debug,
            Level::INFO => LogLevel::Info,
            Level::WARN => LogLevel::Warn,
            Level::ERROR => LogLevel::Error,
        }
    }
}

/// A single structured log event crossing the FFI boundary.
///
/// Plain data only: every field is a primitive or a string so the bindings
/// expose it to any host without a serde backend. `fields` carries the event's
/// key-value pairs flattened to strings, with the special `message` field
/// hoisted out into [`LogLine::message`].
#[frb(non_opaque)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Event time in milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
    /// Severity of the event.
    pub level: LogLevel,
    /// The event's target (typically the emitting module path).
    pub target: String,
    /// The event's primary message.
    pub message: String,
    /// Flattened key-value fields captured from the event, message excluded.
    pub fields: Vec<(String, String)>,
}

/// Collects an event's fields into a message and a flat key-value list.
///
/// `tracing` records the formatted message under the reserved `message` field;
/// it is hoisted into `message` and every other field is rendered with its
/// `Debug` (or string) representation into `fields`.
#[frb(ignore)]
#[derive(Default)]
struct LogVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl LogVisitor {
    fn record(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for LogVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut rendered = String::new();
        // Writing to a String is infallible; ignore the formatter result.
        let _ = write!(rendered, "{value:?}");
        self.record(field, rendered);
    }
}

/// Current wall-clock time as milliseconds since the Unix epoch.
///
/// A clock before the epoch is not expected on a host; it clamps to zero rather
/// than failing the event.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build a [`LogLine`] from a tracing event.
fn line_from_event(event: &Event<'_>) -> LogLine {
    let mut visitor = LogVisitor::default();
    event.record(&mut visitor);

    let metadata = event.metadata();
    LogLine {
        timestamp_ms: now_ms(),
        level: metadata.level().into(),
        target: metadata.target().to_string(),
        message: visitor.message,
        fields: visitor.fields,
    }
}

/// A tracing layer that forwards each event to the host as a [`LogLine`].
///
/// Holds the host's [`StreamSink`]. On every event it extracts a [`LogLine`]
/// and pushes it; a send failure (the host closed the stream) is swallowed so a
/// dropped host listener never disturbs the node.
#[frb(ignore)]
struct StreamLayer {
    sink: StreamSink<LogLine>,
}

impl<S> Layer<S> for StreamLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let line = line_from_event(event);
        // The host owns the stream lifetime; a closed sink is not our error.
        let _ = self.sink.add(line);
    }
}

/// Initialize logging for the embedded client.
///
/// Installs a process-global tracing subscriber that filters events by `level`
/// and forwards each surviving event to `sink` as a typed [`LogLine`]. `level`
/// is an `EnvFilter`-style directive: a bare level (`"info"`, `"debug"`) sets
/// the global maximum, and per-target directives (`"info,vertex_topology=debug"`)
/// tune individual modules. An unparseable directive is rejected.
///
/// Call once, before or just after [`super::client::VertexClient::build`]. A
/// second call returns [`FfiError::LoggingAlreadyInitialized`] without
/// disturbing the installed subscriber: `tracing` permits one global subscriber
/// per process.
///
/// To compile logging out entirely, a host sets a `tracing` `release_max_level_*`
/// feature in its own `Cargo.toml`; cargo feature unification then strips the
/// filtered events at compile time and `init_logging` forwards nothing below the
/// chosen level.
#[frb]
pub fn init_logging(level: String, sink: StreamSink<LogLine>) -> Result<(), FfiError> {
    let filter = EnvFilter::try_new(&level).map_err(|e| FfiError::Logging {
        reason: format!("invalid log filter {level:?}: {e}"),
    })?;

    // Reserve the init slot first so a racing second caller loses cleanly
    // before it ever tries to install a subscriber.
    if INIT.set(()).is_err() {
        return Err(FfiError::LoggingAlreadyInitialized);
    }

    let layer = StreamLayer { sink };

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init()
        .map_err(|e| FfiError::Logging {
            reason: format!("install subscriber: {e}"),
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn visitor_hoists_message_and_collects_fields() {
        // Capture a synthetic event by driving the same extraction `on_event`
        // uses, through a tiny capturing subscriber. `with_default` requires a
        // `'static` subscriber, so the capture slot is shared by `Arc`.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        tracing::subscriber::with_default(
            CaptureSubscriber {
                out: captured.clone(),
            },
            || {
                tracing::error!(peer = "abcd", count = 7, "connection lost");
            },
        );

        let line = captured.lock().unwrap().take().expect("event captured");
        assert_eq!(line.level, LogLevel::Error);
        assert_eq!(line.message, "connection lost");
        // A string-valued field is recorded verbatim, without Debug quoting.
        assert!(
            line.fields
                .contains(&("peer".to_string(), "abcd".to_string()))
        );
        assert!(
            line.fields
                .contains(&("count".to_string(), "7".to_string()))
        );
        assert!(line.timestamp_ms > 0);
    }

    #[test]
    fn level_maps_from_tracing() {
        assert_eq!(LogLevel::from(&Level::TRACE), LogLevel::Trace);
        assert_eq!(LogLevel::from(&Level::ERROR), LogLevel::Error);
    }

    #[test]
    fn double_init_guard_does_not_panic() {
        // Drive the guard directly: the real `init_logging` installs a global
        // subscriber, which a unit test must not do. The guard is the part that
        // must never panic on a second call.
        let guard: OnceLock<()> = OnceLock::new();
        assert!(guard.set(()).is_ok());
        assert!(guard.set(()).is_err());
    }

    /// Minimal subscriber that runs the extraction over one event and stores the
    /// resulting [`LogLine`], so the logic is testable without a global
    /// subscriber or a real `StreamSink`.
    struct CaptureSubscriber {
        out: std::sync::Arc<std::sync::Mutex<Option<LogLine>>>,
    }

    impl tracing::Subscriber for CaptureSubscriber {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &Event<'_>) {
            *self.out.lock().unwrap() = Some(line_from_event(event));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
}
