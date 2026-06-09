//! On-chain chequebook client for the SWAP settlement service.
//!
//! [`ChequebookContract`] is the native, chain-facing half of the chequebook: it
//! deploys a chequebook through the factory, cashes cheques, and reads balances.
//! It is a SWAP settlement detail, so it lives with the cheque codec rather than
//! in the generic chain crate, which knows nothing about chequebook semantics.
//!
//! The client holds a shared `alloy_provider::Provider` and drives the
//! `nectar_contracts` `sol!` interfaces through `alloy_contract` `CallBuilder`s:
//! reads end in `.call()`, writes in `.send()`. There is no parallel reader or
//! sender abstraction over alloy. Fee bumping for stuck writes comes from
//! [`vertex_chain::ProviderExt`], which operates on the
//! [`PendingTransactionBuilder`] a write returns.
//!
//! This module is gated behind the `swap-chequebook` feature so the rest of the
//! crate stays a pure, wasm-safe codec with no RPC stack. The factory address
//! comes from [`vertex_chain::ChainConfig`]; per-chequebook reads and writes
//! target the chequebook address supplied per call.

use core::time::Duration;

use alloy_contract::CallBuilder;
use alloy_network::Ethereum;
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{PendingTransactionBuilder, Provider};
use nectar_contracts::{IChequebook, IChequebookFactory};
use vertex_chain::{ChainConfig, ChainError, TxError};

use crate::SignedCheque;

/// On-chain chequebook client over a shared alloy [`Provider`].
///
/// Generic over the provider so a caller supplies a plain HTTP provider for
/// read-only access or a wallet-filled provider for writes. Reads target the
/// chequebook address passed per call; deploys target the factory from
/// [`ChainConfig`].
#[derive(Debug, Clone)]
pub struct ChequebookContract<P> {
    provider: P,
    config: ChainConfig,
}

impl<P> ChequebookContract<P> {
    /// Build the chequebook client over a provider and the network address book.
    pub const fn new(provider: P, config: ChainConfig) -> Self {
        Self { provider, config }
    }

    /// The network addresses this client is configured for.
    pub const fn config(&self) -> &ChainConfig {
        &self.config
    }

    /// The underlying provider.
    pub const fn provider(&self) -> &P {
        &self.provider
    }
}

impl<P: Provider> ChequebookContract<P> {
    /// Read the chequebook's total balance.
    pub async fn balance(&self, chequebook: Address) -> Result<U256, ChainError> {
        Ok(
            CallBuilder::new_sol(&self.provider, &chequebook, &IChequebook::balanceCall {})
                .call()
                .await?,
        )
    }

