//! Node service implementation for Swarm topology and status information.

use hex::FromHex;
use tonic::{Request, Response, Status};
use vertex_swarm_api::{
    PeerConnectionDirection, PeerDiagnostics as ApiPeerDiagnostics, PeerTrustLevel,
    SwarmTopologyAdmin, SwarmTopologyCommands, SwarmTopologyPeers, SwarmTopologyState,
    SwarmTopologyStats,
};
use vertex_swarm_primitives::{Bin, OverlayAddress};

use crate::proto::node::{
    AddPeerRequest, AddPeerResponse, BinInfo, ConnectionDirection, GetStatusRequest,
    GetStatusResponse, GetTopologyRequest, GetTopologyResponse, ListPeersRequest,
    ListPeersResponse, PeerDiagnostics, PeerInfo, RemovePeerRequest, RemovePeerResponse,
    TrustLevel, node_server::Node,
};

/// Parse a hex-encoded overlay address (with optional `0x` prefix).
#[allow(clippy::result_large_err)]
fn parse_overlay(hex: &str) -> Result<OverlayAddress, Status> {
    let trimmed = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = <[u8; 32]>::from_hex(trimmed)
        .map_err(|_| Status::invalid_argument(format!("invalid overlay address: {hex}")))?;
    Ok(OverlayAddress::from(bytes))
}

fn proto_direction(direction: Option<PeerConnectionDirection>) -> i32 {
    match direction {
        Some(PeerConnectionDirection::Inbound) => ConnectionDirection::Inbound as i32,
        Some(PeerConnectionDirection::Outbound) => ConnectionDirection::Outbound as i32,
        None => ConnectionDirection::Unspecified as i32,
    }
}

fn proto_trust(trust: PeerTrustLevel) -> i32 {
    match trust {
        PeerTrustLevel::Normal => TrustLevel::Normal as i32,
        PeerTrustLevel::LocalSubnet => TrustLevel::LocalSubnet as i32,
        PeerTrustLevel::Trusted => TrustLevel::Trusted as i32,
    }
}

