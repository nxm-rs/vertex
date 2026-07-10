//! Transport-suite classification for dial eligibility.
//!
//! IP capability ([`crate::IpCapability`]) answers "can this host route to
//! that address family"; the types here answer "can the assembled libp2p
//! transport stack open that kind of connection at all". A browser client
//! dials only secure websockets, so a peer advertising nothing but raw TCP
//! multiaddrs is undialable for it no matter how routable the IP is, and
//! vice versa for a native stack without a websocket client. Filtering on
//! both halves up front avoids dials that can only fail inside the
//! transport.

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

/// The transport suite a multiaddr requires from the dialer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRequirement {
    /// Raw TCP (optionally behind `/dns*` resolution), no upgrade encoded
    /// in the address.
    Tcp,
    /// Plain (non-TLS) websocket.
    Websocket,
    /// TLS websocket: `/tls/ws` (with or without `/sni`) or the legacy
    /// `/wss` form.
    SecureWebsocket,
    /// QUIC v1 over UDP (`/udp/../quic-v1`), without a `/webtransport`
    /// suffix.
    Quic,
    /// In-process memory transport (`/memory/<port>`). Dialable only by a node
    /// that has a channel-based memory transport injected; the default TCP and
    /// websocket stacks carry none, so a self-signed record advertising one is
    /// rejected pre-dial rather than admitted for a dial that always fails.
    Memory,
    /// Relayed circuit (`/p2p-circuit`). Dialable only through a relay client
    /// transport, never by a bare platform suite, so the circuit component
    /// dominates whatever suite the relay leg rides.
    Relay,
    /// Anything else (legacy draft QUIC, WebTransport); dialable by no
    /// transport stack vertex currently assembles.
    Other,
}

impl TransportRequirement {
    /// Classify the transport suite `addr` requires.
    ///
    /// A websocket component dominates the TCP it rides on, so
    /// `/ip4/../tcp/../tls/ws` classifies as [`Self::SecureWebsocket`],
    /// not [`Self::Tcp`].
    pub fn of(addr: &Multiaddr) -> Self {
        let mut saw_tcp = false;
        let mut saw_tls = false;
        let mut saw_quic = false;

        for proto in addr.iter() {
            match proto {
                Protocol::Memory(_) => return Self::Memory,
                Protocol::P2pCircuit => return Self::Relay,
                Protocol::Tcp(_) => saw_tcp = true,
                Protocol::Tls => saw_tls = true,
                Protocol::QuicV1 => saw_quic = true,
                // A raw QUIC dialer cannot complete the WebTransport
                // handshake the address demands, so the suffix dominates.
                Protocol::WebTransport => return Self::Other,
                Protocol::Wss(_) => return Self::SecureWebsocket,
                Protocol::Ws(_) => {
                    return if saw_tls {
                        Self::SecureWebsocket
                    } else {
                        Self::Websocket
                    };
                }
                _ => {}
            }
        }

        if saw_quic {
            Self::Quic
        } else if saw_tcp {
            Self::Tcp
        } else {
            Self::Other
        }
    }
}

/// Advertisement rank of a transport suite: TCP first, QUIC second, secure
/// websockets third, everything else last.
fn advertise_rank(addr: &Multiaddr) -> u8 {
    match TransportRequirement::of(addr) {
        TransportRequirement::Tcp => 0,
        TransportRequirement::Quic => 1,
        TransportRequirement::SecureWebsocket => 2,
        TransportRequirement::Websocket
        | TransportRequirement::Memory
        | TransportRequirement::Relay
        | TransportRequirement::Other => 3,
    }
}

/// Compare two multiaddrs by transport suite for advertisement ordering: TCP
/// before QUIC before secure websockets before anything else.
///
/// TCP is the one suite every peer on the network dials, so TCP leaves must
/// outlive the additive suites under the handshake record's deterministic
/// prefix truncation. Like [`crate::family_order`] this is a partial key:
/// addresses of the same suite compare `Equal`, so combine it with further
/// tie-breaks or a stable sort.
///
/// ```
/// use vertex_net_local::transport_order;
///
/// let tcp: libp2p::Multiaddr = "/ip4/8.8.8.8/tcp/1634".parse().unwrap();
/// let quic: libp2p::Multiaddr = "/ip4/1.1.1.1/udp/1634/quic-v1".parse().unwrap();
/// assert_eq!(transport_order(&tcp, &quic), std::cmp::Ordering::Less);
/// ```
pub fn transport_order(a: &Multiaddr, b: &Multiaddr) -> std::cmp::Ordering {
    advertise_rank(a).cmp(&advertise_rank(b))
}

