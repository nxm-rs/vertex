//! wasm-bindgen adapter exposing [`get_stream`](crate::get_stream) and
//! [`put_stream`](crate::put_stream) to JavaScript as async iterators.
//!
//! Each `next()` polls the core stream once, so a slowly-awaiting consumer
//! transitively pauses the network reads. Provider and sender are trait objects
//! because wasm-bindgen cannot export generics. Payload crosses the boundary as
//! a `Uint8Array` copied once per item; inside Rust the chunk stays `Bytes`.

use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::Stream;
use js_sys::{Object, Reflect, Uint8Array};
use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{
    PushReceipt, StampedChunk, SwarmChunkProvider, SwarmChunkSender, SwarmResult,
};
use wasm_bindgen::prelude::*;

use crate::{StreamConfig, VerifiedChunk, get_stream, put_stream};

type CoreGetStream = Pin<Box<dyn Stream<Item = (ChunkAddress, SwarmResult<VerifiedChunk>)>>>;
type CorePutStream = Pin<Box<dyn Stream<Item = (ChunkAddress, SwarmResult<PushReceipt>)>>>;

/// `window_bytes` is kept for ABI stability but ignored; limiting is by chunk count.
fn config_from(_window_bytes: u32, max_concurrency: u32) -> StreamConfig {
    StreamConfig::new(max_concurrency as usize)
}

/// Build `{ done, address, data, stamp, error }` for one download item. `data`
/// is a `Uint8Array` on success or `null` on a per-address error.
fn download_item(
    address: &ChunkAddress,
    result: SwarmResult<VerifiedChunk>,
) -> Result<JsValue, JsValue> {
    let obj = Object::new();
    set(&obj, "done", &JsValue::FALSE)?;
    set(
        &obj,
        "address",
        &Uint8Array::from(address.as_bytes()).into(),
    )?;
    match result {
        Ok(verified) => {
            let (chunk, stamp) = verified.into_parts();
            set(
                &obj,
                "data",
                &Uint8Array::from(chunk.into_bytes().as_ref()).into(),
            )?;
            // A storer may omit the stamp from a delivery.
            let stamp_value = match stamp {
                Some(stamp) => Uint8Array::from(stamp.to_bytes().as_ref()).into(),
                None => JsValue::NULL,
            };
            set(&obj, "stamp", &stamp_value)?;
            set(&obj, "error", &JsValue::NULL)?;
        }
        Err(error) => {
            set(&obj, "data", &JsValue::NULL)?;
            set(&obj, "stamp", &JsValue::NULL)?;
            set(&obj, "error", &JsValue::from_str(&error.to_string()))?;
        }
    }
    Ok(obj.into())
}

/// Build `{ done, address, storer, error }` for one upload ack.
fn upload_item(
    address: &ChunkAddress,
    result: SwarmResult<PushReceipt>,
) -> Result<JsValue, JsValue> {
    let obj = Object::new();
    set(&obj, "done", &JsValue::FALSE)?;
    set(
        &obj,
        "address",
        &Uint8Array::from(address.as_bytes()).into(),
    )?;
    match result {
        Ok(receipt) => {
            set(
                &obj,
                "storer",
                &JsValue::from_str(&receipt.storer.to_string()),
            )?;
            set(&obj, "error", &JsValue::NULL)?;
        }
        Err(error) => {
            set(&obj, "storer", &JsValue::NULL)?;
            set(&obj, "error", &JsValue::from_str(&error.to_string()))?;
        }
    }
    Ok(obj.into())
}

/// The terminal `{ done: true }` object returned once the stream is exhausted.
fn done_item() -> Result<JsValue, JsValue> {
    let obj = Object::new();
    set(&obj, "done", &JsValue::TRUE)?;
    Ok(obj.into())
}

fn set(obj: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(obj, &JsValue::from_str(key), value).map(|_| ())
}

fn parse_address(bytes: &[u8]) -> Result<ChunkAddress, JsValue> {
    ChunkAddress::from_slice(bytes)
        .map_err(|_| JsValue::from_str(&format!("invalid chunk address: {} bytes", bytes.len())))
}

/// Browser-facing streaming download. Constructed by [`get_stream_wasm`]; drive
/// with `await stream.next()` until `{ done: true }`.
#[wasm_bindgen]
pub struct WasmGetStream {
    inner: CoreGetStream,
}

#[wasm_bindgen]
impl WasmGetStream {
    /// Resolve the next download item, or `{ done: true }` when exhausted. Items
    /// arrive in completion order, each carrying its address.
    pub async fn next(&mut self) -> Result<JsValue, JsValue> {
        match self.inner.next().await {
            Some((address, result)) => download_item(&address, result),
            None => done_item(),
        }
    }
}

/// Browser-facing streaming upload. Constructed by [`put_stream_wasm`]; each
/// `await stream.next()` drives the next push to completion and yields its ack.
#[wasm_bindgen]
pub struct WasmPutStream {
    inner: CorePutStream,
}

#[wasm_bindgen]
impl WasmPutStream {
    /// Resolve the next upload ack, or `{ done: true }` when exhausted. Acks
    /// arrive in completion order, each carrying its address.
    pub async fn next(&mut self) -> Result<JsValue, JsValue> {
        match self.inner.next().await {
            Some((address, result)) => upload_item(&address, result),
            None => done_item(),
        }
    }
}

/// Open a streaming download over `provider`. `addresses` is a flat list of
/// 32-byte chunk addresses; a malformed entry is rejected up front.
pub fn get_stream_wasm(
    provider: Arc<dyn SwarmChunkProvider>,
    addresses: Vec<Vec<u8>>,
    window_bytes: u32,
    max_concurrency: u32,
) -> Result<WasmGetStream, JsValue> {
    let addresses: Vec<ChunkAddress> = addresses
        .iter()
        .map(|bytes| parse_address(bytes))
        .collect::<Result<_, _>>()?;
    let config = config_from(window_bytes, max_concurrency);
    let inner = Box::pin(get_stream(provider, addresses, config));
    Ok(WasmGetStream { inner })
}

/// Open a streaming upload over `sender`. `chunks` are pre-reconstructed
/// [`StampedChunk`]s; the caller reconstructs them from wire bytes first.
pub fn put_stream_wasm(
    sender: Arc<dyn SwarmChunkSender>,
    chunks: Vec<StampedChunk>,
    window_bytes: u32,
    max_concurrency: u32,
) -> WasmPutStream {
    let config = config_from(window_bytes, max_concurrency);
    let inner = Box::pin(put_stream(sender, chunks, config));
    WasmPutStream { inner }
}
