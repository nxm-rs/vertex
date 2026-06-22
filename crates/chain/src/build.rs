//! Shared chain provider construction.
//!
//! The chain is not a long-lived service of its own: it is a shared
//! `alloy_provider::Provider` that chain-using consumers (the SWAP settlement
//! service, the storer redistribution agent, a future staking flow) borrow.
//! [`build_chain_provider`] is the single seam that constructs that provider from
//! a transport URL and the node's signer and validates it against the expected
//! chain, so a launch path never names `alloy-provider` or `alloy-signer-local`
//! directly.
//!
//! The constructor is transport-portable: the `alloy-provider` transport is
//! target-split in this crate's manifest (native HTTP over reqwest with system
//! TLS, browser fetch on `wasm32`), so the same body builds a
//! [`SharedChainProvider`] on a native node and in the wasm client.

use alloy_chains::NamedChain;
use alloy_provider::{DynProvider, Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use tracing::info;

use crate::ChainConfig;

/// A failure constructing or validating the shared chain provider.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderBuildError {
    /// The transport could not connect to the configured RPC endpoint.
    #[error("chain connection failed: {0}")]
    Connect(String),

    /// The chain-id query against the connected endpoint failed.
    #[error("chain id query failed: {0}")]
    ChainId(String),

    /// The endpoint reported a different chain than the address book expects.
    #[error("RPC endpoint reports chain id {connected}, expected {expected} ({name})")]
    WrongChain {
        /// Chain id the endpoint reported.
        connected: u64,
        /// Chain id the address book expects.
        expected: u64,
        /// Human-readable name for `expected`, or `unknown`.
        name: String,
    },
}

/// A cloneable, type-erased handle to the node's shared chain provider.
///
/// Wraps a [`DynProvider`] (itself an `Arc` internally) so every chain consumer
/// holds the same connection without naming the concrete filler stack. Consumers
/// build their clients (for example a chequebook contract) over a clone of the
/// inner provider; the [`ChainConfig`] supplies the contract addresses.
#[derive(Clone)]
pub struct SharedChainProvider {
    provider: DynProvider,
    addresses: ChainConfig,
}

impl SharedChainProvider {
    /// The shared alloy provider. Clone it for a chain client.
    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    /// The network contract address book for this chain.
    pub fn addresses(&self) -> &ChainConfig {
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
    addresses: ChainConfig,
) -> Result<SharedChainProvider, ProviderBuildError> {
    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(rpc_url)
        .await
        .map_err(|e| ProviderBuildError::Connect(e.to_string()))?;

    // Fail fast if the endpoint is not the expected network.
    let connected = provider
        .get_chain_id()
        .await
        .map_err(|e| ProviderBuildError::ChainId(e.to_string()))?;
    let expected = addresses.chain.id();
    if connected != expected {
        return Err(ProviderBuildError::WrongChain {
            connected,
            expected,
            name: NamedChain::try_from(connected)
                .map(|c| c.to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
        });
    }

    info!(chain_id = expected, "Chain access enabled");

    Ok(SharedChainProvider {
        provider: provider.erased(),
        addresses,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;

    /// An unparseable connection string is reported as a build error rather than
    /// panicking, and never touches the network. This exercises the construction
    /// seam offline; a live chain-id validation is out of CI scope.
    #[tokio::test]
    async fn invalid_rpc_url_is_a_build_error() {
        let signer = PrivateKeySigner::random();
        let err = build_chain_provider("not a url", signer, ChainConfig::testnet())
            .await
            .expect_err("an unparseable RPC URL must fail");
        assert!(
            matches!(err, ProviderBuildError::Connect(_)),
            "expected a connect error, got {err:?}"
        );
    }
}

/// Links and constructs the alloy provider builder on wasm without a live RPC.
///
/// `build_chain_provider` itself needs an awaited connection, which a headless
/// wasm test cannot drive, so this asserts the transport-construction seam
/// (`ProviderBuilder::new().connect_http(url)`) links and builds a provider on
/// `wasm32` without panicking. It proves the browser-fetch transport features
/// resolve; the live chain-id validation is exercised natively.
#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use alloy_provider::ProviderBuilder;
    use alloy_signer_local::PrivateKeySigner;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn provider_builder_links_on_wasm() {
        let signer = PrivateKeySigner::random();
        let url = "http://localhost:8545".parse().expect("a valid url");
        let _provider = ProviderBuilder::new().wallet(signer).connect_http(url);
    }
}
