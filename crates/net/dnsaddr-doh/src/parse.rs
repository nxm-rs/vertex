//! Parsing of DNS-over-HTTPS JSON responses and dnsaddr TXT records.
//!
//! Both Cloudflare (`https://cloudflare-dns.com/dns-query`) and Google
//! (`https://dns.google/resolve`) answer a `type=TXT` query with the same JSON
//! shape when asked with the `accept: application/dns-json` header: an `Answer`
//! array whose entries carry a numeric `type` and a string `data` field. For TXT
//! records `type` is `16` and `data` is the record text, usually wrapped in the
//! surrounding double quotes that the DNS presentation format uses.

use libp2p::Multiaddr;
use serde::Deserialize;

/// DNS resource-record type for TXT records.
const DNS_TYPE_TXT: u16 = 16;

/// A decoded DNS-over-HTTPS JSON response.
///
/// Only the `Answer` section is modelled; the status and question sections are
/// not needed to extract dnsaddr records.
#[derive(Debug, Clone, Deserialize)]
pub struct DnsJsonResponse {
    /// Answer records, absent when the name has no matching records.
    #[serde(default, rename = "Answer")]
    pub answer: Vec<DnsJsonAnswer>,
}

/// A single answer record from a DNS-over-HTTPS response.
#[derive(Debug, Clone, Deserialize)]
pub struct DnsJsonAnswer {
    /// Numeric DNS record type (16 for TXT).
    #[serde(rename = "type")]
    pub record_type: u16,
    /// Record payload. For TXT records this is the text, normally double-quoted.
    pub data: String,
}

/// Parse a DNS-over-HTTPS JSON body into its answer records.
///
/// # Errors
///
/// Returns the underlying `serde_json` error when the body is not the expected
/// DNS-JSON object.
pub fn parse_dns_json(body: &str) -> Result<DnsJsonResponse, serde_json::Error> {
    serde_json::from_str(body)
}

/// Strip a single layer of surrounding ASCII double quotes from a TXT record.
///
/// DNS presentation format wraps TXT data in double quotes; some providers
/// return them and some do not, so the strip is best-effort.
fn unquote(data: &str) -> &str {
    data.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(data)
}

/// Extract the multiaddr text from every `dnsaddr=<multiaddr>` TXT answer.
///
/// Non-TXT answers and TXT answers that do not start with the `dnsaddr=` prefix
/// are skipped. The returned strings are the raw multiaddr text; parsing into a
/// [`Multiaddr`] happens in the resolver so it can recurse on `/dnsaddr/` entries.
pub fn extract_dnsaddr_values(response: &DnsJsonResponse) -> Vec<String> {
    response
        .answer
        .iter()
        .filter(|a| a.record_type == DNS_TYPE_TXT)
        .filter_map(|a| {
            unquote(a.data.trim())
                .strip_prefix("dnsaddr=")
                .map(|v| v.trim().to_string())
        })
        .collect()
}

