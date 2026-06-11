//! Browser `fetch`-backed DNS-over-HTTPS [`TxtFetcher`].
//!
//! This module is wasm-only: it issues the DoH query through the browser `fetch`
//! API via `gloo_net`. The endpoint is configurable so an operator can point at
//! a provider they trust. The default is Cloudflare; Google is offered as an
//! alternate. Both answer the `application/dns-json` query shape the parser
//! expects.
//!
//! # Privacy
//!
//! A DoH resolver sees every query the browser sends it, so the chosen provider
//! learns that this client is resolving the Swarm bootnode names. That is an
//! accepted tradeoff for a browser demo and is mitigated by making the endpoint
//! configurable; a deployment with stricter requirements points [`DohClient`] at
//! a self-hosted or otherwise trusted resolver.

use std::future::Future;
use std::pin::Pin;

use gloo_net::http::Request;

use crate::error::DohError;
use crate::resolver::TxtFetcher;

/// Cloudflare DNS-over-HTTPS endpoint (`application/dns-json`).
pub const CLOUDFLARE_DOH: &str = "https://cloudflare-dns.com/dns-query";

/// Google DNS-over-HTTPS endpoint (`application/dns-json`).
pub const GOOGLE_DOH: &str = "https://dns.google/resolve";

/// A browser `fetch`-backed DoH client targeting a configurable endpoint.
///
/// Construct with [`DohClient::cloudflare`] or [`DohClient::google`] for the
/// built-in providers, or [`DohClient::new`] to point at any
/// `application/dns-json` resolver.
#[derive(Debug, Clone)]
pub struct DohClient {
    endpoint: String,
}

impl Default for DohClient {
    fn default() -> Self {
        Self::cloudflare()
    }
}

impl DohClient {
    /// Create a client targeting an arbitrary `application/dns-json` endpoint.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    /// Create a client targeting the Cloudflare DoH endpoint.
    #[must_use]
    pub fn cloudflare() -> Self {
        Self::new(CLOUDFLARE_DOH)
    }

    /// Create a client targeting the Google DoH endpoint.
    #[must_use]
    pub fn google() -> Self {
        Self::new(GOOGLE_DOH)
    }

    /// The configured DoH endpoint URL.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl TxtFetcher for DohClient {
    fn fetch_txt(
        &self,
        name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, DohError>> + '_>> {
        let query_name = format!("_dnsaddr.{name}");
        let endpoint = self.endpoint.clone();
        Box::pin(async move {
            let response = Request::get(&endpoint)
                .query([("name", query_name.as_str()), ("type", "TXT")])
                .header("accept", "application/dns-json")
                .send()
                .await
                .map_err(|e| DohError::Request(e.to_string()))?;

            if !response.ok() {
                return Err(DohError::Request(format!("status {}", response.status())));
            }

            response
                .text()
                .await
                .map_err(|e| DohError::Request(e.to_string()))
        })
    }
}
