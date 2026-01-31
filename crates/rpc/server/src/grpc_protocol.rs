//! Extension trait for protocols with gRPC service registration.
//!
//! The [`GrpcProtocol`] trait extends [`Protocol`] to add gRPC service
//! registration capability. Protocols that want to expose gRPC services
//! should implement this trait.

use crate::GrpcRegistry;
use vertex_node_api::NodeProtocol;

/// Extension trait for protocols that expose gRPC services.
///
/// This trait extends [`NodeProtocol`] to add gRPC service registration.
/// The `register_grpc` method is called by the node launcher after
/// building the protocol, allowing protocols to register their services.
///
/// # Example
///
/// ```ignore
/// use vertex_rpc_server::{GrpcProtocol, GrpcRegistry};
/// use vertex_node_api::NodeProtocol;
///
/// struct MyProtocol;
///
/// impl NodeProtocol for MyProtocol {
///     // ... protocol implementation
/// }
///
/// impl GrpcProtocol for MyProtocol {
///     fn register_grpc(components: &Self::Components, registry: &mut GrpcRegistry) {
///         registry.add_service(MyServiceServer::new(MyService::from(components)));
///         registry.add_descriptor(MY_FILE_DESCRIPTOR_SET);
///     }
/// }
/// ```
pub trait GrpcProtocol: NodeProtocol {
    /// Register gRPC services backed by the protocol's components.
    ///
    /// Called by the node launcher after building the protocol.
    /// Implementations should add their gRPC services and file descriptors
    /// to the registry.
    fn register_grpc(components: &Self::Components, registry: &mut GrpcRegistry);
}
