//! Container types for launch context attachments.

use std::sync::Arc;

use vertex_observability::{MetricsServerConfig, PrometheusRecorder};

/// Metrics infrastructure attachment.
#[derive(Debug, Clone)]
pub struct WithMetrics {
    config: Option<MetricsServerConfig>,
    recorder: Option<Arc<PrometheusRecorder>>,
}

impl WithMetrics {
    pub(crate) fn new(
        config: Option<MetricsServerConfig>,
        recorder: Option<Arc<PrometheusRecorder>>,
    ) -> Self {
        Self { config, recorder }
    }

    pub fn config(&self) -> Option<&MetricsServerConfig> {
        self.config.as_ref()
    }

    pub fn recorder(&self) -> Option<&Arc<PrometheusRecorder>> {
        self.recorder.as_ref()
    }
}
