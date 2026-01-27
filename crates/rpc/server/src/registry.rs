//! gRPC service registry for dynamic service composition.
//!
//! The [`GrpcRegistry`] allows protocols to register their gRPC services
//! during the build phase, eliminating the need for a separate "providers"
//! extraction step.

use std::net::SocketAddr;
use tonic::service::Routes;

/// Registry for gRPC services that protocols register during build.
///
/// Collects services and file descriptors, then builds into a tonic server.
///
/// # Example
///
/// ```ignore
/// use vertex_rpc_server::GrpcRegistry;
///
/// let mut registry = GrpcRegistry::new();
///
/// // Protocol registers its services during build
/// registry.add_service(MyServiceServer::new(my_service));
/// registry.add_descriptor(MY_FILE_DESCRIPTOR_SET);
///
/// // Launcher builds the server from the registry
/// let server = registry.into_router();
/// ```
#[derive(Default)]
pub struct GrpcRegistry {
    routes: Option<Routes>,
    descriptors: Vec<&'static [u8]>,
}

impl GrpcRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a gRPC service to the registry.
    ///
    /// Services are accumulated and will be composed into a single router.
    pub fn add_service<S>(&mut self, service: S)
    where
        S: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<tonic::body::BoxBody>,
                Error = std::convert::Infallible,
            > + tonic::server::NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
    {
        self.routes = Some(match self.routes.take() {
            Some(routes) => routes.add_service(service),
            None => Routes::new(service),
        });
    }

    /// Add file descriptors for gRPC reflection.
    ///
    /// Descriptors are used by tools like `grpcurl` for service discovery.
    pub fn add_descriptor(&mut self, descriptor: &'static [u8]) {
        self.descriptors.push(descriptor);
    }

    /// Check if any services have been registered.
    pub fn is_empty(&self) -> bool {
        self.routes.is_none()
    }

    /// Get the registered file descriptors.
    pub fn descriptors(&self) -> &[&'static [u8]] {
        &self.descriptors
    }

    /// Consume the registry and return the composed routes.
    ///
    /// Returns `None` if no services were registered.
    pub fn into_routes(self) -> Option<Routes> {
        self.routes
    }

    /// Build a gRPC server from this registry.
    ///
    /// Includes reflection service if descriptors were registered.
    pub fn into_server(
        mut self,
        addr: SocketAddr,
    ) -> Result<GrpcServerHandle, tonic_reflection::server::Error> {
        // Build reflection service if we have descriptors
        if !self.descriptors.is_empty() {
            let mut reflection_builder = tonic_reflection::server::Builder::configure();
            for desc in &self.descriptors {
                reflection_builder = reflection_builder.register_encoded_file_descriptor_set(desc);
            }
            let reflection_service = reflection_builder.build_v1()?;
            self.add_service(reflection_service);
        }

        Ok(GrpcServerHandle {
            routes: self.routes,
            addr,
        })
    }
}

impl std::fmt::Debug for GrpcRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcRegistry")
            .field("has_routes", &self.routes.is_some())
            .field("descriptor_count", &self.descriptors.len())
            .finish()
    }
}

/// Handle to a configured gRPC server ready to serve.
pub struct GrpcServerHandle {
    routes: Option<Routes>,
    addr: SocketAddr,
}

impl GrpcServerHandle {
    /// Get the address the server will bind to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Serve the gRPC server.
    ///
    /// Returns when the server shuts down.
    pub async fn serve(self) -> Result<(), tonic::transport::Error> {
        if let Some(routes) = self.routes {
            tonic::transport::Server::builder()
                .add_routes(routes)
                .serve(self.addr)
                .await
        } else {
            // No services registered, just return
            Ok(())
        }
    }

    /// Serve with graceful shutdown.
    pub async fn serve_with_shutdown<F>(self, signal: F) -> Result<(), tonic::transport::Error>
    where
        F: std::future::Future<Output = ()>,
    {
        if let Some(routes) = self.routes {
            tonic::transport::Server::builder()
                .add_routes(routes)
                .serve_with_shutdown(self.addr, signal)
                .await
        } else {
            // No services registered, wait for shutdown signal
            signal.await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = GrpcRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.descriptors().is_empty());
    }

    #[test]
    fn test_add_descriptor() {
        let mut registry = GrpcRegistry::new();
        registry.add_descriptor(b"test descriptor");
        assert_eq!(registry.descriptors().len(), 1);
    }
}
