//! Internal layer building for tracing-subscriber.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider,
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
    Resource,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

use crate::{FileConfig, LogFormat, OtlpConfig, OtlpLogsConfig, StdoutConfig, TracingGuard};

/// Boxed tracing layer, used in return types to reduce complexity.
type BoxedLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Build the complete subscriber from configs and initialize it.
pub(crate) fn build_and_init(
    stdout: Option<&StdoutConfig>,
    file: Option<&FileConfig>,
    otlp: Option<&OtlpConfig>,
    otlp_logs: Option<&OtlpLogsConfig>,
) -> eyre::Result<TracingGuard> {
    let (console_layer, env_filter) = build_console_layer(stdout);
    let (file_layer, file_guard) = build_file_layer(file)?;
    let (otel_layer, tracer_provider) = build_otel_layer(otlp)?;
    let (otel_logs_layer, logger_provider) = build_otel_logs_layer(otlp_logs)?;

    // Build tokio-console layer if feature enabled.
    // spawn() returns a layer with its own filter for tokio/runtime spans.
    #[cfg(feature = "tokio-console")]
    let tokio_console_layer = Some(console_subscriber::spawn());

    #[cfg(not(feature = "tokio-console"))]
    let tokio_console_layer: Option<tracing_subscriber::layer::Identity> = None;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .with(otel_layer)
        .with(otel_logs_layer)
        .with(tokio_console_layer)
        .try_init()
        .map_err(|e| eyre::eyre!("Failed to initialize tracing subscriber: {e}"))?;

    if let Some(cfg) = otlp {
        tracing::info!(
            service_name = %cfg.service_name(),
            endpoint = %cfg.endpoint(),
            "OpenTelemetry tracing initialized"
        );
    }

    if let Some(cfg) = otlp_logs {
        tracing::info!(
            endpoint = %cfg.endpoint(),
            "OTLP log export initialized"
        );
    }

    Ok(TracingGuard::new(tracer_provider, logger_provider, file_guard))
}

fn build_console_layer<S>(
    config: Option<&StdoutConfig>,
) -> (Option<BoxedLayer<S>>, EnvFilter)
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    // Base filter from config or environment
    let base_filter = config
        .map(|c| c.filter().to_string())
        .unwrap_or_else(|| "error".to_string());

    // When tokio-console is enabled, we must allow tokio spans through
    #[cfg(feature = "tokio-console")]
    let filter_str = format!("{},tokio=trace,runtime=trace", base_filter);

    #[cfg(not(feature = "tokio-console"))]
    let filter_str = base_filter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&filter_str));

    let Some(config) = config else {
        return (None, filter);
    };

    let layer = fmt::layer().with_ansi(config.ansi());

    let layer: BoxedLayer<S> = match config.format() {
        LogFormat::Terminal => Box::new(layer),
        LogFormat::Json => Box::new(layer.json()),
    };

    (Some(layer), filter)
}

fn build_file_layer<S>(
    config: Option<&FileConfig>,
) -> eyre::Result<(Option<BoxedLayer<S>>, Option<WorkerGuard>)>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let Some(config) = config else {
        return Ok((None, None));
    };

    std::fs::create_dir_all(config.directory())?;

    let file_appender = tracing_appender::rolling::daily(config.directory(), config.filename());
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let layer = fmt::layer().with_ansi(false).with_writer(non_blocking);

    let layer: BoxedLayer<S> = match config.format() {
        LogFormat::Terminal => Box::new(layer),
        LogFormat::Json => Box::new(layer.json()),
    };

    Ok((Some(layer), Some(guard)))
}

fn build_otel_layer<S>(
    config: Option<&OtlpConfig>,
) -> eyre::Result<(Option<BoxedLayer<S>>, Option<SdkTracerProvider>)>
where
    S: tracing::Subscriber
        + for<'span> tracing_subscriber::registry::LookupSpan<'span>
        + Send
        + Sync,
{
    let Some(config) = config else {
        return Ok((None, None));
    };

    use opentelemetry::KeyValue;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(config.endpoint())
        .build()?;

    let resource = Resource::builder()
        .with_attributes([KeyValue::new(
            "service.name",
            config.service_name().to_string(),
        )])
        .build();

    let sampler = if config.sampling_ratio() >= 1.0 {
        Sampler::AlwaysOn
    } else if config.sampling_ratio() <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sampling_ratio())
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    let tracer = provider.tracer(config.service_name().to_string());
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((Some(Box::new(layer)), Some(provider)))
}

fn build_otel_logs_layer<S>(
    config: Option<&OtlpLogsConfig>,
) -> eyre::Result<(Option<BoxedLayer<S>>, Option<SdkLoggerProvider>)>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let Some(config) = config else {
        return Ok((None, None));
    };

    use opentelemetry::KeyValue;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;

    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(config.endpoint())
        .build()?;

    let resource = Resource::builder()
        .with_attributes([KeyValue::new(
            "service.name",
            config.service_name().to_string(),
        )])
        .build();

    let provider = SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let layer = OpenTelemetryTracingBridge::new(&provider);

    Ok((Some(Box::new(layer)), Some(provider)))
}
