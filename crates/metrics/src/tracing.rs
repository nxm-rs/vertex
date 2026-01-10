//! Tracing system for Vertex Swarm

use crate::TracingConfig;
use eyre::Context;
use opentelemetry::{
    runtime::Tokio,
    sdk::{
        trace::{self, Sampler},
        Resource,
    },
    KeyValue,
};
use std::str::FromStr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Initialize the tracing system
pub fn initialize_tracing(config: &TracingConfig) -> eyre::Result<()> {
    if !config.enabled {
        return Ok(());
    }

    // Create a registry for multiple layers
    let registry = tracing_subscriber::registry();

    // Create an environment filter
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::from_str("info,vertex=debug").unwrap());

    // Add the environment filter layer
    let registry = registry.with(env_filter);

    // Add the opentelemetry layer based on the configured system
    let registry = match config.system {
        crate::TracingSystem::Jaeger => {
            let tracer = init_jaeger_tracer(config)?;
            registry.with(tracing_opentelemetry::layer().with_tracer(tracer))
        }
        crate::TracingSystem::Otlp => {
            let tracer = init_otlp_tracer(config)?;
            registry.with(tracing_opentelemetry::layer().with_tracer(tracer))
        }
    };

    // Initialize the registry
    registry.try_init()?;

    Ok(())
}

/// Initialize a Jaeger tracer
fn init_jaeger_tracer(config: &TracingConfig) -> eyre::Result<trace::Tracer> {
    let endpoint = config
        .jaeger_endpoint
        .as_deref()
        .unwrap_or("http://localhost:14268/api/traces");

    // Create a new Jaeger exporter pipeline
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_endpoint(endpoint)
        .with_service_name(&config.service_name)
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_resource(Resource::new(vec![KeyValue::new(
                    "service.version",
                    env!("CARGO_PKG_VERSION"),
                )])),
        )
        .install_batch(Tokio)
        .context("Failed to install Jaeger tracer")?;

    Ok(tracer)
}

/// Initialize an OTLP tracer
fn init_otlp_tracer(config: &TracingConfig) -> eyre::Result<trace::Tracer> {
    let endpoint = config
        .otlp_endpoint
        .as_deref()
        .unwrap_or("http://localhost:4317");

    // Create a new OTLP exporter pipeline
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_resource(Resource::new(vec![
                    KeyValue::new("service.name", config.service_name.clone()),
                    KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                ])),
        )
        .install_batch(Tokio)
        .context("Failed to install OTLP tracer")?;

    Ok(tracer)
}
