//! Browser binding for the streaming get/put pipelines.
//!
//! This is the wasm-bindgen adapter over the transport-agnostic core in this
//! crate: the same byte-bounded [`get_stream`](crate::get_stream)
//! and [`put_stream`](crate::put_stream) pipelines, surfaced to JavaScript as
//! async iterators. A browser host drives them with `for await (const item of
//! stream)`; each `next()` resolves to a plain object carrying the chunk (or the
//! per-item error) and a `done` flag, the JS async-iterator protocol.
//!
//! The bound lives in Rust exactly as on native: the adapter holds the core
//! stream and polls it one item per `next()` call, so a browser consumer that
//! awaits slowly transitively pauses the network reads and the heap stays within
//! the configured byte window. Payload crosses the boundary as a `js_sys`
//! `Uint8Array` copied once per item; inside Rust the chunk stays `Bytes`.
//!
//! The provider and sender are taken as trait objects
//! (`Arc<dyn SwarmChunkProvider>`, `Arc<dyn SwarmChunkSender>`) so the exported
//! types are concrete (wasm-bindgen cannot export generics) while still wrapping
//! the one core. The browser client wires its real provider in when the wasm
//! client builder lands; until then the adapter is exercised against in-memory
//! providers in tests, the same core path the native FFI uses.

use std::collections::VecDeque;
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

/// Boxed core download stream over a trait-object provider.
type CoreGetStream = Pin<Box<dyn Stream<Item = SwarmResult<VerifiedChunk>>>>;
/// Boxed core upload stream over a trait-object sender.
type CorePutStream = Pin<Box<dyn Stream<Item = SwarmResult<PushReceipt>>>>;

/// Translate the boundary stream config from JS into the core config.
///
/// Both knobs are clamped to at least one by the core, so a zero from JS
/// degrades to one-at-a-time streaming rather than deadlocking.
fn config_from(window_bytes: u32, max_concurrency: u32) -> StreamConfig {
    StreamConfig::new(window_bytes as usize, max_concurrency as usize)
}

/// Build the JS result object `{ done, address, data, error }` for one download
/// item.
///
/// `data` is a `Uint8Array` of the chunk's wire bytes on success, or `null` on a
/// per-address error; `error` carries the failure message. A single object shape
/// keeps the JS async-iterator contract uniform across success and failure.
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
            // One copy at the boundary; the chunk stayed `Bytes` until here.
            set(
                &obj,
                "data",
                &Uint8Array::from(chunk.into_bytes().as_ref()).into(),
            )?;
            // A storer may omit the stamp from a delivery; emit a null stamp
            // when absent.
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

/// Build the JS result object `{ done, address, error }` for one upload ack.
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

/// The terminal `{ done: true }` object every async iterator returns once the
/// stream is exhausted.
fn done_item() -> Result<JsValue, JsValue> {
    let obj = Object::new();
    set(&obj, "done", &JsValue::TRUE)?;
    Ok(obj.into())
}

/// Set `key` to `value` on `obj`, mapping a reflection failure to a `JsValue`
/// error.
fn set(obj: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(obj, &JsValue::from_str(key), value).map(|_| ())
}

/// Parse a 32-byte chunk address from a JS byte array, rejecting a wrong length.
fn parse_address(bytes: &[u8]) -> Result<ChunkAddress, JsValue> {
    ChunkAddress::from_slice(bytes)
        .map_err(|_| JsValue::from_str(&format!("invalid chunk address: {} bytes", bytes.len())))
}

/// A browser-facing streaming download.
///
/// Constructed by [`get_stream_wasm`]. Drive it from JS with the async-iterator
/// protocol: each `await stream.next()` yields the next download item, ending
/// with `{ done: true }`. The pipeline keeps outstanding payload within the byte
/// window passed at construction.
#[wasm_bindgen]
pub struct WasmGetStream {
    inner: CoreGetStream,
    addresses: VecDeque<ChunkAddress>,
}

#[wasm_bindgen]
impl WasmGetStream {
    /// Resolve the next download item, or `{ done: true }` when exhausted.
    ///
    /// Polls the core stream once. Because the adapter advances only when JS
    /// awaits, a slow consumer paces the network reads and the in-flight byte
    /// window is never exceeded.
    pub async fn next(&mut self) -> Result<JsValue, JsValue> {
        match self.inner.next().await {
            Some(result) => {
                // Output order matches request order one-to-one, so the address
                // for this item is the next one in the original list. A core
                // item without a paired address is impossible (the counts match
                // by construction); fall back to a zero address rather than
                // panic if that invariant ever breaks.
                let address = self.addresses.pop_front().unwrap_or_default();
                download_item(&address, result)
            }
            None => done_item(),
        }
    }
}

/// A browser-facing streaming upload.
///
/// Constructed by [`put_stream_wasm`]. Each `await stream.next()` feeds the next
/// chunk's push to completion and yields its ack, ending with `{ done: true }`.
/// Rust owns the bounded in-flight window, so a slow consumer pauses the pushes.
#[wasm_bindgen]
pub struct WasmPutStream {
    inner: CorePutStream,
    addresses: VecDeque<ChunkAddress>,
}

#[wasm_bindgen]
impl WasmPutStream {
    /// Resolve the next upload ack, or `{ done: true }` when exhausted.
    pub async fn next(&mut self) -> Result<JsValue, JsValue> {
        match self.inner.next().await {
            Some(result) => {
                let address = self.addresses.pop_front().unwrap_or_default();
                upload_item(&address, result)
            }
            None => done_item(),
        }
    }
}

/// Open a byte-bounded streaming download over `provider`.
///
/// `addresses` is a flat list of 32-byte chunk addresses; a malformed entry is
/// rejected up front. The returned [`WasmGetStream`] is a JS async iterator over
/// the verified chunks, never exceeding `window_bytes` of in-flight payload.
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
    let inner = Box::pin(get_stream(provider, addresses.clone(), config));
    Ok(WasmGetStream {
        inner,
        addresses: addresses.into(),
    })
}

/// Open a byte-bounded streaming upload over `sender`.
///
/// `chunks` are pre-reconstructed [`StampedChunk`]s; the caller (the wasm client)
/// reconstructs them from wire bytes before calling, the same way the native FFI
/// does at its boundary. The returned [`WasmPutStream`] is a JS async iterator
/// over the per-chunk acks, never exceeding `window_bytes` of in-flight payload.
pub fn put_stream_wasm(
    sender: Arc<dyn SwarmChunkSender>,
    chunks: Vec<StampedChunk>,
    window_bytes: u32,
    max_concurrency: u32,
) -> WasmPutStream {
    let addresses: VecDeque<ChunkAddress> = chunks.iter().map(|c| *c.address()).collect();
    let config = config_from(window_bytes, max_concurrency);
    let inner = Box::pin(put_stream(sender, chunks, config));
    WasmPutStream { inner, addresses }
}
