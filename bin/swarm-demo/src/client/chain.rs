//! Batch discovery from gnosis `BatchCreated` logs over a raw browser fetch,
//! filtering by (unindexed) owner client-side.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolEvent, sol};
use nectar_postage::Batch;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

/// Mainnet PostageStamp contract address (gnosis).
pub const POSTAGE_STAMP_ADDRESS: &str = "0x45a1502382541Cd610CC9068e88727426b696293";
/// Deployment block of the mainnet PostageStamp contract.
pub const POSTAGE_STAMP_DEPLOY_BLOCK: u64 = 31_305_656;

sol! {
    /// Locally-mirrored PostageStamp `BatchCreated` event (owner unindexed).
    event BatchCreated(
        bytes32 indexed batchId,
        uint256 totalAmount,
        uint256 normalisedBalance,
        address owner,
        uint8 depth,
        uint8 bucketDepth,
        bool immutableFlag
    );
}

/// A batch discovered on-chain for the queried owner.
#[derive(Debug, Clone)]
pub struct DiscoveredBatch {
    /// The batch id.
    pub batch_id: B256,
    /// The batch owner (matches the queried key).
    pub owner: Address,
    /// Batch depth (total capacity = 2^depth chunks).
    pub depth: u8,
    /// Collision-bucket depth.
    pub bucket_depth: u8,
    /// Whether the batch is immutable.
    pub immutable: bool,
    /// Normalised balance (value per chunk) at creation.
    pub normalised_balance: U256,
}

/// Discover batches owned by `owner` from `BatchCreated` logs over the block
/// window (`to_block` `None` means latest).
pub async fn discover_batches(
    owner: Address,
    rpc_url: &str,
    from_block: u64,
    to_block: Option<u64>,
) -> Result<Vec<DiscoveredBatch>, JsValue> {
    let topic0 = BatchCreated::SIGNATURE_HASH;
    let to = match to_block {
        Some(b) => format!("0x{b:x}"),
        None => "latest".to_string(),
    };

    let params = format!(
        r#"[{{"address":"{addr}","fromBlock":"0x{from:x}","toBlock":"{to}","topics":["0x{topic}"]}}]"#,
        addr = POSTAGE_STAMP_ADDRESS,
        from = from_block,
        to = to,
        topic = hex::encode(topic0.as_slice()),
    );
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":{params}}}"#);

    let logs = rpc_get_logs(rpc_url, &body).await?;

    let mut out = Vec::new();
    for log in logs {
        let Some(batch) = decode_batch_created(&log)? else {
            continue;
        };
        if batch.owner == owner {
            out.push(batch);
        }
    }
    Ok(out)
}

impl DiscoveredBatch {
    /// Reconstruct a [`Batch`] from the discovered on-chain geometry.
    pub fn to_batch(&self) -> Batch {
        Batch::new(
            self.batch_id,
            0,
            0,
            self.owner,
            self.depth,
            self.bucket_depth,
            self.immutable,
        )
    }
}

/// Resolve the [`Batch`] geometry for `batch_id` from its `BatchCreated` event;
/// `Ok(None)` if not in the window.
pub async fn resolve_batch(
    batch_id: B256,
    owner: Address,
    rpc_url: &str,
    from_block: u64,
    to_block: Option<u64>,
) -> Result<Option<Batch>, JsValue> {
    let batches = discover_batches(owner, rpc_url, from_block, to_block).await?;
    Ok(batches
        .into_iter()
        .find(|b| b.batch_id == batch_id)
        .map(|b| b.to_batch()))
}

/// One decoded log entry: topics + data.
struct RawLog {
    topics: Vec<B256>,
    data: Vec<u8>,
}

/// POST the JSON-RPC body via browser `fetch` and parse the result logs.
async fn rpc_get_logs(rpc_url: &str, body: &str) -> Result<Vec<RawLog>, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&JsValue::from_str(body));

    let request = Request::new_with_str_and_init(rpc_url, &opts)?;
    request.headers().set("content-type", "application/json")?;

    let resp_value = JsFuture::from(window.fetch_with_request(&request)).await?;
    let resp: Response = resp_value.dyn_into()?;
    let text = JsFuture::from(resp.text()?).await?;
    let text = text
        .as_string()
        .ok_or_else(|| JsValue::from_str("rpc response not text"))?;

    parse_logs_json(&text)
}

/// Parse the `result` log array out of a JSON-RPC response string.
fn parse_logs_json(text: &str) -> Result<Vec<RawLog>, JsValue> {
    // Use js JSON.parse to avoid pulling serde_json; navigate with Reflect.
    let value = js_sys::JSON::parse(text)?;
    if let Ok(err) = js_sys::Reflect::get(&value, &JsValue::from_str("error"))
        && !err.is_undefined()
        && !err.is_null()
    {
        let msg = js_sys::JSON::stringify(&err)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_default();
        return Err(JsValue::from_str(&format!("rpc error: {msg}")));
    }
    let result = js_sys::Reflect::get(&value, &JsValue::from_str("result"))?;
    let array: js_sys::Array = result
        .dyn_into()
        .map_err(|_| JsValue::from_str("rpc result is not an array (range may be too large)"))?;

    let mut out = Vec::new();
    for entry in array.iter() {
        let topics_val = js_sys::Reflect::get(&entry, &JsValue::from_str("topics"))?;
        let topics_arr: js_sys::Array = topics_val
            .dyn_into()
            .map_err(|_| JsValue::from_str("log topics not an array"))?;
        let mut topics = Vec::new();
        for t in topics_arr.iter() {
            let s = t
                .as_string()
                .ok_or_else(|| JsValue::from_str("topic not hex"))?;
            topics.push(parse_b256_hex(&s)?);
        }
        let data_val = js_sys::Reflect::get(&entry, &JsValue::from_str("data"))?;
        let data_hex = data_val
            .as_string()
            .ok_or_else(|| JsValue::from_str("log data not hex"))?;
        let data = parse_hex(&data_hex)?;
        out.push(RawLog { topics, data });
    }
    Ok(out)
}

/// Decode a [`RawLog`] as a `BatchCreated` event, if its topic0 matches.
fn decode_batch_created(log: &RawLog) -> Result<Option<DiscoveredBatch>, JsValue> {
    if log.topics.first() != Some(&BatchCreated::SIGNATURE_HASH) {
        return Ok(None);
    }
    let decoded = BatchCreated::decode_raw_log(log.topics.iter().copied(), &log.data)
        .map_err(|e| JsValue::from_str(&format!("decode BatchCreated: {e}")))?;
    Ok(Some(DiscoveredBatch {
        batch_id: decoded.batchId,
        owner: decoded.owner,
        depth: decoded.depth,
        bucket_depth: decoded.bucketDepth,
        immutable: decoded.immutableFlag,
        normalised_balance: decoded.normalisedBalance,
    }))
}

fn parse_hex(s: &str) -> Result<Vec<u8>, JsValue> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(trimmed).map_err(|e| JsValue::from_str(&format!("bad hex: {e}")))
}

fn parse_b256_hex(s: &str) -> Result<B256, JsValue> {
    let bytes = parse_hex(s)?;
    if bytes.len() != 32 {
        return Err(JsValue::from_str("topic not 32 bytes"));
    }
    Ok(B256::from_slice(&bytes))
}
