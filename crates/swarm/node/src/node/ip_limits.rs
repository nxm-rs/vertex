//! Per-IP inbound connection admission.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::IpAddr;
use std::task::{Context, Poll};

use libp2p::core::transport::PortUse;
use libp2p::core::{ConnectedPoint, Endpoint};
use libp2p::swarm::behaviour::ConnectionEstablished;
use libp2p::swarm::{
    ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm, dummy,
};
use libp2p::{Multiaddr, PeerId};
use tracing::debug;
use vertex_net_local::{AddressScope, classify_ip, extract_ip};
use vertex_swarm_api::SwarmNetworkConfig;

/// Build the per-IP inbound admission behaviour from the network
/// configuration: the cap from `max_inbound_per_ip` (`0` disables) and the
/// exempt set from the trusted peers' IPs.
pub(crate) fn build_ip_connection_limits(config: &impl SwarmNetworkConfig) -> IpConnectionLimits {
    let exempt_ips = config
        .trusted_peers()
        .iter()
        .filter_map(extract_ip)
        .map(|ip| ip.to_canonical())
        .collect();
    let limit = match config.max_inbound_per_ip() {
        0 => None,
        n => Some(n),
    };
    IpConnectionLimits::new(limit, exempt_ips)
}

/// Source grouping key: one IPv4 address, or one IPv6 /64 prefix. A single
/// host routinely holds a whole /64, so per-address IPv6 counting would not
/// bound anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IpGroup {
    V4(std::net::Ipv4Addr),
    V6(u64),
}

impl From<IpAddr> for IpGroup {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(v4),
            IpAddr::V6(v6) => Self::V6((v6.to_bits() >> 64) as u64),
        }
    }
}

/// An inbound connection was denied because its source IP group already holds
/// the configured number of established inbound connections.
#[derive(Debug, thiserror::Error)]
#[error("inbound connection limit of {limit} per IP reached")]
pub struct IpLimitExceeded {
    limit: u32,
}

impl IpLimitExceeded {
    /// The configured per-IP cap in force when the connection was denied.
    pub fn limit(&self) -> u32 {
        self.limit
    }
}

/// Caps established inbound connections per source IP group, denying at the
/// pending stage (before any protocol upgrade work) and re-checking at
/// establishment.
///
/// Denial happens before the handshake and the peer registry ever see the
/// connection, so a capped connection can never displace an incumbent and
/// needs no teardown of its own. Outbound dials are never counted or denied.
pub(crate) struct IpConnectionLimits {
    /// `None` means unlimited (no bookkeeping at all).
    limit: Option<u32>,
    /// Canonicalized IPs of configured trusted peers, never capped.
    exempt_ips: HashSet<IpAddr>,
    /// Exempt non-public source scopes (loopback, private, link-local).
    /// Disabled only by tests that dial over loopback.
    exempt_local: bool,
    established: HashMap<IpGroup, u32>,
    connections: HashMap<ConnectionId, IpGroup>,
}

impl IpConnectionLimits {
    pub(crate) fn new(limit: Option<u32>, exempt_ips: HashSet<IpAddr>) -> Self {
        Self {
            limit,
            exempt_ips,
            exempt_local: true,
            established: HashMap::new(),
            connections: HashMap::new(),
        }
    }

    /// The counted group for a remote, or `None` when the source is exempt
    /// (no extractable IP, a trusted IP, or a non-public scope).
    fn counted_group(&self, remote: &Multiaddr) -> Option<IpGroup> {
        let ip = extract_ip(remote)?.to_canonical();
        if self.exempt_ips.contains(&ip) {
            return None;
        }
        if self.exempt_local && classify_ip(ip) != Some(AddressScope::Public) {
            return None;
        }
        Some(IpGroup::from(ip))
    }

    fn check(&self, remote: &Multiaddr) -> Result<(), ConnectionDenied> {
        let Some(limit) = self.limit else {
            return Ok(());
        };
        let Some(group) = self.counted_group(remote) else {
            return Ok(());
        };
        let current = self.established.get(&group).copied().unwrap_or(0);
        if current >= limit {
            debug!(%remote, limit, "denying inbound connection: per-IP cap reached");
            return Err(ConnectionDenied::new(IpLimitExceeded { limit }));
        }
        Ok(())
    }

