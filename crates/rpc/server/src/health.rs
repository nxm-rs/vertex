//! gRPC Health check service implementation.
//!
//! Implements the standard gRPC health checking protocol.
//! See: https://github.com/grpc/grpc/blob/master/doc/health-checking.md

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::broadcast;
use tonic::{Request, Response, Status};

use crate::proto::health::{
    HealthCheckRequest, HealthCheckResponse, health_check_response::ServingStatus,
    health_server::Health,
};

/// Live readiness source for the overall server status.
///
/// Consulted per check so `grpc.health.v1` reflects the node's real readiness
/// rather than a constant.
pub type ReadinessSource = Arc<dyn Fn() -> ServingStatus + Send + Sync>;

/// Health service implementation.
///
/// Tracks the health status of various services and provides
/// both unary and streaming health check endpoints. The overall ("") status is
/// derived from a live [`ReadinessSource`] when one is wired, otherwise from the
/// stored map (`SERVING` by default).
pub struct HealthService {
    /// Service name -> status mapping.
    statuses: Arc<RwLock<HashMap<String, ServingStatus>>>,
    /// Broadcast channel for status updates.
    status_tx: broadcast::Sender<(String, ServingStatus)>,
    /// Live readiness for the overall status, if wired.
    readiness: Option<ReadinessSource>,
}

impl std::fmt::Debug for HealthService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthService")
            .field("has_readiness", &self.readiness.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for HealthService {
    fn default() -> Self {
        let (status_tx, _) = broadcast::channel(16);
        let statuses = Arc::new(RwLock::new(HashMap::new()));

        // Set overall server status to SERVING by default
        statuses
            .write()
            .insert(String::new(), ServingStatus::Serving);

        Self {
            statuses,
            status_tx,
            readiness: None,
        }
    }
}

impl HealthService {
    /// Create a new health service.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire a live readiness source for the overall ("") status.
    #[must_use]
    pub fn with_readiness(mut self, readiness: ReadinessSource) -> Self {
        self.readiness = Some(readiness);
        self
    }

    /// The overall status: the live readiness source when wired, else the stored
    /// overall status (`SERVING` by default).
    fn overall_status(&self) -> ServingStatus {
        match &self.readiness {
            Some(readiness) => readiness(),
            None => self
                .statuses
                .read()
                .get("")
                .copied()
                .unwrap_or(ServingStatus::Serving),
        }
    }

    /// Set the health status of a service.
    ///
    /// Use an empty string for the overall server status.
    pub fn set_status(&self, service: impl Into<String>, status: ServingStatus) {
        let service = service.into();
        self.statuses.write().insert(service.clone(), status);
        // Ignore send errors (no receivers)
        let _ = self.status_tx.send((service, status));
    }

    /// Get the health status of a service.
    pub fn get_status(&self, service: &str) -> Option<ServingStatus> {
        self.statuses.read().get(service).copied()
    }

    /// Mark the server as serving.
    pub fn set_serving(&self) {
        self.set_status("", ServingStatus::Serving);
    }

    /// Mark the server as not serving.
    pub fn set_not_serving(&self) {
        self.set_status("", ServingStatus::NotServing);
    }
}

#[tonic::async_trait]
impl Health for HealthService {
    async fn check(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let service = &request.get_ref().service;

        let status = if service.is_empty() {
            self.overall_status()
        } else {
            self.statuses
                .read()
                .get(service)
                .copied()
                .unwrap_or(ServingStatus::ServiceUnknown)
        };

        // Return NOT_FOUND for unknown services (per spec)
        if status == ServingStatus::ServiceUnknown && !service.is_empty() {
            return Err(Status::not_found(format!("unknown service: {}", service)));
        }

        Ok(Response::new(HealthCheckResponse {
            status: status.into(),
        }))
    }

    type WatchStream =
        Pin<Box<dyn tokio_stream::Stream<Item = Result<HealthCheckResponse, Status>> + Send>>;

    async fn watch(
        &self,
        request: Request<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let service = request.into_inner().service;

        // Get initial status
        let initial_status = if service.is_empty() {
            self.overall_status()
        } else {
            self.statuses
                .read()
                .get(&service)
                .copied()
                .unwrap_or(ServingStatus::ServiceUnknown)
        };

        // Subscribe to updates
        let mut rx = self.status_tx.subscribe();
        let service_filter = service.clone();

        let stream = async_stream::stream! {
            // Send initial status
            yield Ok(HealthCheckResponse {
                status: initial_status.into(),
            });

            // Stream updates for this service
            loop {
                match rx.recv().await {
                    Ok((svc, status)) if svc == service_filter => {
                        yield Ok(HealthCheckResponse {
                            status: status.into(),
                        });
                    }
                    Ok(_) => continue, // Different service, skip
                    Err(broadcast::error::RecvError::Lagged(_)) => continue, // Dropped messages
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_service_default_status() {
        let service = HealthService::new();
        assert_eq!(service.get_status(""), Some(ServingStatus::Serving));
    }

    #[test]
    fn test_health_service_set_status() {
        let service = HealthService::new();

        service.set_status("test", ServingStatus::NotServing);
        assert_eq!(service.get_status("test"), Some(ServingStatus::NotServing));

        service.set_not_serving();
        assert_eq!(service.get_status(""), Some(ServingStatus::NotServing));

        service.set_serving();
        assert_eq!(service.get_status(""), Some(ServingStatus::Serving));
    }

    #[test]
    fn test_health_service_unknown_service() {
        let service = HealthService::new();
        assert_eq!(service.get_status("unknown"), None);
    }

    #[test]
    fn overall_status_without_readiness_uses_stored_map() {
        let service = HealthService::new();
        assert_eq!(service.overall_status(), ServingStatus::Serving);
        service.set_not_serving();
        assert_eq!(service.overall_status(), ServingStatus::NotServing);
    }

    #[test]
    fn overall_status_reflects_live_readiness_over_stored_map() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let ready = Arc::new(AtomicBool::new(false));
        let probe = ready.clone();
        let service = HealthService::new().with_readiness(Arc::new(move || {
            if probe.load(Ordering::SeqCst) {
                ServingStatus::Serving
            } else {
                ServingStatus::NotServing
            }
        }));

        // The stored overall status is SERVING by default, but the live source
        // overrides it.
        assert_eq!(service.overall_status(), ServingStatus::NotServing);
        ready.store(true, Ordering::SeqCst);
        assert_eq!(service.overall_status(), ServingStatus::Serving);

        // Writing the stored map does not shadow the live source.
        service.set_not_serving();
        assert_eq!(service.overall_status(), ServingStatus::Serving);
    }
}
