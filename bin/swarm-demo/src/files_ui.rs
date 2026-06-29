//! Minimal browser UI for the file upload/download/manifest surface.

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Blob, BlobPropertyBag, Document, Event, HtmlAnchorElement, HtmlInputElement, Url};

use crate::SwarmClient;

const FILES_ROOT_ID: &str = "files";
const BATCH_ID: &str = "batch-id";
const OWNER_KEY_ID: &str = "owner-key";
const RPC_URL_ID: &str = "rpc-url";
const FILE_INPUT_ID: &str = "file-input";
const UPLOAD_STATUS_ID: &str = "upload-status";
const REF_INPUT_ID: &str = "ref-input";
const FILES_LOG_ID: &str = "files-log";

fn document() -> Document {
    web_sys::window()
        .and_then(|w| w.document())
        .expect("browser document is available")
}

fn by_id_input(id: &str) -> Option<HtmlInputElement> {
    document()
        .get_element_by_id(id)
        .and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
}

fn set_text(id: &str, text: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        el.set_text_content(Some(text));
    }
}

/// Mount the file panel and wire its handlers to `client`.
pub fn mount(client: SwarmClient) {
    let doc = document();

    let inner = format!(
        "<p class=\"hint\">Upload splits, stamps, and pushes chunks, then \
           returns a mantaray manifest reference. Download reassembles a file \
           reference; manifest paths can be walked.</p>\
         <label>batch id <input id=\"{BATCH_ID}\" type=\"text\" \
           placeholder=\"0x… (32-byte batch id)\" size=\"68\" /></label>\
         <label>owner key <input id=\"{OWNER_KEY_ID}\" type=\"password\" \
           placeholder=\"0x… (32-byte private key)\" size=\"68\" /></label>\
         <label>rpc url (optional) <input id=\"{RPC_URL_ID}\" type=\"text\" \
           placeholder=\"gnosis JSON-RPC - recovers real batch geometry\" \
           size=\"68\" /></label>\
         <p><label>file <input id=\"{FILE_INPUT_ID}\" type=\"file\" /></label></p>\
         <p id=\"{UPLOAD_STATUS_ID}\" class=\"status\"></p>\
         <p><label>reference <input id=\"{REF_INPUT_ID}\" type=\"text\" \
           placeholder=\"0x… (manifest root or file reference)\" size=\"68\" /></label>\
           <button id=\"download-btn\">download</button>\
           <button id=\"ls-btn\">list manifest</button></p>\
         <div id=\"{FILES_LOG_ID}\" class=\"log\"></div>"
    );

    if let Some(container) = doc.get_element_by_id("files-mount") {
        container.set_inner_html(&inner);
    } else {
        let body = doc.body().expect("document has a body");
        let root = doc.create_element("div").expect("create div");
        root.set_class_name("panel");
        root.set_id(FILES_ROOT_ID);
        root.set_inner_html(&format!("<h2>Files</h2>{inner}"));
        body.append_child(&root).expect("append files panel");
    }

    wire_upload(client.clone());
    wire_download(client.clone());
    wire_ls(client);
}

/// Append a line to the files log.
fn log_line(text: &str) {
    let doc = document();
    if let Some(log) = doc.get_element_by_id(FILES_LOG_ID) {
        let row = doc.create_element("div").expect("create row");
        row.set_class_name("event");
        row.set_text_content(Some(text));
        let _ = log.append_child(&row);
    }
}

