//! Storer-side serving seam for the client service.
//!
//! A client node has no storage: it requests and pushes chunks but never serves
//! them. A storer node additionally answers inbound retrieval requests from its
//! local store and takes custody of pushed chunks, returning a signed
//! statement-of-custody receipt. The two roles share the same [`ClientService`]
//! event loop, so the storer-only capabilities are injected through this seam:
//! client nodes leave it `None`, storer nodes wire a [`LocalServing`].
//!
//! [`ClientService`]: crate::ClientService

use alloy_primitives::Signature;
use alloy_signer::SignerSync;
use nectar_primitives::{ChunkAddress, Nonce};
use tracing::warn;
use vertex_swarm_api::{StampedChunk, SwarmIdentity, SwarmLocalStore};
use vertex_swarm_primitives::{Bin, OverlayAddress, StorageRadius};

/// Storer-side serving capability consumed by the client service.
///
/// Object-safe so the service can hold it as `Arc<dyn SwarmServing>` without a
/// type parameter. The concrete [`LocalServing`] captures the node identity and
/// its local store.
pub trait SwarmServing: Send + Sync {
    /// Look up a stamped chunk to serve for an inbound retrieval request.
    ///
    /// Returns the stamped chunk on a hit, or `None` on a miss. Serving over
    /// the wire requires the stamp that authorized the chunk, so this resolves
    /// through the stamp-aware store path; a backend that does not persist
    /// stamps reports a miss.
    fn serve(&self, address: &ChunkAddress) -> Option<StampedChunk>;

    /// Whether this node is responsible for storing `address`.
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Take custody of a pushed chunk by storing it locally.
    ///
    /// Returns `true` if the chunk is now stored (or was already stored).
    fn store(&self, chunk: &StampedChunk) -> bool;

    /// Sign a statement-of-custody receipt for `address`.
    ///
    /// Returns the signature, the nonce, and the current storage radius for the
    /// receipt, or `None` if signing failed.
    fn sign_receipt(&self, address: &ChunkAddress) -> Option<ReceiptParts>;
}

/// The signed parts of a statement-of-custody receipt.
#[derive(Debug, Clone)]
pub struct ReceiptParts {
    /// Signature over the chunk address.
    pub signature: Signature,
    /// The node nonce echoed in the receipt.
    pub nonce: Nonce,
    /// The node's current storage radius.
    pub storage_radius: StorageRadius,
}

/// Concrete [`SwarmServing`] backed by a node identity and a local store.
///
/// Responsibility is computed from the node overlay and a storage radius. The
/// radius source is a documented conservative stub until the storer reserve
/// drives it (see [`Self::new`]).
pub struct LocalServing<I: SwarmIdentity> {
    identity: I,
    overlay: OverlayAddress,
    local_store: std::sync::Arc<dyn SwarmLocalStore>,
    storage_radius: StorageRadius,
}

impl<I: SwarmIdentity> LocalServing<I> {
    /// Build a serving seam from the node identity and its local store.
    ///
    /// `storage_radius` is the node's area-of-responsibility radius. Until the
    /// storer reserve publishes a live radius, callers pass a conservative
    /// fixed value (the reference treats the whole neighbourhood at the
    /// configured depth as responsible); the responsibility check is therefore
    /// a documented stub.
    ///
    /// TODO(#76): drive `storage_radius` from the storer reserve / `SwarmStorer`
    /// once that radius source is wired, instead of a fixed value.
    pub fn new(
        identity: I,
        local_store: std::sync::Arc<dyn SwarmLocalStore>,
        storage_radius: StorageRadius,
    ) -> Self {
        let overlay = OverlayAddress::from(*identity.overlay_address());
        Self {
            identity,
            overlay,
            local_store,
            storage_radius,
        }
    }
}

impl<I: SwarmIdentity> SwarmServing for LocalServing<I> {
    fn serve(&self, address: &ChunkAddress) -> Option<StampedChunk> {
        match self.local_store.retrieve_stamped(address) {
            Ok(stamped) => stamped,
            Err(e) => {
                warn!(%address, error = ?e, "Local store retrieve failed while serving");
                None
            }
        }
    }

    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        // A chunk is in our area of responsibility when its proximity order to
        // our overlay is at least the storage radius. This mirrors the
        // reference rule (chunk stored when proximity_order(node, chunk) >=
        // radius). The radius itself is currently a conservative stub; see
        // `LocalServing::new`.
        let proximity = self.overlay.proximity(address);
        Bin::from(proximity) >= self.storage_radius.bin()
    }

    fn store(&self, chunk: &StampedChunk) -> bool {
        match self.local_store.store_stamped(chunk) {
            Ok(()) => true,
            Err(e) => {
                warn!(address = %chunk.address(), error = ?e, "Local store rejected pushed chunk");
                false
            }
        }
    }

    fn sign_receipt(&self, address: &ChunkAddress) -> Option<ReceiptParts> {
        // Wire-compat: the reference storer signs the raw 32-byte chunk address
        // with the node signing key under the EIP-191 personal-message prefix,
        // and echoes the node nonce and storage radius in the receipt. A
        // requester recovers the signer from the address bytes, so the signed
        // payload must be exactly the address. `sign_message_sync` applies the
        // EIP-191 prefix, matching the reference recovery path.
        let signer = self.identity.signer();
        let signature = match signer.sign_message_sync(address.as_slice()) {
            Ok(sig) => sig,
            Err(e) => {
                warn!(%address, error = ?e, "Failed to sign custody receipt");
                return None;
            }
        };
        Some(ReceiptParts {
            signature,
            nonce: self.identity.nonce(),
            storage_radius: self.storage_radius,
        })
    }
}
