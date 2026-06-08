//! On-chain interaction layer for SWAP chequebooks.
//!
//! This module wires the off-chain [`SignedCheque`] type to the on-chain
//! ERC20SimpleSwap chequebook contract and its factory. It is gated behind the
//! crate `chain` feature so the default and wasm dependency cones never pull an
//! Ethereum RPC stack.
//!
//! The layer is split into a transport-agnostic [`ChequebookChain`] trait and a
//! provider-backed implementation, [`ProviderChequebookChain`], built over an
//! alloy [`Provider`] and the contract bindings from `nectar_contracts`.
//!
//! # Address sources
//!
//! Chequebook, factory and BZZ token addresses are network-specific and must be
//! supplied through [`ChainConfig`]. The deployments for the live networks live
//! in `nectar_contracts::{mainnet, testnet}`; this layer never hardcodes a
//! single network. [`ChainConfig::for_deployments`] derives a config from those
//! deployment constants for convenience.
//!
//! # Reads vs writes
//!
//! Balance queries ([`ChequebookChain::balance`], [`ChequebookChain::liquid_balance`],
//! [`ChequebookChain::liquid_balance_for`], [`ChequebookChain::paid_out`]) are
//! `eth_call` reads against a chequebook address. Cashing
//! ([`ChequebookChain::cash_cheque`], [`ChequebookChain::cash_cheque_beneficiary`])
//! and deployment ([`ChequebookChain::deploy_chequebook`]) submit transactions
//! signed by the provider's wallet.

use alloy_contract::CallBuilder;
use alloy_primitives::{Address, B256, U256};
use alloy_provider::Provider;
use nectar_contracts::{IChequebook, IChequebookFactory};

use crate::SignedCheque;

/// Errors that can occur during on-chain chequebook interaction.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ChainError {
    /// An `eth_call` read against the chequebook contract failed.
    #[error("contract call failed: {0}")]
    Call(String),

    /// Submitting or awaiting a transaction failed.
    #[error("transaction failed: {0}")]
    Transaction(String),

    /// The chequebook deployment transaction did not emit a deployed address.
    #[error("deployment did not report a chequebook address")]
    MissingDeployedAddress,
}

/// Network-specific addresses required to interact with chequebooks on a chain.
///
/// These addresses differ between networks (Gnosis Chain mainnet, Sepolia
/// testnet). Derive them from the `nectar_contracts` deployment constants with
/// [`ChainConfig::for_deployments`] or set them explicitly from operator config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainConfig {
    /// Chequebook factory (SimpleSwapFactory) contract address.
    pub factory: Address,
    /// BZZ token contract address backing chequebook balances.
    pub bzz_token: Address,
}

impl ChainConfig {
    /// Build a config from `nectar_contracts` deployment structs.
    ///
    /// ```
    /// use nectar_contracts::mainnet;
    /// use vertex_swarm_bandwidth_chequebook::chain::ChainConfig;
    ///
    /// let cfg = ChainConfig::for_deployments(mainnet::CHEQUEBOOK_FACTORY, mainnet::BZZ_TOKEN);
    /// assert_eq!(cfg.factory, mainnet::CHEQUEBOOK_FACTORY.address);
    /// assert_eq!(cfg.bzz_token, mainnet::BZZ_TOKEN.address);
    /// ```
    #[must_use]
    pub const fn for_deployments(
        factory: nectar_contracts::ChequebookFactory,
        bzz_token: nectar_contracts::Token,
    ) -> Self {
        Self {
            factory: factory.address,
            bzz_token: bzz_token.address,
        }
    }
}

/// On-chain chequebook operations.
///
/// Implementations talk to a single chain reachable over the configured RPC
/// transport. A chequebook address is passed per call so one backend can serve
/// reads against multiple counterparties' chequebooks, while writes are signed
/// by the backend's wallet.
#[expect(
    async_fn_in_trait,
    reason = "single in-crate provider impl; no Send bound needed across the FFI/gRPC boundary"
)]
pub trait ChequebookChain {
    /// Total BZZ balance held by the chequebook contract.
    async fn balance(&self, chequebook: Address) -> Result<U256, ChainError>;