/// Whether a multiaddr is a browser-dialable secure-WebSocket leaf.
///
/// A browser can only dial WebSocket transports, and the live network advertises
/// its browser leaves as `/tls/.../ws` (secure WebSocket). Plain `/ws` without
/// TLS and the bare `tcp/1634` siblings are not dialable from a browser, so they
/// are filtered out. The leaf must also carry a `/p2p/` peer id to be dialable.
#[must_use]
pub fn is_browser_dialable_wss(addr: &Multiaddr) -> bool {
    use libp2p::multiaddr::Protocol;

    let mut has_plain_ws = false;
    let mut has_wss = false;
    let mut has_tls = false;
    let mut has_p2p = false;

    for protocol in addr.iter() {
        match protocol {
            Protocol::Ws(_) => has_plain_ws = true,
            Protocol::Wss(_) => has_wss = true,
            Protocol::Tls => has_tls = true,
            Protocol::P2p(_) => has_p2p = true,
            _ => {}
        }
    }

    // `/wss` is TLS-implied; `/tls/.../ws` is the explicit form the network uses.
    let secure_ws = has_wss || (has_plain_ws && has_tls);
    secure_ws && has_p2p
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    /// A captured Cloudflare DNS-JSON response for the dnsaddr root, with the
    /// double-quoted `data` field the provider returns.
    const ROOT_RESPONSE: &str = r#"{
        "Status": 0,
        "Answer": [
            {"name": "_dnsaddr.mainnet.ethswarm.org", "type": 16, "TTL": 300,
             "data": "\"dnsaddr=/dnsaddr/apac.mainnet.ethswarm.org\""},
            {"name": "_dnsaddr.mainnet.ethswarm.org", "type": 16, "TTL": 300,
             "data": "\"dnsaddr=/dnsaddr/emea.mainnet.ethswarm.org\""}
        ]
    }"#;

    /// A captured Google DNS-JSON leaf response with concrete multiaddrs: a
    /// browser-dialable wss leaf and the plain tcp/1634 sibling to be filtered.
    const LEAF_RESPONSE: &str = r#"{
        "Status": 0,
        "Answer": [
            {"name": "_dnsaddr.city.mainnet.ethswarm.org", "type": 16,
             "data": "dnsaddr=/ip4/5.78.94.214/tcp/1635/tls/sni/5-78-94-214.k2k4r8pobzefjwtmnob5hb4aw8idrmzh8epsvcjo007e79s2hf8073z3.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg"},
            {"name": "_dnsaddr.city.mainnet.ethswarm.org", "type": 16,
             "data": "dnsaddr=/ip4/5.78.94.214/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg"}
        ]
    }"#;

    #[test]
    fn parses_quoted_root_records() {
        let parsed = parse_dns_json(ROOT_RESPONSE).expect("valid json");
        let values = extract_dnsaddr_values(&parsed);
        assert_eq!(
            values,
            vec![
                "/dnsaddr/apac.mainnet.ethswarm.org",
                "/dnsaddr/emea.mainnet.ethswarm.org",
            ]
        );
    }

    #[test]
    fn parses_unquoted_leaf_records() {
        let parsed = parse_dns_json(LEAF_RESPONSE).expect("valid json");
        let values = extract_dnsaddr_values(&parsed);
        assert_eq!(values.len(), 2);
        assert!(values[0].contains("/ws/p2p/"));
        assert!(values[1].contains("/tcp/1634/p2p/"));
    }

    #[test]
    fn empty_answer_yields_no_values() {
        let parsed = parse_dns_json(r#"{"Status": 0}"#).expect("valid json");
        assert!(extract_dnsaddr_values(&parsed).is_empty());
    }

    #[test]
    fn non_txt_answers_are_skipped() {
        let body = r#"{"Answer": [{"type": 1, "data": "5.78.94.214"}]}"#;
        let parsed = parse_dns_json(body).expect("valid json");
        assert!(extract_dnsaddr_values(&parsed).is_empty());
    }

    #[test]
    fn malformed_json_errors() {
        assert!(parse_dns_json("not json").is_err());
    }

    #[test]
    fn wss_leaf_is_browser_dialable() {
        let addr: Multiaddr = "/ip4/5.78.94.214/tcp/1635/tls/sni/example.libp2p.direct/ws/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg".parse().unwrap();
        assert!(is_browser_dialable_wss(&addr));
    }

    #[test]
    fn plain_tcp_leaf_is_not_dialable() {
        let addr: Multiaddr =
            "/ip4/5.78.94.214/tcp/1634/p2p/QmfEugihe2Pm78YomGupdxSt46Uxgg4DLpjkzgzzeouiKg"
                .parse()
                .unwrap();
        assert!(!is_browser_dialable_wss(&addr));
    }

    #[test]
    fn ws_without_tls_or_p2p_is_not_dialable() {
        let plain_ws: Multiaddr = "/ip4/5.78.94.214/tcp/1635/ws".parse().unwrap();
        assert!(!is_browser_dialable_wss(&plain_ws));
    }
}
