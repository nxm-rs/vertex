//! `StorerBehaviour`: the storer protocol tier, a derived composite of the
//! client behaviour plus pullsync.

use libp2p::swarm::NetworkBehaviour;
use vertex_swarm_client_behaviour::ClientBehaviour;
use vertex_swarm_client_protocol::ClientEvent;

use crate::behaviour::{PullsyncBehaviour, PullsyncEvent};

/// Storer protocol tier: the client behaviour plus pullsync, multiplexed by the
/// libp2p derive into one connection handler. Unlike the client tier, which is a
/// single hand-rolled multiplexer, this composite is assembled from sibling
/// sub-behaviours.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "StorerBehaviourEvent")]
pub struct StorerBehaviour {
    pub client: ClientBehaviour,
    pub pullsync: PullsyncBehaviour,
}

/// Combined `to_swarm` event of [`StorerBehaviour`].
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum StorerBehaviourEvent {
    Client(ClientEvent),
    Pullsync(PullsyncEvent),
}

impl From<ClientEvent> for StorerBehaviourEvent {
    fn from(event: ClientEvent) -> Self {
        Self::Client(event)
    }
}

impl From<PullsyncEvent> for StorerBehaviourEvent {
    fn from(event: PullsyncEvent) -> Self {
        Self::Pullsync(event)
    }
}
