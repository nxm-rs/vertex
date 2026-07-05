//! World-owned event trace.
//!
//! Entries carry the host name, the virtual timestamp, and a normalized event
//! line, so two runs of the same seeded world compare with one `assert_eq!`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use libp2p::swarm::SwarmEvent;
use parking_lot::Mutex;

/// One recorded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    /// Host that recorded the entry.
    pub host: String,
    /// Virtual time at which it was recorded.
    pub elapsed: Duration,
    /// Normalized event line.
    pub line: String,
}

impl fmt::Display for TraceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}ms {}] {}",
            self.elapsed.as_millis(),
            self.host,
            self.line
        )
    }
}

/// Shared, chronologically ordered trace for one world.
///
/// Cheap to clone; every host appends through its [`HostContext`] and the
/// world exposes the merged result after the run.
///
/// [`HostContext`]: crate::HostContext
#[derive(Debug, Clone, Default)]
pub struct SimTrace {
    entries: Arc<Mutex<Vec<TraceEntry>>>,
}

impl SimTrace {
    pub(crate) fn record(&self, host: &str, line: String) {
        self.entries.lock().push(TraceEntry {
            host: host.to_owned(),
            elapsed: turmoil::sim_elapsed().unwrap_or_default(),
            line,
        });
    }

    /// Snapshot of the recorded entries.
    pub fn entries(&self) -> Vec<TraceEntry> {
        self.entries.lock().clone()
    }

    /// Snapshot of the recorded entries as formatted lines.
    pub fn lines(&self) -> Vec<String> {
        self.entries
            .lock()
            .iter()
            .map(TraceEntry::to_string)
            .collect()
    }
}

/// Normalize a swarm event into a trace line, delegating behaviour events to
/// `behaviour`.
///
/// `ConnectionId`s come from a process-global counter and are excluded;
/// everything kept (peer ids, addresses, behaviour summaries) is stable
/// across runs of one seed. Behaviour normalizers must uphold the same
/// exclusion.
pub fn normalized_event<E>(event: &SwarmEvent<E>, behaviour: impl FnOnce(&E) -> String) -> String {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => format!("listen {address}"),
        SwarmEvent::IncomingConnection { send_back_addr, .. } => {
            format!("incoming from {send_back_addr}")
        }
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => format!(
            "established peer={peer_id} addr={}",
            endpoint.get_remote_address()
        ),
        SwarmEvent::ConnectionClosed { peer_id, .. } => format!("closed peer={peer_id}"),
        SwarmEvent::Dialing { .. } => "dialing".to_string(),
        SwarmEvent::NewExternalAddrCandidate { address } => format!("addr-candidate {address}"),
        SwarmEvent::Behaviour(event) => behaviour(event),
        _ => "other".to_string(),
    }
}
