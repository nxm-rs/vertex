//! On-chain chequebook client for the SWAP settlement service.
//!
//! [`ChequebookContract`] is the native, chain-facing half of the chequebook: it
//! deploys a chequebook through the factory, cashes cheques, and reads balances.
//! It is a SWAP settlement detail, so it lives with the cheque codec rather than
//! in the generic chain crate, which knows nothing about chequebook semantics.
//!
//! The client holds a shared `alloy_provider::Provider` and assembles
//! `nectar_contracts` `SolCall` calldata directly: reads go through
//! `Provider::call` and decode the typed return, writes go through
//! `Provider::send_transaction`. There is no parallel reader or sender
//! abstraction over alloy. Fee bumping for stuck writes comes from
//! [`vertex_chain::ProviderExt`].
//!
//! This module is gated behind the `chain` feature so the rest of the crate stays
//! a pure, wasm-safe codec with no RPC stack. The factory address comes from
//! [`vertex_chain::ChainConfig`]; per-chequebook reads and writes target the
//! chequebook address supplied per call.

use core::time::Duration;

use alloy_network::Ethereum;
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{PendingTransactionBuilder, Provider};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_sol_types::SolCall;
use nectar_contracts::{IChequebook, IChequebookFactory};
use vertex_chain::{ChainConfig, ChainError, TxError, TxRequest};

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
    /// Execute a chequebook read `eth_call` and decode the typed return.
    async fn read_call<Call: SolCall>(
        &self,
        target: Address,
        call: Call,
    ) -> Result<Call::Return, ChainError> {
        let request = TransactionRequest {
            to: Some(target.into()),
            input: TransactionInput::new(Bytes::from(call.abi_encode())),
            ..Default::default()
        };
        let out = self.provider.call(request).await?;
        Ok(Call::abi_decode_returns(out.as_ref())?)
    }

    /// Submit a write to `target` and return the pending transaction.
    ///
    /// The [`TxRequest`] description labels the transaction in logs and metrics.
    /// The returned [`PendingTransactionBuilder`] lets the caller await a receipt
    /// or take the hash, and [`vertex_chain::ProviderExt`] replaces it if it
    /// stalls.
    async fn send_call<Call: SolCall>(
        &self,
        target: Address,
        call: Call,
        description: &'static str,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        let request = TxRequest::new(
            TransactionRequest {
                to: Some(target.into()),
                input: TransactionInput::new(Bytes::from(call.abi_encode())),
                ..Default::default()
            },
            description,
        );
        tracing::debug!(tx = request.description, %target, "sending chequebook transaction");
        Ok(self.provider.send_transaction(request.request).await?)
    }

    /// Read the chequebook's total balance.
    pub async fn balance(&self, chequebook: Address) -> Result<U256, ChainError> {
        self.read_call(chequebook, IChequebook::balanceCall {})
            .await
    }

    /// Read the liquid balance available to a beneficiary.
    pub async fn liquid_balance_for(
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

    /// Read the cumulative amount paid out to a beneficiary.
    pub async fn paid_out(
        &self,
        chequebook: Address,
        beneficiary: Address,
    ) -> Result<U256, ChainError> {
        self.read_call(chequebook, IChequebook::paidOutCall { beneficiary })
            .await
    }

    /// Deploy a new chequebook for `issuer` through the factory.
    pub async fn deploy(
        &self,
        issuer: Address,
        timeout: Duration,
        salt: B256,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        self.send_call(
            self.config.chequebook_factory,
            IChequebookFactory::deploySimpleSwapCall {
                issuer,
                defaultHardDepositTimeoutDuration: U256::from(timeout.as_secs()),
                salt,
            },
            "chequebook_deploy",
        )
        .await
    }

    /// Cash a cheque as its beneficiary, paying out to `recipient`.
    pub async fn cash_cheque_beneficiary(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        self.send_call(
            cheque.cheque.chequebook,
            IChequebook::cashChequeBeneficiaryCall {
                recipient,
                cumulativePayout: cheque.cheque.cumulativePayout,
                issuerSig: Bytes::copy_from_slice(cheque.signature.as_ref()),
            },
            "cash_cheque_beneficiary",
        )
        .await
    }

    /// Cash a cheque on behalf of its beneficiary, taking `payout` as the caller.
    pub async fn cash_cheque(
        &self,
        cheque: &SignedCheque,
        recipient: Address,
        payout: U256,
        issuer_sig: bytes::Bytes,
    ) -> Result<PendingTransactionBuilder<Ethereum>, TxError> {
        self.send_call(
            cheque.cheque.chequebook,
            IChequebook::cashChequeCall {
                beneficiary: cheque.cheque.beneficiary,
                recipient,
                cumulativePayout: cheque.cheque.cumulativePayout,
                beneficiarySig: Bytes::copy_from_slice(cheque.signature.as_ref()),
                callerPayout: payout,
                issuerSig: Bytes::copy_from_slice(issuer_sig.as_ref()),
            },
            "cash_cheque",
        )
        .await
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests")]
mod tests {
    //! These tests exercise call and transaction *encoding* only.
    //!
    //! There is no live chain in CI, so the calldata the bindings produce for
    //! `cashCheque`, `cashChequeBeneficiary`, and `deploySimpleSwap` is checked
    //! against `SolCall::abi_encode`, and a read return is exercised against
    //! `SolCall::abi_decode_returns`. A real on-chain cashout (sending a signed
    //! transaction to a node and confirming a receipt) is deliberately out of CI
    //! scope.

    use super::*;
    use crate::ChequeExt;
    use alloy_primitives::{address, b256};
    use alloy_sol_types::SolValue;

    fn cheque() -> SignedCheque {
        let c = crate::Cheque::new(
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
            issuerSig: Bytes::copy_from_slice(c.signature.as_ref()),
        };
        let encoded = call.abi_encode();

        assert_eq!(
            encoded.get(..4).unwrap(),
            IChequebook::cashChequeBeneficiaryCall::SELECTOR
        );
        let decoded = IChequebook::cashChequeBeneficiaryCall::abi_decode(&encoded).unwrap();
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
        let encoded = call.abi_encode();
        assert_eq!(
            encoded.get(..4).unwrap(),
            IChequebook::cashChequeCall::SELECTOR
        );

        let decoded = IChequebook::cashChequeCall::abi_decode(&encoded).unwrap();
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

    #[test]
    fn balance_return_decodes() {
        let expected = U256::from(123_456_789u64);
        let encoded = expected.abi_encode();
        let decoded = IChequebook::balanceCall::abi_decode_returns(&encoded).unwrap();
        assert_eq!(decoded, expected);
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
