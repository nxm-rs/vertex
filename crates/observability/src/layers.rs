//! Internal layer building for tracing-subscriber.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
    Resource,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

use crate::{FileConfig, LogFormat, OtlpConfig, StdoutConfig, TracingGuard};

/// Build the complete subscriber from configs and initialize it.
pub(crate) fn build_and_init(
    stdout: Option<&StdoutConfig>,
    file: Option<&FileConfig>,
    otlp: Option<&OtlpConfig>,
) -> eyre::Result<TracingGuard> {
    let (console_layer, env_filter) = build_console_layer(stdout);
    let (file_layer, file_guard) = build_file_layer(file)?;
    let (otel_layer, provider) = build_otel_layer(otlp)?;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(console_layer)
        .with(file_layer)
        .with(otel_layer)
        .try_init()
        .map_err(|e| eyre::eyre!("Failed to initialize tracing subscriber: {e}"))?;

    if let Some(cfg) = otlp {
        tracing::info!(
            service_name = %cfg.service_name(),
            endpoint = %cfg.endpoint(),
            "OpenTelemetry tracing initialized"
        );
    }

    Ok(TracingGuard::new(provider, file_guard))
}

fn build_console_layer<S>(
    config: Option<&StdoutConfig>,
) -> (Option<Box<dyn Layer<S> + Send + Sync + 'static>>, EnvFilter)
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    let Some(config) = config else {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"));
        return (None, filter);
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(config.filter()));

    let layer = fmt::layer().with_ansi(config.ansi());

    let layer: Box<dyn Layer<S> + Send + Sync + 'static> = match config.format() {
        LogFormat::Terminal => Box::new(layer),
        LogFormat::Json => Box::new(layer.json()),
    };

    (Some(layer), filter)
}

fn build_file_layer<S>(
    config: Option<&FileConfig>,
) -> eyre::Result<(
    Option<Box<dyn Layer<S> + Send + Sync + 'static>>,
    Option<WorkerGuard>,
)>
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

    let layer: Box<dyn Layer<S> + Send + Sync + 'static> = match config.format() {
        LogFormat::Terminal => Box::new(layer),
        LogFormat::Json => Box::new(layer.json()),
    };

    Ok((Some(layer), Some(guard)))
}

fn build_otel_layer<S>(
    config: Option<&OtlpConfig>,
) -> eyre::Result<(
    Option<Box<dyn Layer<S> + Send + Sync + 'static>>,
    Option<SdkTracerProvider>,
)>
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
