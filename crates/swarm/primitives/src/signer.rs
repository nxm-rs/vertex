//! The node identity's overlay-derivation facet over alloy's signing trait.

use alloy_primitives::Address;
use nectar_primitives::{SwarmAddress, compute_overlay};

use crate::{NetworkId, Nonce};

pub use alloy_signer::{Signer, SignerSync};

/// A node's overlay-bearing identity: a sync signer (alloy [`SignerSync`], which
/// covers EIP-191 via `sign_message_sync` and EIP-712 via `sign_hash_sync` over a
/// typed-data hash) plus the overlay-derivation inputs.
///
/// `network_id` and `nonce` are identity properties, not signing inputs: the
/// nonce binds the overlay and is never part of any signed message. `address` is
/// added because [`SignerSync`] omits it (it lives on the async [`Signer`]).
/// Object-safe, so a non-generic `NetworkBehaviour` stores an
/// `Arc<dyn OverlaySigner>`.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait OverlaySigner: SignerSync {
    /// The signer's Ethereum address, one of the three overlay-derivation inputs.
    fn address(&self) -> Address;

    fn network_id(&self) -> NetworkId;

    /// The identity nonce. Binds the overlay; not part of any signed message.
    fn nonce(&self) -> Nonce;

    /// The overlay this identity derives: `compute_overlay(address, network_id, nonce)`.
    fn overlay(&self) -> SwarmAddress {
        compute_overlay(&self.address(), self.network_id(), &self.nonce())
    }
}
