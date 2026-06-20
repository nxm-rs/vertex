//! The storer ingest capability for the pushsync inbound path.
//!
//! A client-only node forwards every inbound pushsync delivery to a closer peer
//! and relays the storer's receipt verbatim — it never takes custody. A storer
//! node additionally holds this capability, which lets the inbound handler take
//! custody of a delivery it is responsible for: it puts the chunk into the
//! reserve and mints (signs) its own custody receipt rather than relaying.
//!
//! The capability is optional and cheaply cloned into each connection handler.
//! When it is absent the handler runs the unchanged client path, so installing
//! it never regresses a client.

use std::sync::Arc;

use alloy_signer::SignerSync;
use vertex_swarm_api::ReserveStore;
use vertex_swarm_net_pushsync::ReceiptSigner;
use vertex_swarm_primitives::{NetworkId, Nonce};

/// Storer-side ingest capability shared into each connection handler.
///
/// Bundles what a storer needs to take custody on an inbound pushsync delivery:
/// the [`ReserveStore`] (responsibility check, stamped `put`, and the declared
/// storage radius the receipt carries) and the node's receipt-minting identity.
/// The identity is the signing key plus the two overlay-derivation inputs
/// (`network_id` and `nonce`) that bind the storer overlay a forwarder recovers
/// from the minted receipt; this type implements [`ReceiptSigner`] so the
/// handler passes it straight to [`vertex_swarm_net_pushsync::Receipt::sign`]
/// without re-threading those inputs as loose parameters.
///
/// `put` admits the chunk unconditionally; the reserve is kept within capacity
/// out of band by the eviction primitives ([`ReserveStore::evict_from_bin`] /
/// [`evict_batch`](ReserveStore::evict_batch)), not by back-pressure on ingest.
///
/// Cloning is `Arc`-cheap; the behaviour clones one of these into every handler
/// at connection establishment.
#[derive(Clone)]
pub(crate) struct StorerCapability {
    /// The proximity-ordered, always-stamped reserve. The handler queries
    /// [`ReserveStore::is_responsible_for`] to decide whether to ingest, calls
    /// [`vertex_swarm_api::SwarmLocalStore::put`] to take custody, and reads
    /// [`ReserveStore::storage_radius`] for the receipt's declared radius.
    pub(crate) reserve: Arc<dyn ReserveStore>,
    /// The receipt signing key, erased to the synchronous signer trait so the
    /// handler is not generic over the identity's concrete signer.
    signer: Arc<dyn SignerSync + Send + Sync>,
    /// The network id, one of the two overlay-derivation inputs the receipt mint
    /// reads from the identity.
    network_id: NetworkId,
    /// The node's identity nonce, the other overlay-derivation input.
    nonce: Nonce,
}

impl StorerCapability {
    /// Bundle the reserve and the node's receipt-minting identity (signer,
    /// network id, and nonce) into a shareable capability.
    pub(crate) fn new(
        reserve: Arc<dyn ReserveStore>,
        signer: Arc<dyn SignerSync + Send + Sync>,
        network_id: NetworkId,
        nonce: Nonce,
    ) -> Self {
        Self {
            reserve,
            signer,
            network_id,
            nonce,
        }
    }
}

/// The capability carries the node's identity, so it is itself the
/// [`ReceiptSigner`] the inbound handler mints custody receipts through.
impl ReceiptSigner for StorerCapability {
    type Signer = dyn SignerSync + Send + Sync;

    fn signer(&self) -> &Self::Signer {
        &*self.signer
    }

    fn network_id(&self) -> NetworkId {
        self.network_id
    }

    fn nonce(&self) -> Nonce {
        self.nonce
    }
}

impl std::fmt::Debug for StorerCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The signer key must never be logged; the reserve has no Debug. Only the
        // public overlay-derivation inputs (network id and nonce) are shown.
        f.debug_struct("StorerCapability")
            .field("network_id", &self.network_id)
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}