/// The transport suites the local node's assembled libp2p stack can dial.
///
/// Mirrors the swarm assembly in `vertex-swarm-node`: the native stack is
/// TCP with DNS resolution plus a QUIC v1 dialer and no websocket client;
/// the browser stack is `libp2p-websocket-websys`, which dials secure
/// websockets only (both the `/dns4/<host>/../tls/ws` and the AutoTLS
/// `/ip4/../tls/sni/<host>/ws` shapes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCapability {
    /// TCP with DNS resolution; no websocket client.
    Tcp,
    /// TCP with DNS resolution plus a QUIC v1 dialer; no websocket client.
    TcpQuic,
    /// Secure websockets only.
    SecureWebsocket,
}

impl TransportCapability {
    /// The capability matching the swarm this build target assembles.
    #[cfg(not(target_arch = "wasm32"))]
    pub const fn platform() -> Self {
        Self::TcpQuic
    }

    /// The capability matching the swarm this build target assembles.
    #[cfg(target_arch = "wasm32")]
    pub const fn platform() -> Self {
        Self::SecureWebsocket
    }

    /// Whether this stack can dial `addr` at the transport layer.
    ///
    /// Covers only the statically assembled suites (TCP, QUIC, or secure
    /// websockets). A `/memory/<port>` address is never dialable through the
    /// platform stack; memory admission is a property of the combined
    /// [`DialCapability::allow_memory`] bit, set only when an in-process memory
    /// transport is injected. A `/p2p-circuit` address is likewise never
    /// dialable here: circuit dialability is a property of an injected relay
    /// client transport, not of the static suite.
    pub fn can_dial(&self, addr: &Multiaddr) -> bool {
        matches!(
            (self, TransportRequirement::of(addr)),
            (Self::Tcp, TransportRequirement::Tcp)
                | (
                    Self::TcpQuic,
                    TransportRequirement::Tcp | TransportRequirement::Quic
                )
                | (Self::SecureWebsocket, TransportRequirement::SecureWebsocket)
        )
    }
}

/// Combined dial eligibility: IP-family reachability and transport support.
///
/// This is the one filter dial preparation and gossip intake share, so the
/// set of peers admitted to the known table and the set of addresses handed
/// to the dialer can never disagree about what is dialable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialCapability {
    /// IP-family reachability (listen-derived, or pinned for dial-only
    /// nodes).
    pub ip: crate::IpCapability,
    /// Transport suites the assembled stack can dial.
    pub transport: TransportCapability,
    /// Admit `/memory/<port>` multiaddrs. Off by default: the platform TCP and
    /// websocket stacks carry no memory transport, so a gossiped memory-only
    /// record dies at intake. Set only when an in-process memory transport is
    /// injected (the integration harness), so its channel addresses dial.
    pub allow_memory: bool,
}

impl DialCapability {
    /// Whether `addr` is dialable: the transport half supports it and
    /// [`crate::is_dialable`] passes for the IP half.
    pub fn can_dial(&self, addr: &Multiaddr) -> bool {
        self.transport_can_dial(addr) && crate::is_dialable(addr, self.ip)
    }

    /// Whether the assembled transport stack can dial `addr`, ignoring the IP
    /// half. Memory addresses are admitted only when [`Self::allow_memory`] is
    /// set. Callers that must apply the transport filter before the IP
    /// capability is known use this directly.
    pub fn transport_can_dial(&self, addr: &Multiaddr) -> bool {
        if matches!(TransportRequirement::of(addr), TransportRequirement::Memory) {
            return self.allow_memory;
        }
        self.transport.can_dial(addr)
    }

