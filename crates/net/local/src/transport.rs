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
    /// Anything else (QUIC, memory, relay-only); dialable by no transport
    /// stack vertex currently assembles.
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

        for proto in addr.iter() {
            match proto {
                Protocol::Tcp(_) => saw_tcp = true,
                Protocol::Tls => saw_tls = true,
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

        if saw_tcp { Self::Tcp } else { Self::Other }
    }
}

/// The transport suites the local node's assembled libp2p stack can dial.
///
/// Mirrors the swarm assembly in `vertex-swarm-node`: the native stack is
/// TCP with DNS resolution and no websocket client; the browser stack is
/// `libp2p-websocket-websys`, which dials secure websockets only (both the
/// `/dns4/<host>/../tls/ws` and the AutoTLS `/ip4/../tls/sni/<host>/ws`
/// shapes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCapability {
    /// TCP with DNS resolution; no websocket client.
    Tcp,
    /// Secure websockets only.
    SecureWebsocket,
}

impl TransportCapability {
    /// The capability matching the swarm this build target assembles.
    #[cfg(not(target_arch = "wasm32"))]
    pub const fn platform() -> Self {
        Self::Tcp
    }

    /// The capability matching the swarm this build target assembles.
    #[cfg(target_arch = "wasm32")]
    pub const fn platform() -> Self {
        Self::SecureWebsocket
    }

    /// Whether this stack can dial `addr` at the transport layer.
    pub fn can_dial(&self, addr: &Multiaddr) -> bool {
        matches!(
            (self, TransportRequirement::of(addr)),
            (Self::Tcp, TransportRequirement::Tcp)
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
}

impl DialCapability {
    /// Whether `addr` is dialable: the transport stack supports it and
    /// [`crate::is_dialable`] passes for the IP half.
    pub fn can_dial(&self, addr: &Multiaddr) -> bool {
        self.transport.can_dial(addr) && crate::is_dialable(addr, self.ip)
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
            ("/ip4/1.2.3.4/udp/1634/quic-v1", TransportRequirement::Other),
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
    }

    #[test]
    fn tcp_stack_rejects_websockets() {
        let cap = TransportCapability::Tcp;
        assert!(cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(cap.can_dial(&addr("/dns4/bee.example.org/tcp/1634")));
        assert!(!cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634/ws")));
    }

    #[test]
    fn secure_websocket_stack_rejects_tcp_and_plain_ws() {
        let cap = TransportCapability::SecureWebsocket;
        assert!(cap.can_dial(&addr(
            "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws"
        )));
        assert!(cap.can_dial(&addr("/dns4/host.example.org/tcp/443/tls/ws")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634/ws")));
    }

    #[test]
    fn dial_capability_combines_ip_and_transport() {
        // A browser-shaped capability: dual-stack IP, wss-only transport.
        let browser = DialCapability {
            ip: IpCapability::Dual,
            transport: TransportCapability::SecureWebsocket,
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
        };
        assert!(!cap.can_dial(&addr("/ip4/8.8.8.8/tcp/1634")));
    }
}
