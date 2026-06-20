//! gRPC service registry: protocols register their services during the build
//! phase, then the registry composes them into a tonic server.

use std::net::SocketAddr;
use tonic::service::Routes;

/// Collects gRPC services and reflection file descriptors, then builds a tonic
/// server.
///
/// # Example
///
/// ```ignore
/// use vertex_rpc_server::GrpcRegistry;
///
/// let mut registry = GrpcRegistry::new();
/// registry.add_service(MyServiceServer::new(my_service));
/// registry.add_descriptor(MY_FILE_DESCRIPTOR_SET);
/// let server = registry.into_server(addr)?;
/// ```
#[derive(Default)]
pub struct GrpcRegistry {
    routes: Option<Routes>,
    descriptors: Vec<&'static [u8]>,
}

impl GrpcRegistry {
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Add file descriptors for gRPC reflection (used by tools like `grpcurl`).
    pub fn add_descriptor(&mut self, descriptor: &'static [u8]) {
        self.descriptors.push(descriptor);
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_none()
    }

    pub fn descriptors(&self) -> &[&'static [u8]] {
        &self.descriptors
    }

    pub fn into_routes(self) -> Option<Routes> {
        self.routes
    }

    /// Build a gRPC server, registering a reflection service if any descriptors
    /// were added.
    pub fn into_server(
        mut self,
        addr: SocketAddr,
    ) -> Result<GrpcServerHandle, tonic_reflection::server::Error> {
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
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn serve(self) -> Result<(), tonic::transport::Error> {
        if let Some(routes) = self.routes {
            configure_server(tonic::transport::Server::builder())
                .add_routes(routes)
                .serve(self.addr)
                .await
        } else {
            Ok(())
        }
    }

    /// Serve with graceful shutdown.
    pub async fn serve_with_shutdown<F>(self, signal: F) -> Result<(), tonic::transport::Error>
    where
        F: std::future::Future<Output = ()>,
    {
        if let Some(routes) = self.routes {
            configure_server(tonic::transport::Server::builder())
                .add_routes(routes)
                .serve_with_shutdown(self.addr, signal)
                .await
        } else {
            signal.await;
            Ok(())
        }
    }
}

/// Max simultaneous HTTP/2 streams per connection. Each streaming chunk RPC is
/// one stream, bounding how many an untrusted client can run at once.
const MAX_CONCURRENT_STREAMS: u32 = 256;
/// Max in-flight requests served concurrently per connection.
const MAX_CONNECTION_CONCURRENCY: usize = 256;

/// Connection-level limits bounding the gRPC amplification surface; streaming
/// chunk RPCs are reachable by untrusted clients.
fn configure_server(builder: tonic::transport::Server) -> tonic::transport::Server {
    builder
        .concurrency_limit_per_connection(MAX_CONNECTION_CONCURRENCY)
        .max_concurrent_streams(Some(MAX_CONCURRENT_STREAMS))
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