    /// Read the liquid balance available to a beneficiary.
    pub async fn liquid_balance_for(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError> {
        Ok(CallBuilder::new_sol(
            &self.provider,
            &chequebook,
            &IChequebook::liquidBalanceForCall { beneficiary },
        )
        .call()
        .await?)
    }

    /// Read the cumulative amount paid out to a beneficiary.
    pub async fn paid_out(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError> {
        Ok(CallBuilder::new_sol(
            &self.provider,
            &chequebook,
            &IChequebook::paidOutCall { beneficiary },
        )
        .call()
        .await?)
    }

    /// Deploy a new chequebook for `issuer` through the factory.
    pub async fn deploy(
        &self,
        issuer: Address,
        timeout: Duration,
        salt: B256,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let factory = self.config.chequebook_factory;
        let call = IChequebookFactory::deploySimpleSwapCall {
            issuer,
            defaultHardDepositTimeoutDuration: U256::from(timeout.as_secs()),
            salt,
        };
        tracing::debug!(tx = "chequebook_deploy", %factory, "sending chequebook transaction");
        Ok(CallBuilder::new_sol(&self.provider, &factory, &call)
            .send()
            .await?)
    }

    /// Cash a cheque as its beneficiary, paying out to `recipient`.
    pub async fn cash_cheque_beneficiary(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let target = cheque.cheque.chequebook;
        let call = IChequebook::cashChequeBeneficiaryCall {
            recipient,
            cumulativePayout: cheque.cheque.cumulativePayout,
            issuerSig: Bytes::copy_from_slice(cheque.signature.as_ref()),
        };
        tracing::debug!(tx = "cash_cheque_beneficiary", %target, "sending chequebook transaction");
        Ok(CallBuilder::new_sol(&self.provider, &target, &call)
            .send()
            .await?)
    }

    /// Cash a cheque on behalf of its beneficiary, taking `payout` as the caller.
    pub async fn cash_cheque(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
        payout: U256,
        issuer_sig: bytes::Bytes,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let target = cheque.cheque.chequebook;
        let call = IChequebook::cashChequeCall {
            beneficiary: cheque.cheque.beneficiary,
            recipient,
            cumulativePayout: cheque.cheque.cumulativePayout,
            beneficiarySig: Bytes::copy_from_slice(cheque.signature.as_ref()),
            callerPayout: payout,
            issuerSig: Bytes::copy_from_slice(issuer_sig.as_ref()),
        };
        tracing::debug!(tx = "cash_cheque", %target, "sending chequebook transaction");
        Ok(CallBuilder::new_sol(&self.provider, &target, &call)
            .send()
            .await?)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    //! These tests exercise call and transaction *encoding* only.
    //!
    //! There is no live chain in CI, so the calldata the `CallBuilder` produces
    //! for `cashCheque`, `cashChequeBeneficiary`, and `deploySimpleSwap` is
    //! checked against the `SolCall` selectors and arguments, a read is exercised
    //! against an alloy mock provider, and the factory deploy calldata the
    //! builder emits is asserted directly. A real on-chain cashout (sending a
    //! signed transaction to a node and confirming a receipt) is deliberately
    //! out of CI scope.

    use super::*;
    use crate::ChequeExt;
    use alloy_contract::CallBuilder;
    use alloy_primitives::{address, b256};
    use alloy_provider::{ProviderBuilder, mock::Asserter};
    use alloy_sol_types::{SolCall, SolValue};

    fn cheque() -> SignedCheque {
        let c = crate::Cheque::new(
            address!("1111111111111111111111111111111111111111"),
            address!("2222222222222222222222222222222222222222"),
            U256::from(1_000_000u64),
        );
        SignedCheque::new(c, bytes::Bytes::from(vec![7u8; 65]))
    }

    /// The mock provider has no wallet, so a `CallBuilder` cannot estimate gas or
    /// sign. The calldata is independent of all that: assert it directly on the
    /// builder before it ever reaches the transport.
    fn builder_calldata<C: SolCall>(call: &C) -> Bytes {
        let provider = ProviderBuilder::new().connect_mocked_client(Asserter::new());
        CallBuilder::new_sol(&provider, &Address::ZERO, call)
            .calldata()
            .clone()
    }

    #[test]
    fn cash_cheque_beneficiary_calldata() {
        let c = cheque();
        let call = IChequebook::cashChequeBeneficiaryCall {
            recipient: address!("3333333333333333333333333333333333333333"),
            cumulativePayout: c.cheque.cumulativePayout,
            issuerSig: Bytes::copy_from_slice(c.signature.as_ref()),
        };
        let calldata = builder_calldata(&call);

        assert_eq!(
            calldata.get(..4).unwrap(),
            IChequebook::cashChequeBeneficiaryCall::SELECTOR
        );
        let decoded = IChequebook::cashChequeBeneficiaryCall::abi_decode(&calldata).unwrap();
        assert_eq!(decoded.recipient, call.recipient);
        assert_eq!(decoded.cumulativePayout, U256::from(1_000_000u64));
        assert_eq!(decoded.issuerSig, call.issuerSig);
    }

    #[test]
    fn cash_cheque_calldata() {
        let c = cheque();
        let issuer_sig = vec![9u8; 65];
        let call = IChequebook::cashChequeCall {
            beneficiary: c.cheque.beneficiary,
            recipient: address!("3333333333333333333333333333333333333333"),
            cumulativePayout: c.cheque.cumulativePayout,
            beneficiarySig: Bytes::copy_from_slice(c.signature.as_ref()),
            callerPayout: U256::from(42u64),
            issuerSig: Bytes::copy_from_slice(&issuer_sig),
        };
        let calldata = builder_calldata(&call);
        assert_eq!(
            calldata.get(..4).unwrap(),
            IChequebook::cashChequeCall::SELECTOR
        );

        let decoded = IChequebook::cashChequeCall::abi_decode(&calldata).unwrap();
        assert_eq!(decoded.beneficiary, c.cheque.beneficiary);
        assert_eq!(decoded.beneficiarySig, call.beneficiarySig);
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
        let calldata = builder_calldata(&call);
        assert_eq!(
            calldata.get(..4).unwrap(),
            IChequebookFactory::deploySimpleSwapCall::SELECTOR
        );

        let decoded = IChequebookFactory::deploySimpleSwapCall::abi_decode(&calldata).unwrap();
        assert_eq!(
            decoded.defaultHardDepositTimeoutDuration,
            U256::from(86_400u64)
        );
    }

    #[tokio::test]
    async fn balance_read_decodes_mock_response() {
        // The mock provider returns a canned ABI-encoded u256 for eth_call; the
        // `CallBuilder` decodes it back to the typed return.
        let asserter = Asserter::new();
        let expected = U256::from(123_456_789u64);
        asserter.push_success(&Bytes::from(expected.abi_encode()));

        let provider = ProviderBuilder::new().connect_mocked_client(asserter);
        let client = ChequebookContract::new(provider, ChainConfig::mainnet());

        let got = client
            .balance(address!("5555555555555555555555555555555555555555"))
            .await
            .unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn config_pulls_addresses_from_deployments() {
        let mainnet = ChainConfig::mainnet();
        assert_eq!(
            mainnet.chequebook_factory,
            nectar_contracts::mainnet::CHEQUEBOOK_FACTORY.address
        );
        assert_ne!(mainnet.chequebook_factory, Address::ZERO);
        // Addresses differ between networks; the layer never hardcodes one.
        let testnet = ChainConfig::testnet();
        assert_ne!(mainnet.chequebook_factory, testnet.chequebook_factory);
    }
}
