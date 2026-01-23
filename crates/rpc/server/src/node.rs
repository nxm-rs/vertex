//! Node service implementation for topology and status information.

use std::sync::Arc;

use tonic::{Request, Response, Status};
use vertex_rpc_core::TopologyProvider;

use crate::proto::node::{
    BinInfo, GetStatusRequest, GetStatusResponse, GetTopologyRequest, GetTopologyResponse,
    node_server::Node,
};

/// Node service implementation.
pub struct NodeService {
    topology: Option<Arc<dyn TopologyProvider>>,
}

impl NodeService {
    /// Create a new node service with an optional topology provider.
    pub fn new(topology: Option<Arc<dyn TopologyProvider>>) -> Self {
        Self { topology }
    }
}

#[tonic::async_trait]
impl Node for NodeService {
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        let Some(topology) = &self.topology else {
            return Err(Status::unavailable("topology not configured"));
        };

        Ok(Response::new(GetStatusResponse {
            overlay_address: topology.overlay_address(),
            depth: topology.depth() as u32,
            connected_peers: topology.connected_peers_count() as u32,
            known_peers: topology.known_peers_count() as u32,
            pending_connections: topology.pending_connections_count() as u32,
        }))
    }

    async fn get_topology(
        &self,
        _request: Request<GetTopologyRequest>,
    ) -> Result<Response<GetTopologyResponse>, Status> {
        let Some(topology) = &self.topology else {
            return Err(Status::unavailable("topology not configured"));
        };

        let bin_sizes = topology.bin_sizes();
        let bins: Vec<BinInfo> = bin_sizes
            .iter()
            .enumerate()
            .map(|(po, (connected, known))| {
                let connected_addrs = if *connected > 0 {
                    topology.connected_peers_in_bin(po as u8)
                } else {
                    Vec::new()
                };

                BinInfo {
                    proximity_order: po as u32,
                    connected_peers: *connected as u32,
                    known_peers: *known as u32,
                    connected_peer_addresses: connected_addrs,
                }
            })
            .collect();

        Ok(Response::new(GetTopologyResponse {
            overlay_address: topology.overlay_address(),
            depth: topology.depth() as u32,
            bins,
        }))
    }
}
