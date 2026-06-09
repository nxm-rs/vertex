//! Unit tests for the chain trait surface.
//!
//! These exercise the pure pieces this crate owns: error label derivation, the
//! request and config constructors, the disabled implementations, and the
//! default `send_and_confirm` composition. There is no provider here, so there
//! is nothing to mock.

use core::time::Duration;

use alloy_primitives::{Address, B256, U256};
use bytes::Bytes;

use crate::{
    ChainConfig, ChainError, ChainReader, ChequebookChain, DisabledChain, ProviderError,
    TransactionSender, TxError, TxRequest, TxStatus,
};

#[test]
fn provider_error_labels_are_snake_case() {
    let s: &'static str = (&ProviderError::Disabled).into();
    assert_eq!(s, "disabled");
    let s: &'static str = (&ProviderError::Transport(String::new())).into();
    assert_eq!(s, "transport");
    let s: &'static str = (&ProviderError::NotFound(String::new())).into();
    assert_eq!(s, "not_found");
}

#[test]
fn tx_error_labels_are_snake_case() {
    let s: &'static str = (&TxError::GasEstimation(String::new())).into();
    assert_eq!(s, "gas_estimation");
    let s: &'static str = (&TxError::ConfirmationTimeout { hash: B256::ZERO }).into();
    assert_eq!(s, "confirmation_timeout");
}

#[test]
fn chain_error_labels_are_snake_case() {
    let s: &'static str = (&ChainError::Provider(ProviderError::Disabled)).into();
    assert_eq!(s, "provider");
    let s: &'static str = (&ChainError::Tx(TxError::Rejected(String::new()))).into();
    assert_eq!(s, "tx");
}

#[test]
fn provider_error_flows_into_tx_and_chain() {
    // ProviderError -> TxError via #[from].
    let tx: TxError = ProviderError::Disabled.into();
    assert!(matches!(tx, TxError::Provider(ProviderError::Disabled)));

    // ProviderError -> ChainError and TxError -> ChainError via #[from].
    let chain: ChainError = ProviderError::Disabled.into();
    assert!(matches!(chain, ChainError::Provider(_)));
    let chain: ChainError = TxError::Rejected(String::new()).into();
    assert!(matches!(chain, ChainError::Tx(_)));
}

#[test]
fn tx_request_call_has_no_gas_overrides() {
    let req = TxRequest::call(Address::ZERO, Bytes::from_static(b"\x01\x02"), "test_call");
    assert_eq!(req.to, Some(Address::ZERO));
    assert_eq!(req.value, U256::ZERO);
    assert_eq!(req.gas_limit, None);
    assert_eq!(req.min_gas_limit, None);
    assert_eq!(req.tip_boost_percent, 0);
    assert_eq!(req.description, "test_call");
}

#[test]
fn tx_request_deploy_has_no_recipient() {
    let req = TxRequest::deploy(Bytes::from_static(b"\xde\xad"), "test_deploy");
    assert_eq!(req.to, None);
    assert_eq!(req.data, Bytes::from_static(b"\xde\xad"));
}

#[test]
fn tx_status_is_success() {
    assert!(TxStatus::Success.is_success());
    assert!(!TxStatus::Reverted.is_success());
}

#[test]
fn chain_config_constructors_agree() {
    let mainnet = ChainConfig::mainnet();
    assert_eq!(mainnet.chain_id, 100);
    assert_eq!(
        mainnet.chequebook_factory,
        nectar_contracts::mainnet::CHEQUEBOOK_FACTORY.address
    );
    assert_eq!(
        mainnet.bzz_token,
        nectar_contracts::mainnet::BZZ_TOKEN.address
    );

    let testnet = ChainConfig::testnet();
    assert_eq!(testnet.chain_id, 11155111);
    assert_eq!(
        testnet.price_oracle,
        nectar_contracts::testnet::STORAGE_PRICE_ORACLE.address
    );

    let manual = ChainConfig::from_deployments(
        mainnet.chain_id,
        mainnet.chequebook_factory,
        mainnet.bzz_token,
        mainnet.price_oracle,
    );
    assert_eq!(manual, mainnet);
}

#[tokio::test]
async fn disabled_reader_reports_disabled() {
    let chain = DisabledChain::new();
    assert!(matches!(
        chain.chain_id().await,
        Err(ProviderError::Disabled)
    ));
    assert!(matches!(
        ChainReader::balance(&chain, Address::ZERO).await,
        Err(ProviderError::Disabled)
    ));
}

#[tokio::test]
async fn disabled_sender_reports_disabled() {
    let chain = DisabledChain::new();
    let req = TxRequest::call(Address::ZERO, Bytes::new(), "x");
    assert!(matches!(
        chain.send(req).await,
        Err(TxError::Provider(ProviderError::Disabled))
    ));
    // The default send_and_confirm composition propagates the send failure.
    let req = TxRequest::call(Address::ZERO, Bytes::new(), "x");
    assert!(matches!(
        chain.send_and_confirm(req).await,
        Err(TxError::Provider(ProviderError::Disabled))
    ));
}

#[tokio::test]
async fn disabled_chequebook_reports_disabled() {
    let chain = DisabledChain::new();
    assert!(matches!(
        ChequebookChain::balance(&chain, Address::ZERO).await,
        Err(ChainError::Provider(ProviderError::Disabled))
    ));
    assert!(matches!(
        chain
            .deploy(Address::ZERO, Duration::from_secs(0), B256::ZERO)
            .await,
        Err(ChainError::Provider(ProviderError::Disabled))
    ));
}

#[tokio::test]
async fn disabled_chain_is_object_safe_behind_arc() {
    use std::sync::Arc;
    let reader: Arc<dyn ChainReader> = Arc::new(DisabledChain::new());
    assert!(matches!(
        reader.block_number().await,
        Err(ProviderError::Disabled)
    ));
}
