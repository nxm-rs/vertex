//! Generic transport seam for serving node components.
//!
//! A node is launched with a [`Transport`]: components register into a
//! per-launch [`Transport::Registry`] via [`ServeWith`], the registry is turned
//! into a bound [`Transport::Server`], and the server is driven to completion.
//! gRPC is one concrete impl ([`GrpcTransport`]); the launch path names no
//! transport, so a node can be served with any transport.

use std::future::Future;
use std::net::SocketAddr;

use crate::RegistersGrpcServices;
use crate::registry::{GrpcRegistry, GrpcServerHandle};

/// A transport a node can be served with.
pub trait Transport: Send + Sync + 'static {
    /// Per-launch registry the components register into.
    type Registry: Default + Send;
    /// Bound server produced from a populated registry.
    type Server: TransportServer;
    /// Error building the bound server.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build the bound server from a populated registry and bind address.
    fn into_server(reg: Self::Registry, addr: SocketAddr) -> Result<Self::Server, Self::Error>;
}

/// A bound server ready to serve until a shutdown signal fires.
pub trait TransportServer: Send + 'static {
    /// Error returned while serving.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serve until `signal` resolves.
    fn serve_with_shutdown(
        self,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Components that can register themselves into a [`Transport`]'s registry.
pub trait ServeWith<Tr: Transport>: Send + Sync {
    fn register(&self, reg: &mut Tr::Registry);
}

/// gRPC transport: serves components over tonic.
pub struct GrpcTransport;

impl Transport for GrpcTransport {
    type Registry = GrpcRegistry;
    type Server = GrpcServerHandle;
    type Error = tonic_reflection::server::Error;

    fn into_server(reg: Self::Registry, addr: SocketAddr) -> Result<Self::Server, Self::Error> {
        reg.into_server(addr)
    }
}

impl TransportServer for GrpcServerHandle {
    type Error = tonic::transport::Error;

    fn serve_with_shutdown(
        self,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        GrpcServerHandle::serve_with_shutdown(self, signal)
    }
}

/// Any gRPC registrant serves over [`GrpcTransport`].
impl<C: RegistersGrpcServices> ServeWith<GrpcTransport> for C {
    fn register(&self, reg: &mut GrpcRegistry) {
        self.register_grpc_services(reg);
    }
}