    fn note_established(&mut self, id: ConnectionId, remote: &Multiaddr) {
        if self.limit.is_none() {
            return;
        }
        let Some(group) = self.counted_group(remote) else {
            return;
        };
        self.connections.insert(id, group);
        *self.established.entry(group).or_insert(0) += 1;
    }

    fn note_closed(&mut self, id: ConnectionId) {
        let Some(group) = self.connections.remove(&id) else {
            return;
        };
        if let Some(count) = self.established.get_mut(&group) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.established.remove(&group);
            }
        }
    }
}

impl NetworkBehaviour for IpConnectionLimits {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_pending_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.check(remote_addr)
    }

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.check(remote_addr)?;
        Ok(dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(ConnectionEstablished {
                connection_id,
                endpoint: ConnectedPoint::Listener { send_back_addr, .. },
                ..
            }) => {
                self.note_established(connection_id, send_back_addr);
            }
            FromSwarm::ConnectionClosed(ConnectionClosed { connection_id, .. }) => {
                self.note_closed(connection_id);
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid multiaddr")
    }

    fn conn(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    fn limits(limit: u32) -> IpConnectionLimits {
        IpConnectionLimits::new(Some(limit), HashSet::new())
    }

    fn assert_denied_at(limits: &IpConnectionLimits, remote: &Multiaddr, expected_limit: u32) {
        let denied = limits.check(remote).expect_err("connection is denied");
        let exceeded = denied
            .downcast::<IpLimitExceeded>()
            .expect("cause is IpLimitExceeded");
        assert_eq!(exceeded.limit(), expected_limit);
    }

    #[test]
    fn cap_denies_at_limit_and_recovers_on_close() {
        let mut limits = limits(2);
        let remote = addr("/ip4/203.0.113.7/tcp/1634");

        limits.check(&remote).expect("first admitted");
        limits.note_established(conn(1), &remote);
        limits.check(&remote).expect("second admitted");
        limits.note_established(conn(2), &remote);

        assert_denied_at(&limits, &remote, 2);

        // A different IP has its own budget.
        limits
            .check(&addr("/ip4/198.51.100.9/tcp/1634"))
            .expect("other IP admitted");

        // Closing one connection reopens the budget; a second close of the
        // same connection is a no-op.
        limits.note_closed(conn(1));
        limits.check(&remote).expect("admitted after close");
        limits.note_closed(conn(1));
        limits.note_closed(conn(2));
        assert!(limits.established.is_empty(), "counts fully released");
    }

    #[test]
    fn ipv6_counts_per_slash64() {
        let mut limits = limits(1);
        let first = addr("/ip6/2001:db8:1:2::1/tcp/1634");
        let same_prefix = addr("/ip6/2001:db8:1:2:ffff::9/tcp/1634");
        let other_prefix = addr("/ip6/2001:db8:1:3::1/tcp/1634");

        limits.check(&first).expect("first admitted");
        limits.note_established(conn(1), &first);

        assert_denied_at(&limits, &same_prefix, 1);
        limits.check(&other_prefix).expect("different /64 admitted");
    }

    #[test]
    fn mapped_v4_shares_the_v4_group() {
        let mut limits = limits(1);
        let v4 = addr("/ip4/203.0.113.7/tcp/1634");
        let mapped = addr("/ip6/::ffff:203.0.113.7/tcp/1634");

        limits.check(&v4).expect("first admitted");
        limits.note_established(conn(1), &v4);
        assert_denied_at(&limits, &mapped, 1);
    }

    #[test]
    fn local_scopes_are_exempt() {
        let mut limits = limits(1);
        for remote in [
            addr("/ip4/127.0.0.1/tcp/1634"),
            addr("/ip4/192.168.1.10/tcp/1634"),
            addr("/ip4/169.254.0.5/tcp/1634"),
            addr("/ip6/fe80::1/tcp/1634"),
        ] {
            for n in 0..3 {
                limits.check(&remote).expect("local source never capped");
                limits.note_established(conn(n), &remote);
            }
        }
        assert!(limits.established.is_empty(), "exempt sources not counted");
    }

    #[test]
    fn trusted_ips_are_exempt() {
        let trusted: IpAddr = "203.0.113.7".parse().expect("valid IP");
        let mut limits = IpConnectionLimits::new(Some(1), HashSet::from([trusted]));
        let remote = addr("/ip4/203.0.113.7/tcp/1634");

        for n in 0..3 {
            limits.check(&remote).expect("trusted IP never capped");
            limits.note_established(conn(n), &remote);
        }
        assert!(limits.established.is_empty(), "trusted source not counted");
    }

    #[test]
    fn zero_config_disables_the_cap() {
        struct Config;
        impl SwarmNetworkConfig for Config {
            fn listen_addrs(&self) -> &[Multiaddr] {
                &[]
            }
            fn bootnodes(&self) -> &[Multiaddr] {
                &[]
            }
            fn discovery_enabled(&self) -> bool {
                false
            }
            fn max_peers(&self) -> usize {
                400
            }
            fn idle_timeout(&self) -> std::time::Duration {
                std::time::Duration::from_secs(30)
            }
            fn max_inbound_per_ip(&self) -> u32 {
                0
            }
        }

        let mut limits = build_ip_connection_limits(&Config);
        assert!(limits.limit.is_none());
        let remote = addr("/ip4/203.0.113.7/tcp/1634");
        for n in 0..100 {
            limits.check(&remote).expect("unlimited");
            limits.note_established(conn(n), &remote);
        }
        assert!(limits.established.is_empty(), "no bookkeeping when off");
    }

    mod swarm {
        use std::time::Duration;

        use libp2p::Swarm;
        use libp2p::swarm::{ListenError, SwarmEvent};
        use libp2p_swarm_test::SwarmExt as _;

        use super::*;

        fn capped_swarm(limit: u32) -> Swarm<IpConnectionLimits> {
            Swarm::new_ephemeral_tokio(|_| {
                let mut limits = IpConnectionLimits::new(Some(limit), HashSet::new());
                // The test dials over TCP loopback, which the default
                // exemption would wave through.
                limits.exempt_local = false;
                limits
            })
        }

        /// With a cap of 1, the first loopback connection is admitted and the
        /// second is denied with an [`IpLimitExceeded`] cause while the
        /// incumbent stays connected.
        #[tokio::test]
        async fn denies_over_cap_and_keeps_the_incumbent() {
            let mut listener = capped_swarm(1);
            let mut first = capped_swarm(8);
            let mut second = capped_swarm(8);

            listener.listen().with_tcp_addr_external().await;
            let listen_addr = listener
                .external_addresses()
                .next()
                .cloned()
                .expect("listener has a TCP external address");

            first.dial(listen_addr.clone()).expect("dial is initiated");
            let established = async {
                loop {
                    tokio::select! {
                        event = listener.next_swarm_event() => {
                            if matches!(event, SwarmEvent::ConnectionEstablished { .. }) {
                                return;
                            }
                        }
                        _ = first.next_swarm_event() => {}
                    }
                }
            };
            vertex_tasks::time::timeout(Duration::from_secs(10), established)
                .await
                .expect("first connection establishes");

            second.dial(listen_addr).expect("dial is initiated");
            let denial = async {
                loop {
                    tokio::select! {
                        event = listener.next_swarm_event() => {
                            if let SwarmEvent::IncomingConnectionError {
                                error: ListenError::Denied { cause },
                                ..
                            } = event
                            {
                                return cause;
                            }
                        }
                        _ = first.next_swarm_event() => {}
                        _ = second.next_swarm_event() => {}
                    }
                }
            };
            let cause = vertex_tasks::time::timeout(Duration::from_secs(10), denial)
                .await
                .expect("listener denies the second connection");

            let exceeded = cause
                .downcast::<IpLimitExceeded>()
                .expect("denial cause is IpLimitExceeded");
            assert_eq!(exceeded.limit(), 1);
            assert_eq!(
                listener.connected_peers().count(),
                1,
                "the incumbent connection survives the denial"
            );
        }
    }
}