fn proto_diagnostics(d: ApiPeerDiagnostics) -> PeerDiagnostics {
    PeerDiagnostics {
        overlay: d.overlay.to_string(),
        peer_id: d.peer_id.map(|p| p.to_string()),
        multiaddrs: d.multiaddrs.iter().map(|m| m.to_string()).collect(),
        ip: d.ip.map(|ip| ip.to_string()),
        proximity_order: u32::from(d.proximity_order),
        score: d.score,
        connected: d.connected,
        connected_since: d.connected_since,
        uptime_secs: d.uptime_secs,
        direction: proto_direction(d.direction),
        trust: proto_trust(d.trust),
        verified: d.verified,
    }
}

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
impl<T> Node for NodeService<T>
where
    T: SwarmTopologyState
        + SwarmTopologyStats
        + SwarmTopologyPeers
        + SwarmTopologyAdmin
        + SwarmTopologyCommands
        + Send
        + Sync
        + 'static,
{
    async fn get_status(
        &self,
        _request: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        Ok(Response::new(GetStatusResponse {
            overlay_address: self.topology.overlay_address().to_string(),
            depth: u32::from(self.topology.depth().get()),
            connected_peers: self.topology.connected_peers_count() as u32,
            known_peers: self.topology.routing_peers_count() as u32,
            pending_connections: self.topology.pending_connections_count() as u32,
            stored_peers: self.topology.stored_peers_count() as u32,
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
            .map(|(idx, (connected, known))| {
                let (connected_addrs, peer_info) = if *connected > 0 {
                    let details = self
                        .topology
                        .connected_peer_details_in_bin(Bin::new(idx as u8).unwrap_or(Bin::MAX));
                    let addrs = details.iter().map(|(o, _)| o.to_string()).collect();
                    let info = details
                        .into_iter()
                        .map(|(overlay, multiaddrs)| PeerInfo {
                            overlay: overlay.to_string(),
                            multiaddrs: multiaddrs.iter().map(|m| m.to_string()).collect(),
                        })
                        .collect();
                    (addrs, info)
                } else {
                    (Vec::new(), Vec::new())
                };

                BinInfo {
                    proximity_order: idx as u32,
                    connected_peers: *connected as u32,
                    known_peers: *known as u32,
                    connected_peer_addresses: connected_addrs,
                    connected_peer_info: peer_info,
                }
            })
            .collect();

        Ok(Response::new(GetTopologyResponse {
            overlay_address: self.topology.overlay_address().to_string(),
            depth: u32::from(self.topology.depth().get()),
            bins,
        }))
    }

    async fn add_peer(
        &self,
        request: Request<AddPeerRequest>,
    ) -> Result<Response<AddPeerResponse>, Status> {
        let req = request.into_inner();
        let addr: libp2p::Multiaddr = req
            .multiaddr
            .parse()
            .map_err(|e| Status::invalid_argument(format!("invalid multiaddr: {e}")))?;

        self.topology
            .dial(addr)
            .await
            .map_err(|e| Status::unavailable(format!("dial command failed: {e}")))?;

        Ok(Response::new(AddPeerResponse { accepted: true }))
    }

    async fn remove_peer(
        &self,
        request: Request<RemovePeerRequest>,
    ) -> Result<Response<RemovePeerResponse>, Status> {
        let req = request.into_inner();
        let overlay = parse_overlay(&req.overlay)?;

        self.topology
            .disconnect(overlay)
            .await
            .map_err(|e| Status::unavailable(format!("disconnect command failed: {e}")))?;

        Ok(Response::new(RemovePeerResponse { accepted: true }))
    }

    async fn list_peers(
        &self,
        request: Request<ListPeersRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        let req = request.into_inner();
        let connected_only = !req.include_known;
        let peers = self
            .topology
            .peer_diagnostics(connected_only)
            .into_iter()
            .map(proto_diagnostics)
            .collect();

        Ok(Response::new(ListPeersResponse { peers }))
    }
}

#[cfg(test)]
mod tests {
    use vertex_swarm_api::{PeerTrustLevel, SwarmTopologyAdmin};
    use vertex_swarm_test_utils::MockTopology;

    use super::*;

    fn diag(overlay: OverlayAddress, connected: bool) -> ApiPeerDiagnostics {
        ApiPeerDiagnostics {
            overlay,
            peer_id: None,
            multiaddrs: Vec::new(),
            ip: None,
            proximity_order: 0,
            score: None,
            connected,
            connected_since: None,
            uptime_secs: None,
            direction: None,
            trust: PeerTrustLevel::Normal,
            verified: false,
        }
    }

    #[test]
    fn parse_overlay_accepts_plain_and_prefixed_hex() {
        let hex = "ff".to_string() + &"00".repeat(31);
        let plain = parse_overlay(&hex).expect("plain hex parses");
        let prefixed = parse_overlay(&format!("0x{hex}")).expect("0x-prefixed hex parses");
        assert_eq!(plain, prefixed);
    }

    #[test]
    fn parse_overlay_rejects_invalid_hex() {
        assert!(parse_overlay("0x").is_err());
        assert!(parse_overlay("not-hex").is_err());
        // Wrong length (16 bytes, not 32).
        assert!(parse_overlay(&"ab".repeat(16)).is_err());
    }

    #[tokio::test]
    async fn list_peers_connected_only_filters_disconnected() {
        let connected = OverlayAddress::from([0x11; 32]);
        let disconnected = OverlayAddress::from([0x22; 32]);
        let topology = MockTopology::default()
            .with_diagnostics(vec![diag(connected, true), diag(disconnected, false)]);

        // connected_only path (include_known = false): only the connected peer.
        let service = NodeService::new(topology.clone());
        let resp = service
            .list_peers(Request::new(ListPeersRequest {
                include_known: false,
            }))
            .await
            .expect("list_peers succeeds")
            .into_inner();
        assert_eq!(resp.peers.len(), 1);
        assert_eq!(resp.peers[0].overlay, connected.to_string());

        // include_known path: both peers.
        assert_eq!(
            SwarmTopologyAdmin::peer_diagnostics(&topology, false).len(),
            2
        );
        let resp = service
            .list_peers(Request::new(ListPeersRequest {
                include_known: true,
            }))
            .await
            .expect("list_peers succeeds")
            .into_inner();
        assert_eq!(resp.peers.len(), 2);
    }
}
