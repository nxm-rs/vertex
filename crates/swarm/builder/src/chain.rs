//! Shared chain provider construction for chain-needing node types.
//!
//! The chain is not a long-lived service of its own: it is a shared
//! `alloy_provider::Provider` that chain-using consumers (the SWAP settlement
//! service, the storer redistribution agent, a future staking flow) borrow. This
//! module is the single seam that constructs that provider from the node's
//! configuration and validates it against the network spec, so the launch path
//! never names `alloy-provider` or `alloy-signer-local` directly and the default
//! node cone stays chain-free.
//!
//! [`build_chain_provider`] builds a wallet-filled native HTTP provider over the
//! configured RPC URL, signed by the node's Ethereum identity, fails fast if the
//! endpoint is not the expected chain, and returns a cloneable, type-erased
//! [`SharedChainProvider`] handle. There is no background task to spawn: the
//! provider is a handle, and the build that needs no chain simply holds none.

use alloy_chains::NamedChain;
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use tracing::info;
use vertex_chain::ChainConfig as ChainAddressBook;

use crate::error::SwarmNodeError;

/// A cloneable, type-erased handle to the node's shared chain provider.
///
/// Wraps a [`DynProvider`] (itself an `Arc` internally) so every chain consumer
/// holds the same connection without naming the concrete filler stack. Consumers
/// build their clients (for example `ChequebookContract`) over a clone of the
/// inner provider; the [`ChainAddressBook`] supplies the contract addresses.
#[derive(Clone)]
pub struct SharedChainProvider {
    provider: DynProvider,
    addresses: ChainAddressBook,
}

impl SharedChainProvider {
    /// The shared alloy provider. Clone it for a chain client.
    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    /// The network contract address book for this chain.
    pub fn addresses(&self) -> &ChainAddressBook {
        &self.addresses
    }
}

impl std::fmt::Debug for SharedChainProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedChainProvider")
            .field("chain", &self.addresses.chain)
            .finish_non_exhaustive()
    }
}

/// Build and validate the node's shared chain provider against a live endpoint.
///
/// Builds a wallet-filled alloy provider over `rpc_url` signed by `signer` (the
/// recommended gas, fee, chain-id, and nonce fillers plus the wallet filler so
/// writes are signed by the node identity), validates the connected chain id
/// against `addresses.chain` so an operator pointed at the wrong endpoint fails
/// fast at startup, and erases the provider into a cloneable
/// [`SharedChainProvider`].
///
/// Logs the chain id once the connection is validated. There is no service to
/// spawn: the returned handle is the chain, and consumers borrow it.
pub async fn build_chain_provider(
    rpc_url: &str,
    signer: PrivateKeySigner,
    addresses: ChainAddressBook,
) -> Result<SharedChainProvider, SwarmNodeError> {
    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(rpc_url)
        .await
        .map_err(|e| SwarmNodeError::Chain(format!("chain connection failed: {e}")))?;

    // Fail fast if the endpoint is not the expected network.
    let connected = provider
        .get_chain_id()
        .await
        .map_err(|e| SwarmNodeError::Chain(format!("chain id query failed: {e}")))?;
    let expected = addresses.chain.id();
    if connected != expected {
        return Err(SwarmNodeError::Chain(format!(
            "RPC endpoint reports chain id {connected}, expected {expected} ({})",
            NamedChain::try_from(connected)
                .map(|c| c.to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
        )));
    }

    info!(chain_id = expected, "Chain access enabled");

    Ok(SharedChainProvider {
        provider: provider.erased(),
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;

    /// An unparseable connection string is reported as a chain error rather than
    /// panicking, and never touches the network. This exercises the construction
    /// seam offline; a live chain-id validation is out of CI scope.
    #[tokio::test]
    async fn invalid_rpc_url_is_a_chain_error() {
        let signer = PrivateKeySigner::random();
        let err = build_chain_provider("not a url", signer, ChainAddressBook::testnet())
            .await
            .expect_err("an unparseable RPC URL must fail");
        assert!(
            matches!(err, SwarmNodeError::Chain(_)),
            "expected a chain error, got {err:?}"
        );
    }
}
