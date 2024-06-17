use crate::metrics::prometheus_exporter;
use metrics_exporter_prometheus::PrometheusHandle;
use once_cell::sync::Lazy;

/// The default prometheus recorder handle. We use a global static to ensure that it is only
/// installed once.
pub static PROMETHEUS_RECORDER_HANDLE: Lazy<PrometheusHandle> =
    Lazy::new(|| prometheus_exporter::install_recorder().unwrap());

#[derive(Debug, Clone)]
pub struct NodeConfig {

}

impl NodeConfig {
    /// Installs the prometheus recorder.
    pub fn install_prometheus_recorder(&self) -> eyre::Result<PrometheusHandle> {
        Ok(PROMETHEUS_RECORDER_HANDLE.clone())
    }
}