    /// Liquid (uncommitted) balance available across all beneficiaries.
    async fn liquid_balance(&self, chequebook: Address) -> Result<U256, ChainError>;

    /// Liquid balance available to a specific beneficiary.
    async fn liquid_balance_for(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError>;

    /// Cumulative amount already paid out to a beneficiary.
    async fn paid_out(&self, chequebook: Address, beneficiary: Address)
    -> Result<U256, ChainError>;

    /// Deploy a new chequebook for `issuer` via the configured factory.
    ///
    /// Returns the transaction hash. The deployed address is reported through
    /// the factory's `SimpleSwapDeployed` event in the receipt; resolving it is
    /// left to the caller, which holds the receipt-handling policy.
    async fn deploy_chequebook(
        &self,
        issuer: Address,
        default_hard_deposit_timeout: U256,
        salt: B256,
    ) -> Result<B256, ChainError>;

    /// Cash a cheque on behalf of a beneficiary, paying `recipient`.
    ///
    /// `caller_payout` is the bounty the caller takes for submitting the
    /// transaction; `issuer_sig` authorises that bounty. The beneficiary
    /// signature is taken from the [`SignedCheque`].
    async fn cash_cheque(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
        caller_payout: U256,
        issuer_sig: &[u8],
    ) -> Result<B256, ChainError>;

    /// Cash a cheque as the beneficiary, paying `recipient`.
    ///
    /// Uses the issuer signature carried by the [`SignedCheque`].
    async fn cash_cheque_beneficiary(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
    ) -> Result<B256, ChainError>;
}

/// Provider-backed [`ChequebookChain`] implementation.
///
/// Wraps an alloy [`Provider`] (which carries the RPC transport and, for writes,
/// the wallet) and the [`ChainConfig`] addresses. Reads go through `eth_call`;
/// writes are signed and broadcast by the provider's wallet filler.
#[derive(Debug, Clone)]
pub struct ProviderChequebookChain<P> {
    provider: P,
    config: ChainConfig,
}

impl<P> ProviderChequebookChain<P> {
    /// Build a backend over a provider and network config.
    pub const fn new(provider: P, config: ChainConfig) -> Self {
        Self { provider, config }
    }

    /// The network addresses this backend is configured for.
    #[must_use]
    pub const fn config(&self) -> &ChainConfig {
        &self.config
    }

    /// The underlying provider.
    pub const fn provider(&self) -> &P {
        &self.provider
    }
}

impl<P: Provider> ProviderChequebookChain<P> {
    /// Execute a chequebook read call and map errors into [`ChainError::Call`].
    async fn read_call<C: alloy_sol_types::SolCall>(
        &self,
        chequebook: Address,
        call: C,
    ) -> Result<C::Return, ChainError> {
        CallBuilder::new_sol(&self.provider, &chequebook, &call)
            .call()
            .await
            .map_err(|e| ChainError::Call(e.to_string()))
    }
}

impl<P: Provider> ChequebookChain for ProviderChequebookChain<P> {
    async fn balance(&self, chequebook: Address) -> Result<U256, ChainError> {
        self.read_call(chequebook, IChequebook::balanceCall {})
            .await
    }

    async fn liquid_balance(&self, chequebook: Address) -> Result<U256, ChainError> {
        self.read_call(chequebook, IChequebook::liquidBalanceCall {})
            .await
    }

