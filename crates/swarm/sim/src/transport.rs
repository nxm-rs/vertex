//! libp2p transport over turmoil's simulated TCP.
//!
//! Every returned future touches the sim's thread-local world when polled, so
//! the transport only works inside a turmoil host; polling it outside a
//! simulation panics in turmoil itself.

use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::future::BoxFuture;
use libp2p::{
    Multiaddr,
    core::{
        multiaddr::Protocol,
        transport::{DialOpts, ListenerId, Transport, TransportError, TransportEvent},
    },
};
use turmoil::net::{TcpListener, TcpStream};

/// Raw stream transport over turmoil TCP, supporting `/ip4|ip6/../tcp/..`
/// multiaddrs. Compose it with an authentication upgrade and a muxer before
/// handing it to a swarm.
#[derive(Default)]
pub struct TurmoilTransport {
    listeners: Vec<Listener>,
}

struct Listener {
    id: ListenerId,
    state: ListenerState,
}

enum ListenerState {
    /// Bind in flight. turmoil resolves the bind on first poll; it is a
    /// future only because it must run inside a host's runtime.
    Binding(BoxFuture<'static, io::Result<TcpListener>>),
    /// Bound and accepting. The accept future owns an [`Arc`] clone of the
    /// listener and is re-armed after every inbound connection.
    Accepting {
        listener: Arc<TcpListener>,
        accept: BoxFuture<'static, io::Result<(TcpStream, SocketAddr)>>,
    },
    /// Bind failed and the error was reported; the listener is inert.
    Closed,
}

fn arm_accept(
    listener: &Arc<TcpListener>,
) -> BoxFuture<'static, io::Result<(TcpStream, SocketAddr)>> {
    let listener = Arc::clone(listener);
    Box::pin(async move { listener.accept().await })
}

fn multiaddr_to_socketaddr(addr: &Multiaddr) -> Option<SocketAddr> {
    let mut iter = addr.iter();
    let ip = match iter.next()? {
        Protocol::Ip4(ip) => std::net::IpAddr::V4(ip),
        Protocol::Ip6(ip) => std::net::IpAddr::V6(ip),
        _ => return None,
    };
    match (iter.next()?, iter.next()) {
        (Protocol::Tcp(port), None) => Some(SocketAddr::new(ip, port)),
        _ => None,
    }
}

fn socketaddr_to_multiaddr(addr: SocketAddr) -> Multiaddr {
    Multiaddr::empty()
        .with(addr.ip().into())
        .with(Protocol::Tcp(addr.port()))
}

impl Transport for TurmoilTransport {
    type Output = TurmoilStream;
    type Error = io::Error;
    type ListenerUpgrade = futures::future::Ready<io::Result<TurmoilStream>>;
    type Dial = BoxFuture<'static, io::Result<TurmoilStream>>;

    fn listen_on(
        &mut self,
        id: ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        let socket_addr =
            multiaddr_to_socketaddr(&addr).ok_or(TransportError::MultiaddrNotSupported(addr))?;
        self.listeners.push(Listener {
            id,
            state: ListenerState::Binding(Box::pin(TcpListener::bind(socket_addr))),
        });
        Ok(())
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        let before = self.listeners.len();
        self.listeners.retain(|l| l.id != id);
        before != self.listeners.len()
    }

    fn dial(
        &mut self,
        addr: Multiaddr,
        _opts: DialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        let socket_addr =
            multiaddr_to_socketaddr(&addr).ok_or(TransportError::MultiaddrNotSupported(addr))?;
        Ok(Box::pin(async move {
            let stream = TcpStream::connect(socket_addr).await?;
            Ok(TurmoilStream { inner: stream })
        }))
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let this = self.get_mut();
        for listener in &mut this.listeners {
            match &mut listener.state {
                ListenerState::Binding(bind) => match bind.as_mut().poll(cx) {
                    Poll::Ready(Ok(bound)) => {
                        let listen_addr = bound
                            .local_addr()
                            .map(socketaddr_to_multiaddr)
                            .unwrap_or_else(|_| Multiaddr::empty());
                        let bound = Arc::new(bound);
                        let accept = arm_accept(&bound);
                        listener.state = ListenerState::Accepting {
                            listener: bound,
                            accept,
                        };
                        return Poll::Ready(TransportEvent::NewAddress {
                            listener_id: listener.id,
                            listen_addr,
                        });
                    }
                    Poll::Ready(Err(error)) => {
                        listener.state = ListenerState::Closed;
                        return Poll::Ready(TransportEvent::ListenerError {
                            listener_id: listener.id,
                            error,
                        });
                    }
                    Poll::Pending => {}
                },
                ListenerState::Accepting {
                    listener: bound,
                    accept,
                } => match accept.as_mut().poll(cx) {
                    Poll::Ready(Ok((stream, remote))) => {
                        let local_addr = bound
                            .local_addr()
                            .map(socketaddr_to_multiaddr)
                            .unwrap_or_else(|_| Multiaddr::empty());
                        let listener_id = listener.id;
                        *accept = arm_accept(bound);
                        return Poll::Ready(TransportEvent::Incoming {
                            listener_id,
                            upgrade: futures::future::ready(Ok(TurmoilStream { inner: stream })),
                            local_addr,
                            send_back_addr: socketaddr_to_multiaddr(remote),
                        });
                    }
                    Poll::Ready(Err(error)) => {
                        let listener_id = listener.id;
                        *accept = arm_accept(bound);
                        return Poll::Ready(TransportEvent::ListenerError { listener_id, error });
                    }
                    Poll::Pending => {}
                },
                ListenerState::Closed => {}
            }
        }
        Poll::Pending
    }
}

/// A turmoil TCP stream exposed through the futures I/O traits libp2p
/// upgrades expect.
pub struct TurmoilStream {
    inner: TcpStream,
}

impl futures::AsyncRead for TurmoilStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buf = tokio::io::ReadBuf::new(buf);
        futures::ready!(tokio::io::AsyncRead::poll_read(
            Pin::new(&mut self.inner),
            cx,
            &mut read_buf
        ))?;
        Poll::Ready(Ok(read_buf.filled().len()))
    }
}

impl futures::AsyncWrite for TurmoilStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.inner), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.inner), cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.inner), cx)
    }
}