/// Wire the file input: on change, read the bytes and upload.
fn wire_upload(client: SwarmClient) {
    let Some(input) = by_id_input(FILE_INPUT_ID) else {
        return;
    };

    let cb = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        let client = client.clone();
        spawn_local(async move {
            let Some(input) = by_id_input(FILE_INPUT_ID) else {
                return;
            };
            let Some(files) = input.files() else {
                return;
            };
            let Some(file) = files.get(0) else {
                set_text(UPLOAD_STATUS_ID, "no file selected");
                return;
            };
            let filename = file.name();
            set_text(UPLOAD_STATUS_ID, &format!("reading {filename}…"));

            // Read the file bytes via Blob::array_buffer (a Promise<ArrayBuffer>).
            let blob: &Blob = file.as_ref();
            let buf = match JsFuture::from(blob.array_buffer()).await {
                Ok(b) => b,
                Err(e) => {
                    set_text(UPLOAD_STATUS_ID, &format!("read failed: {e:?}"));
                    return;
                }
            };
            let bytes = Uint8Array::new(&buf).to_vec();

            let batch_id = by_id_input(BATCH_ID).map(|i| i.value()).unwrap_or_default();
            let owner_key = by_id_input(OWNER_KEY_ID)
                .map(|i| i.value())
                .unwrap_or_default();
            // Optional: a gnosis RPC url to recover the batch's real on-chain
            // geometry; empty means "use the default geometry" (the client warns).
            let rpc_url = by_id_input(RPC_URL_ID)
                .map(|i| i.value())
                .unwrap_or_default();
            if batch_id.is_empty() || owner_key.is_empty() {
                set_text(
                    UPLOAD_STATUS_ID,
                    "enter a batch id and owner key before uploading",
                );
                return;
            }

            set_text(
                UPLOAD_STATUS_ID,
                &format!("uploading {filename} ({} bytes)…", bytes.len()),
            );
            match client
                .upload_file(bytes, filename.clone(), batch_id, owner_key, rpc_url, 0)
                .await
            {
                Ok(reference) => {
                    set_text(UPLOAD_STATUS_ID, &format!("uploaded → {reference}"));
                    log_line(&format!("upload {filename} → {reference}"));
                    // Prefill the reference input for convenience.
                    if let Some(r) = by_id_input(REF_INPUT_ID) {
                        r.set_value(&reference);
                    }
                }
                Err(e) => {
                    set_text(UPLOAD_STATUS_ID, &format!("upload failed: {e:?}"));
                }
            }
        });
    });

    input
        .add_event_listener_with_callback("change", cb.as_ref().unchecked_ref())
        .expect("add change listener");
    cb.forget();
}

/// Wire the download button: reassemble the reference and save it.
fn wire_download(client: SwarmClient) {
    let Some(btn) = document().get_element_by_id("download-btn") else {
        return;
    };

    let cb = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        let client = client.clone();
        spawn_local(async move {
            let reference = by_id_input(REF_INPUT_ID)
                .map(|i| i.value())
                .unwrap_or_default();
            if reference.is_empty() {
                log_line("download: enter a reference");
                return;
            }
            log_line(&format!("download {reference}…"));
            match client.download_file(reference.clone()).await {
                Ok(bytes) => {
                    log_line(&format!("downloaded {} bytes", bytes.len()));
                    trigger_save(&bytes, "swarm-download.bin");
                }
                Err(e) => log_line(&format!("download failed: {e:?}")),
            }
        });
    });

    btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .expect("add click listener");
    cb.forget();
}

/// Wire the list-manifest button.
fn wire_ls(client: SwarmClient) {
    let Some(btn) = document().get_element_by_id("ls-btn") else {
        return;
    };

    let cb = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
        let client = client.clone();
        spawn_local(async move {
            let root = by_id_input(REF_INPUT_ID)
                .map(|i| i.value())
                .unwrap_or_default();
            if root.is_empty() {
                log_line("list: enter a manifest root reference");
                return;
            }
            log_line(&format!("list manifest {root}…"));
            match client.ls_manifest(root).await {
                Ok(entries) => {
                    if entries.length() == 0 {
                        log_line("(empty manifest)");
                    }
                    for entry in entries.iter() {
                        let path = js_sys::Reflect::get(&entry, &JsValue::from_str("path"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default();
                        let address = js_sys::Reflect::get(&entry, &JsValue::from_str("address"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default();
                        log_line(&format!("  {path} → {address}"));
                    }
                }
                Err(e) => log_line(&format!("list failed: {e:?}")),
            }
        });
    });

    btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .expect("add click listener");
    cb.forget();
}

/// Trigger a browser file save of `bytes` via a synthetic anchor click.
fn trigger_save(bytes: &[u8], filename: &str) {
    let array = Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());
    let opts = BlobPropertyBag::new();
    opts.set_type("application/octet-stream");
    let Ok(blob) = Blob::new_with_u8_array_sequence_and_options(&parts, &opts) else {
        log_line("save failed: could not build blob");
        return;
    };
    let Ok(url) = Url::create_object_url_with_blob(&blob) else {
        log_line("save failed: could not create object url");
        return;
    };

    let doc = document();
    if let Ok(anchor) = doc.create_element("a") {
        if let Ok(anchor) = anchor.dyn_into::<HtmlAnchorElement>() {
            anchor.set_href(&url);
            anchor.set_download(filename);
            anchor.click();
        }
    }
    let _ = Url::revoke_object_url(&url);
}