    async fn liquid_balance_for(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError> {
        self.read_call(
            chequebook,
            IChequebook::liquidBalanceForCall { beneficiary },
        )
        .await
    }

    async fn paid_out(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError> {
        self.read_call(chequebook, IChequebook::paidOutCall { beneficiary })
            .await
    }

    async fn deploy_chequebook(
        &self,
        issuer: Address,
        default_hard_deposit_timeout: U256,
        salt: B256,
    ) -> Result<B256, ChainError> {
        let call = IChequebookFactory::deploySimpleSwapCall {
            issuer,
            defaultHardDepositTimeoutDuration: default_hard_deposit_timeout,
            salt,
        };
        let pending = CallBuilder::new_sol(&self.provider, &self.config.factory, &call)
            .send()
            .await
            .map_err(|e| ChainError::Transaction(e.to_string()))?;
        Ok(*pending.tx_hash())
    }

    async fn cash_cheque(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
        caller_payout: U256,
        issuer_sig: &[u8],
    ) -> Result<B256, ChainError> {
        let call = IChequebook::cashChequeCall {
            beneficiary: cheque.cheque.beneficiary,
            recipient,
            cumulativePayout: cheque.cheque.cumulativePayout,
            beneficiarySig: alloy_primitives::Bytes(cheque.signature.clone()),
            callerPayout: caller_payout,
            issuerSig: alloy_primitives::Bytes::copy_from_slice(issuer_sig),
        };
        let pending = CallBuilder::new_sol(&self.provider, &cheque.cheque.chequebook, &call)
            .send()
            .await
            .map_err(|e| ChainError::Transaction(e.to_string()))?;
        Ok(*pending.tx_hash())
    }

    async fn cash_cheque_beneficiary(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
    ) -> Result<B256, ChainError> {
        let call = IChequebook::cashChequeBeneficiaryCall {
            recipient,
            cumulativePayout: cheque.cheque.cumulativePayout,
            issuerSig: alloy_primitives::Bytes(cheque.signature.clone()),
        };
        let pending = CallBuilder::new_sol(&self.provider, &cheque.cheque.chequebook, &call)
            .send()
            .await
            .map_err(|e| ChainError::Transaction(e.to_string()))?;
        Ok(*pending.tx_hash())
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    //! Tests here exercise call/transaction *encoding* only.
    //!
    //! There is no live chain in CI, so the calldata that the bindings produce
    //! for `cashCheque`, `cashChequeBeneficiary` and `deploySimpleSwap` is
    //! checked against `SolCall::abi_encode`, and reads are exercised against an
    //! alloy mock provider that asserts the request calldata and returns canned
    //! ABI-encoded values. A real on-chain cashout (sending a signed tx to a
    //! node and confirming a receipt) is deliberately out of scope for CI.

    use super::*;
    use alloy_primitives::{Bytes, address, b256};
    use alloy_provider::ProviderBuilder;
    use alloy_provider::mock::Asserter;
    use alloy_sol_types::{SolCall, SolValue};

    use crate::{Cheque, ChequeExt};

    fn cheque() -> SignedCheque {
        let c = Cheque::new(
            address!("1111111111111111111111111111111111111111"),
            address!("2222222222222222222222222222222222222222"),
            U256::from(1_000_000u64),
        );
        SignedCheque::new(c, bytes::Bytes::from(vec![7u8; 65]))
    }

    #[test]
    fn cash_cheque_beneficiary_calldata() {
        let c = cheque();
        let call = IChequebook::cashChequeBeneficiaryCall {
            recipient: address!("3333333333333333333333333333333333333333"),
            cumulativePayout: c.cheque.cumulativePayout,
            issuerSig: Bytes(c.signature.clone()),
        };
        let encoded = call.abi_encode();

        // Selector is the first four bytes of the ABI encoding.
        assert_eq!(
            encoded.get(..4).unwrap(),
            IChequebook::cashChequeBeneficiaryCall::SELECTOR
        );
        // Round-trips back to the same arguments.
        let decoded = IChequebook::cashChequeBeneficiaryCall::abi_decode(&encoded).unwrap();
        assert_eq!(decoded.recipient, call.recipient);
        assert_eq!(decoded.cumulativePayout, U256::from(1_000_000u64));
        assert_eq!(decoded.issuerSig, Bytes(c.signature.clone()));
    }

    #[test]
    fn cash_cheque_calldata() {
        let c = cheque();
        let issuer_sig = vec![9u8; 65];
        let call = IChequebook::cashChequeCall {
            beneficiary: c.cheque.beneficiary,
            recipient: address!("3333333333333333333333333333333333333333"),
            cumulativePayout: c.cheque.cumulativePayout,
            beneficiarySig: Bytes(c.signature.clone()),
            callerPayout: U256::from(42u64),
            issuerSig: Bytes::copy_from_slice(&issuer_sig),
        };
        let encoded = call.abi_encode();
        assert_eq!(
            encoded.get(..4).unwrap(),
            IChequebook::cashChequeCall::SELECTOR
        );

        let decoded = IChequebook::cashChequeCall::abi_decode(&encoded).unwrap();
        assert_eq!(decoded.beneficiary, c.cheque.beneficiary);
        assert_eq!(decoded.beneficiarySig, Bytes(c.signature.clone()));
        assert_eq!(decoded.callerPayout, U256::from(42u64));
        assert_eq!(decoded.issuerSig, Bytes::from(issuer_sig));
    }

    #[test]
    fn deploy_simple_swap_calldata() {
        let call = IChequebookFactory::deploySimpleSwapCall {
            issuer: address!("4444444444444444444444444444444444444444"),
            defaultHardDepositTimeoutDuration: U256::from(86_400u64),
            salt: b256!("00000000000000000000000000000000000000000000000000000000000000aa"),
        };
        let encoded = call.abi_encode();
        assert_eq!(
            encoded.get(..4).unwrap(),
            IChequebookFactory::deploySimpleSwapCall::SELECTOR
        );

        let decoded = IChequebookFactory::deploySimpleSwapCall::abi_decode(&encoded).unwrap();
        assert_eq!(
            decoded.defaultHardDepositTimeoutDuration,
            U256::from(86_400u64)
        );
    }

    #[tokio::test]
    async fn balance_read_decodes_mock_response() {
        // The mock provider returns a canned ABI-encoded u256 for eth_call.
        let asserter = Asserter::new();
        let expected = U256::from(123_456_789u64);
        asserter.push_success(&Bytes::from(expected.abi_encode()));

        let provider = ProviderBuilder::new().connect_mocked_client(asserter);
        let backend = ProviderChequebookChain::new(
            provider,
            ChainConfig::for_deployments(
                nectar_contracts::testnet::CHEQUEBOOK_FACTORY,
                nectar_contracts::testnet::BZZ_TOKEN,
            ),
        );

        let got = backend
            .balance(address!("5555555555555555555555555555555555555555"))
            .await
            .unwrap();
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn paid_out_read_decodes_mock_response() {
        let asserter = Asserter::new();
        let expected = U256::from(7u64);
        asserter.push_success(&Bytes::from(expected.abi_encode()));

        let provider = ProviderBuilder::new().connect_mocked_client(asserter);
        let backend = ProviderChequebookChain::new(
            provider,
            ChainConfig::for_deployments(
                nectar_contracts::mainnet::CHEQUEBOOK_FACTORY,
                nectar_contracts::mainnet::BZZ_TOKEN,
            ),
        );

        let got = backend
            .paid_out(
                address!("5555555555555555555555555555555555555555"),
                address!("2222222222222222222222222222222222222222"),
            )
            .await
            .unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn config_pulls_addresses_from_deployments() {
        let cfg = ChainConfig::for_deployments(
            nectar_contracts::mainnet::CHEQUEBOOK_FACTORY,
            nectar_contracts::mainnet::BZZ_TOKEN,
        );
        assert_eq!(
            cfg.factory,
            nectar_contracts::mainnet::CHEQUEBOOK_FACTORY.address
        );
        assert_eq!(cfg.bzz_token, nectar_contracts::mainnet::BZZ_TOKEN.address);
        assert_ne!(cfg.factory, Address::ZERO);
        // Addresses differ between networks; the layer never hardcodes one.
        let testnet = ChainConfig::for_deployments(
            nectar_contracts::testnet::CHEQUEBOOK_FACTORY,
            nectar_contracts::testnet::BZZ_TOKEN,
        );
        assert_ne!(cfg.factory, testnet.factory);
    }

    #[test]
    fn error_reason_labels_are_snake_case() {
        let label: &'static str = (&ChainError::MissingDeployedAddress).into();
        assert_eq!(label, "missing_deployed_address");
        let label: &'static str = (&ChainError::Call(String::new())).into();
        assert_eq!(label, "call");
    }
}
