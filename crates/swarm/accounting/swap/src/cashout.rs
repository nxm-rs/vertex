//! On-chain cashout for received cheques.
//!
//! Cheque exchange (sign, send, validate, credit) is fully chain-free. Cashing a
//! received cheque is the only step that touches the chain, so it lives behind
//! the `swap-chequebook` feature in this module. The service holds an optional
//! [`Cashout`]; without it, a received cheque is still validated and credited,
//! only never redeemed on-chain.

use alloy_primitives::Address;
use alloy_provider::DynProvider;
use vertex_chain::{ChainConfig, TxError};
use vertex_swarm_accounting_chequebook::{ChequebookContract, SignedCheque};

/// On-chain redeemer for received cheques.
///
/// Wraps a [`ChequebookContract`] over the shared chain provider and the address
/// that cashed funds are paid out to (our payout recipient).
#[derive(Debug, Clone)]
pub struct Cashout {
    contract: ChequebookContract<DynProvider>,
    recipient: Address,
}

impl Cashout {
    /// Build a cashout client over the shared provider.
    pub fn new(provider: DynProvider, config: ChainConfig, recipient: Address) -> Self {
        Self {
            contract: ChequebookContract::new(provider, config),
            recipient,
        }
    }

    /// Cash a received cheque as its beneficiary, paying out to our recipient.
    ///
    /// Returns once the transaction has been submitted; confirmation is left to
    /// the caller, which holds the returned pending-transaction handle's chain.
    pub async fn cash(&self, cheque: &SignedCheque) -> Result<(), TxError> {
        let _pending = self
            .contract
            .cash_cheque_beneficiary(cheque, self.recipient)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    //! Exercises cashout call encoding only; there is no live chain in CI.

    use alloy_contract::CallBuilder;
    use alloy_primitives::{Bytes, U256, address};
    use alloy_provider::{ProviderBuilder, mock::Asserter};
    use alloy_sol_types::SolCall;
    use nectar_contracts::IChequebook;
    use vertex_swarm_accounting_chequebook::{
        Bytes as ChequeBytes, Cheque, ChequeExt, SignedCheque,
    };

    /// The cashout call carries the cheque's cumulative payout, the issuer
    /// signature, and our recipient, targeting the cheque's own chequebook. Build
    /// the call the way [`Cashout::cash`] does and assert the calldata directly so
    /// the encoding is pinned without a live chain.
    #[test]
    fn cash_cheque_beneficiary_calldata() {
        let cheque = SignedCheque::new(
            Cheque::new(
                address!("1111111111111111111111111111111111111111"),
                address!("2222222222222222222222222222222222222222"),
                U256::from(1_000_000u64),
            ),
            ChequeBytes::from(vec![7u8; 65]),
        );
        let recipient = address!("3333333333333333333333333333333333333333");

        let call = IChequebook::cashChequeBeneficiaryCall {
            recipient,
            cumulativePayout: cheque.cheque.cumulativePayout,
            issuerSig: Bytes::copy_from_slice(cheque.signature.as_ref()),
        };

        let provider = ProviderBuilder::new().connect_mocked_client(Asserter::new());
        let calldata = CallBuilder::new_sol(&provider, &cheque.cheque.chequebook, &call)
            .calldata()
            .clone();

        assert_eq!(
            calldata.get(..4).unwrap(),
            IChequebook::cashChequeBeneficiaryCall::SELECTOR
        );
        let decoded = IChequebook::cashChequeBeneficiaryCall::abi_decode(&calldata).unwrap();
        assert_eq!(decoded.recipient, recipient);
        assert_eq!(decoded.cumulativePayout, U256::from(1_000_000u64));
        assert_eq!(decoded.issuerSig, call.issuerSig);
    }
}