    /// Whether at least one of `addrs` is dialable.
    pub fn can_dial_any<'a>(&self, addrs: impl IntoIterator<Item = &'a Multiaddr>) -> bool {
        addrs.into_iter().any(|addr| self.can_dial(addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IpCapability;

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid multiaddr")
    }

    #[test]
    fn classifies_live_network_shapes() {
        // The shapes that appear in mainnet hive gossip and dnsaddr leaves.
        let cases = [
            ("/ip4/1.2.3.4/tcp/1634", TransportRequirement::Tcp),
            ("/dns4/bee.example.org/tcp/1634", TransportRequirement::Tcp),
            ("/ip6/2001:db8::1/tcp/1634", TransportRequirement::Tcp),
            (
                "/dns4/host.example.org/tcp/443/tls/ws",
                TransportRequirement::SecureWebsocket,
            ),
            (
                "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws",
                TransportRequirement::SecureWebsocket,
            ),
            (
                "/dns4/host.example.org/tcp/443/wss",
                TransportRequirement::SecureWebsocket,
            ),
            ("/ip4/1.2.3.4/tcp/1634/ws", TransportRequirement::Websocket),
            ("/ip4/1.2.3.4/udp/1634/quic-v1", TransportRequirement::Quic),
            (
                "/ip6/2001:db8::1/udp/1634/quic-v1",
                TransportRequirement::Quic,
            ),
            ("/ip4/1.2.3.4/udp/1634/quic", TransportRequirement::Other),
            (
                "/ip4/1.2.3.4/udp/1634/quic-v1/webtransport",
                TransportRequirement::Other,
            ),
        ];
        for (s, expected) in cases {
            assert_eq!(TransportRequirement::of(&addr(s)), expected, "{s}");
        }
    }

    #[test]
    fn classification_survives_p2p_suffix() {
        let wss = addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg",
        );
        assert_eq!(
            TransportRequirement::of(&wss),
            TransportRequirement::SecureWebsocket
        );
        let tcp = addr("/ip4/1.2.3.4/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg");
        assert_eq!(TransportRequirement::of(&tcp), TransportRequirement::Tcp);
        let quic = addr(
            "/ip4/1.2.3.4/udp/1634/quic-v1/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg",
        );
        assert_eq!(TransportRequirement::of(&quic), TransportRequirement::Quic);
    }

    #[test]
    fn tcp_stack_rejects_websockets_and_quic() {
        let cap = TransportCapability::Tcp;
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(cap.can_dial(&addr("/dns4/bee.example.org/tcp/1634")));
        assert!(!cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634/ws")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic-v1")));
    }

    #[test]
    fn tcp_quic_stack_dials_both_but_rejects_websockets() {
        let cap = TransportCapability::TcpQuic;
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(cap.can_dial(&addr("/dns4/bee.example.org/tcp/1634")));
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic-v1")));
        assert!(cap.can_dial(&addr("/ip6/2001:db8::1/udp/1634/quic-v1")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic-v1/webtransport")));
        assert!(!cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634/ws")));
    }

    #[test]
    fn secure_websocket_stack_rejects_tcp_plain_ws_and_quic() {
        let cap = TransportCapability::SecureWebsocket;
        assert!(cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
        assert!(cap.can_dial(&addr("/dns4/host.example.org/tcp/443/tls/ws")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634/ws")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic-v1")));
    }

    #[test]
    fn memory_classification_survives_p2p_suffix() {
        let mem = addr("/memory/1234");
        assert_eq!(TransportRequirement::of(&mem), TransportRequirement::Memory);
        // The platform transport half never admits memory on its own.
        assert!(!TransportCapability::Tcp.can_dial(&mem));
        assert!(!TransportCapability::TcpQuic.can_dial(&mem));
        assert!(!TransportCapability::SecureWebsocket.can_dial(&mem));

        // A /p2p/ suffix does not change the classification.
        let with_peer = addr("/memory/1234/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg");
        assert_eq!(
            TransportRequirement::of(&with_peer),
            TransportRequirement::Memory
        );
    }

    #[test]
    fn relay_classification_dominates_the_relay_leg() {
        // The circuit component wins regardless of the suite the relay leg
        // rides, and a target /p2p/ suffix does not change it.
        let cases = [
            "/ip4/1.2.3.4/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg/p2p-circuit",
            "/ip4/1.2.3.4/udp/1634/quic-v1/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg/p2p-circuit",
            "/ip4/1.2.3.4/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg/p2p-circuit/p2p/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N",
            "/p2p-circuit",
        ];
        for s in cases {
            assert_eq!(
                TransportRequirement::of(&addr(s)),
                TransportRequirement::Relay,
                "{s}"
            );
        }
    }

    #[test]
    fn no_platform_stack_dials_a_relayed_address() {
        let relayed = addr(
            "/ip4/1.2.3.4/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg/p2p-circuit",
        );
        assert!(!TransportCapability::Tcp.can_dial(&relayed));
        assert!(!TransportCapability::TcpQuic.can_dial(&relayed));
        assert!(!TransportCapability::SecureWebsocket.can_dial(&relayed));
    }

    #[test]
    fn relayed_address_ranks_last_for_advertisement() {
        let tcp = addr("/ip4/8.8.8.8/tcp/1634");
        let relayed = addr(
            "/ip4/1.2.3.4/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg/p2p-circuit",
        );
        assert_eq!(transport_order(&tcp, &relayed), std::cmp::Ordering::Less);
    }

    #[test]
    fn default_capability_rejects_memory() {
        // Without the memory bit a production node drops a memory address, even
        // one carrying no IP, so a gossiped memory-only record dies at intake.
        let cap = DialCapability {
            ip: IpCapability::None,
            transport: TransportCapability::Tcp,
            allow_memory: false,
        };
        assert!(!cap.can_dial(&addr("/memory/1234")));
    }

    #[test]
    fn memory_bit_admits_memory_regardless_of_ip() {
        // With the bit set an injected memory transport dials the channel
        // address; a memory address carries no IP to reach, so the IP half
        // never blocks it.
        let cap = DialCapability {
            ip: IpCapability::None,
            transport: TransportCapability::Tcp,
            allow_memory: true,
        };
        let mem = addr("/memory/1234");
        assert!(cap.can_dial(&mem));
        assert!(cap.transport_can_dial(&mem));

        // The bit does not widen the platform transport suite: a TCP stack
        // still rejects a websocket-only address.
        assert!(!cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
    }

    #[test]
    fn dial_capability_combines_ip_and_transport() {
        // A browser-shaped capability: dual-stack IP, wss-only transport.
        let browser = DialCapability {
            ip: IpCapability::Dual,
            transport: TransportCapability::SecureWebsocket,
            allow_memory: false,
        };
        let wss = addr("/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws");
        let tcp = addr("/ip4/8.8.8.8/tcp/1634");
        assert!(browser.can_dial(&wss));
        assert!(!browser.can_dial(&tcp));
        assert!(browser.can_dial_any([&tcp, &wss]));
        assert!(!browser.can_dial_any([&tcp]));

        // A v4-only native node rejects a v6 TCP address on the IP half.
        let native_v4 = DialCapability {
            ip: IpCapability::V4Only,
            transport: TransportCapability::Tcp,
            allow_memory: false,
        };
        assert!(native_v4.can_dial(&tcp));
        assert!(!native_v4.can_dial(&addr("/ip6/2001:db8::1/tcp/1634")));
        assert!(!native_v4.can_dial(&wss));
    }

    #[test]
    fn dial_capability_unknown_ip_rejects_everything() {
        let cap = DialCapability {
            ip: IpCapability::None,
            transport: TransportCapability::Tcp,
            allow_memory: false,
        };
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
    }

    #[test]
    fn transport_order_ranks_tcp_quic_wss_then_other() {
        use std::cmp::Ordering;

        let tcp = addr("/ip4/8.8.8.8/tcp/1634");
        let quic = addr("/ip4/1.1.1.1/udp/1634/quic-v1");
        let wss = addr("/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws");
        let other = addr("/ip4/8.8.8.8/udp/1634/quic-v1/webtransport");

        assert_eq!(transport_order(&tcp, &quic), Ordering::Less);
        assert_eq!(transport_order(&quic, &wss), Ordering::Less);
        assert_eq!(transport_order(&wss, &other), Ordering::Less);
        assert_eq!(transport_order(&quic, &tcp), Ordering::Greater);

        // Same suite compares Equal: the rank is a partial key, callers add
        // their own tie-breaks.
        assert_eq!(
            transport_order(&quic, &addr("/ip6/2001:db8::1/udp/1634/quic-v1")),
            Ordering::Equal
        );
    }

    // Pin the native platform seam so a regression back to a TCP-only
    // capability (which would silently drop QUIC dials) fails here rather
    // than only surfacing on the live network.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_platform_dials_tcp_and_quic() {
        let cap = TransportCapability::platform();
        assert_eq!(cap, TransportCapability::TcpQuic);
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/udp/1634/quic-v1")));
    }
}
