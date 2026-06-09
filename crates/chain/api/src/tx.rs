//! A transaction request with a description.
//!
//! Gas estimation, nonce selection, and fee pricing are alloy's job: build the
//! inner [`TransactionRequest`] and let the provider's fillers complete it. The
//! only thing this crate adds is a static description, so a send site can label
//! a transaction (`"chequebook_deploy"`) in logs and metrics without allocating.

use core::ops::{Deref, DerefMut};

use alloy_rpc_types_eth::TransactionRequest;

/// An alloy [`TransactionRequest`] tagged with a static description.
///
/// Derefs to the inner request, so all of alloy's builder methods and fillers
/// apply directly. The [`description`](Self::description) is a `&'static str` so
/// it doubles as a metric label without allocation.
#[derive(Debug, Clone, Default)]
pub struct TxRequest {
    /// The alloy transaction request the provider submits.
    pub request: TransactionRequest,

    /// Static, human-readable label for logs and metrics.
    pub description: &'static str,
}

impl TxRequest {
    /// Tag an existing [`TransactionRequest`] with a description.
    pub fn new(request: TransactionRequest, description: &'static str) -> Self {
        Self {
            request,
            description,
        }
    }
}

impl Deref for TxRequest {
    type Target = TransactionRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

impl DerefMut for TxRequest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.request
    }
}

impl From<TransactionRequest> for TxRequest {
    fn from(request: TransactionRequest) -> Self {
        Self::new(request, "")
    }
}
