//! Node service implementation for Swarm topology and status information.

use tonic::{Request, Response, Status};
use vertex_swarm_api::{SwarmTopology, TopologyStats};

use crate::proto::node::{
    BinInfo, GetStatusRequest, GetStatusResponse, GetTopologyRequest, GetTopologyResponse,
    PeerInfo, node_server::Node,
};

/// Node service implementation.
///
/// Provides gRPC endpoints for querying Swarm node status and topology.
pub struct NodeService<T> {
    topology: T,
}

impl<T> NodeService<T> {
    pub fn new(topology: T) -> Self {
        Self { topology }
    }
}

#[tonic::async_trait]
impl<T: SwarmTopology + TopologyStats + Send + Sync + 'static> Node for NodeService<T> {
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            overlay_address: self.topology.overlay_address(),
            depth: self.topology.depth() as u32,
            connected_peers: self.topology.connected_peers_count() as u32,
            known_peers: self.topology.known_peers_count() as u32,
            pending_connections: self.topology.pending_connections_count() as u32,
        }))
    }

    async fn get_topology(
        &self,
        _request: Request<GetTopologyRequest>,
    ) -> Result<Response<GetTopologyResponse>, Status> {
        let bin_sizes = self.topology.bin_sizes();
        let bins: Vec<BinInfo> = bin_sizes
            .iter()
            .enumerate()
            .map(|(po, (connected, known))| {
                let (connected_addrs, peer_info) = if *connected > 0 {
                    let details = self.topology.connected_peer_details_in_bin(po as u8);
                    let addrs = details.iter().map(|(hex, _)| hex.clone()).collect();
                    let info = details
                        .into_iter()
                        .map(|(overlay, multiaddrs)| PeerInfo { overlay, multiaddrs })
                        .collect();
                    (addrs, info)
                } else {
                    (Vec::new(), Vec::new())
                };

                BinInfo {
                    proximity_order: po as u32,
                    connected_peers: *connected as u32,
                    known_peers: *known as u32,
                    connected_peer_addresses: connected_addrs,
                    connected_peer_info: peer_info,
                }
            })
            .collect();

        Ok(Response::new(GetTopologyResponse {
            overlay_address: self.topology.overlay_address(),
            depth: self.topology.depth() as u32,
            bins,
        }))
    }
}
