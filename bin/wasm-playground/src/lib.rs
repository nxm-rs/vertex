use std::convert::TryInto;

use alloy_signer_wallet::LocalWallet;
use postage::batch;
use rand::RngCore;
use tracing::{debug, error, info, trace, warn};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::window;
use web_sys::HtmlTextAreaElement;
use web_sys::{Event, HtmlButtonElement, HtmlDivElement, HtmlInputElement};

#[wasm_bindgen(start)]
fn main() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    let document = web_sys::window().unwrap().document().unwrap();

    // Create a new div element to contain the private key
    let pk_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    pk_container.set_id("pk_container");
    document
        .body()
        .unwrap()
        .append_child(&pk_container)
        .unwrap();

    // Create a label for the private key
    let pk_label = document.create_element("label").unwrap();
    pk_label.set_text_content(Some("Private key:"));
    pk_container.append_child(&pk_label).unwrap();

    // Create a text area for the private key
    let pk_input = document
        .create_element("input")
        .unwrap()
        .dyn_into::<HtmlInputElement>()
        .unwrap();
    pk_input.set_id("private_key");
    pk_container.append_child(&pk_input).unwrap();

    // Add a button to generate a private key
    let pk_button = document
        .create_element("button")
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap();
    pk_button.set_inner_text("Generate");
    pk_container.append_child(&pk_button).unwrap();

    // Create a new div element to contain the batch id
    let batch_id_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    batch_id_container.set_id("batch_id_container");
    document
        .body()
        .unwrap()
        .append_child(&batch_id_container)
        .unwrap();

    // Create a label for the batch id
    let batch_id_label = document.create_element("label").unwrap();
    batch_id_label.set_text_content(Some("Batch id:"));
    batch_id_container.append_child(&batch_id_label).unwrap();

    // Create an input field for the batch id
    let batch_id_input = document
        .create_element("input")
        .unwrap()
        .dyn_into::<HtmlInputElement>()
        .unwrap();
    batch_id_input.set_id("batch_id");
    batch_id_container.append_child(&batch_id_input).unwrap();

    // Create a new button to generate a batch id
    let batch_id_generate = document
        .create_element("button")
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap();
    batch_id_generate.set_inner_text("Generate");
    batch_id_container.append_child(&batch_id_generate).unwrap();

    // Create a new div element to put in two fields:
    // - the index of the bucket
    // - where in the bucket the chunk is
    let bucket_details_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    bucket_details_container.set_id("bucket_details_container");
    document
        .body()
        .unwrap()
        .append_child(&bucket_details_container)
        .unwrap();

    // Create a label for the batch depth
    let batch_depth_label = document.create_element("label").unwrap();
    batch_depth_label.set_text_content(Some("Batch depth:"));
    bucket_details_container
        .append_child(&batch_depth_label)
        .unwrap();

    // Create an input field for the bucket index
    let batch_depth_input = document
        .create_element("input")
        .unwrap()
        .dyn_into::<HtmlInputElement>()
        .unwrap();
    batch_depth_input.set_id("bucket_index");
    bucket_details_container
        .append_child(&batch_depth_input)
        .unwrap();

    // Create a label for the chunk index in the bucket
    let bucket_depth_label = document.create_element("label").unwrap();
    bucket_depth_label.set_text_content(Some("Bucket depth:"));
    bucket_details_container
        .append_child(&bucket_depth_label)
        .unwrap();

    // Create an input field for the chunk index in the bucket
    let bucket_depth_input = document
        .create_element("input")
        .unwrap()
        .dyn_into::<HtmlInputElement>()
        .unwrap();
    bucket_depth_input.set_id("bucket_depth");
    bucket_details_container
        .append_child(&bucket_depth_input)
        .unwrap();

    // Create a new div element to contain the input and button
    let text_input_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    text_input_container.set_id("container_div");
    document
        .body()
        .unwrap()
        .append_child(&text_input_container)
        .unwrap();

    // Create a label for the input area
    let label = document.create_element("label").unwrap();
    label.set_text_content(Some("Input:"));
    text_input_container.append_child(&label).unwrap();

    // Create an input text area
    let data = document
        .create_element("textarea")
        .unwrap()
        .dyn_into::<HtmlTextAreaElement>()
        .unwrap();
    data.set_id("input_text");
    data.set_cols(80);
    data.set_rows(10);
    text_input_container.append_child(&data).unwrap();

    // create a new div element to contain the calculate button
    let button_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    button_container.set_id("button_container");
    document
        .body()
        .unwrap()
        .append_child(&button_container)
        .unwrap();

    // Create a "Calculate" button
    let button = document
        .create_element("button")
        .unwrap()
        .dyn_into::<HtmlButtonElement>()
        .unwrap();
    button.set_inner_text("Calculate");
    button_container.append_child(&button).unwrap();

    // Create a div element to contain the output
    let output_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    output_container.set_id("output_container");
    document
        .body()
        .unwrap()
        .append_child(&output_container)
        .unwrap();

    // Create a label for the output area
    let output_label = document.create_element("label").unwrap();
    output_label.set_text_content(Some("Output:"));
    output_container.append_child(&output_label).unwrap();

    // Create a text area for the output
    let output = document
        .create_element("textarea")
        .unwrap()
        .dyn_into::<HtmlTextAreaElement>()
        .unwrap();
    output.set_cols(80);
    output.set_rows(10);
    output.set_id("output_text");
    output_container.append_child(&output).unwrap();

    // Create a new div element to contain the serialised pat
    let pat_container = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlDivElement>()
        .unwrap();
    pat_container.set_id("pat_container");
    document
        .body()
        .unwrap()
        .append_child(&pat_container)
        .unwrap();

    // Create a label for the pat
    let pat_label = document.create_element("label").unwrap();
    pat_label.set_text_content(Some("Pat:"));
    pat_container.append_child(&pat_label).unwrap();

    // Create a text area for the pat
    let pat = document
        .create_element("textarea")
        .unwrap()
        .dyn_into::<HtmlTextAreaElement>()
        .unwrap();
    pat.set_cols(80);
    pat.set_rows(10);
    pat.set_id("pat_text");
    pat_container.append_child(&pat).unwrap();

    // the closure that runs when the private key "Generate" button is clicked
    let pk_input_clone = pk_input.clone();
    let pk_closure = Closure::wrap(Box::new(move |_event: Event| {
        // Generate a private key
        // use rand to get a random 32 byte array
        let mut rng = rand::thread_rng();
        let mut pk = [0u8; 32];
        rng.fill_bytes(&mut pk);
        pk_input_clone.set_value(hex::encode(pk).as_str());
    }) as Box<dyn FnMut(Event)>);

    pk_button.set_onclick(Some(pk_closure.as_ref().unchecked_ref()));
    pk_closure.forget();

    // the closure that runs when the batch id "Generate" button is clicked
    let batch_id_input_clone = batch_id_input.clone();
    let batch_id_closure = Closure::wrap(Box::new(move |_event: Event| {
        // Generate a batch id
        // use rand to get a random 32 byte array
        let mut rng = rand::thread_rng();
        let mut batch_id = [0u8; 32];
        rng.fill_bytes(&mut batch_id);
        batch_id_input_clone.set_value(hex::encode(batch_id).as_str());
    }) as Box<dyn FnMut(Event)>);

    batch_id_generate.set_onclick(Some(batch_id_closure.as_ref().unchecked_ref()));
    batch_id_closure.forget();

    // the closure that runs when the "Calculate" button is clicked
    let calculate_closure = Closure::wrap(Box::new(move |_event: Event| {
        let output_clone = output.clone();
        let pat_clone = pat.clone();

        // Build a chunk with the input text. The chunk size is 4096 bytes.
        let f = bmt::file::ChunkedFile::new(
            data.value().as_bytes().to_vec(),
            bmt::chunk::Options {
                max_payload_size: 4096,
            },
        );
        // Display the swarm hash
        output_clone.set_value(format!("Swarm hash: {:?}", hex::encode(f.address())).as_str());

        // Get the batch owner's wallet (and therefore also stamp signer for Postman Pat)
        let wallet = pk_input.value().parse::<LocalWallet>();
        let wallet = match wallet {
            Ok(wallet) => wallet,
            Err(e) => {
                error!("could not parse the private key: {:?}", e);
                return;
            }
        };

        // Make sure the batch id is valid
        let batch_id: [u8; 32] = match hex::decode(batch_id_input.value()) {
            Ok(batch_id) => match batch_id.try_into() {
                Ok(batch_id) => batch_id,
                Err(e) => {
                    error!("could not convert the batch id to a 32 byte array: {:?}", e);
                    return;
                }
            },
            Err(e) => {
                error!("could not decode the batch id: {:?}", e);
                return;
            }
        };

        // Make sure the bucket number is valid
        let batch_depth: u32 = match batch_depth_input.value().parse() {
            Ok(batch_depth) => batch_depth,
            Err(e) => {
                error!("could not parse the batch depth: {:?}", e);
                return;
            }
        };

        // Make sure the chunk index is valid
        let bucket_depth: u32 = match bucket_depth_input.value().parse() {
            Ok(bucket_depth) => bucket_depth,
            Err(e) => {
                error!("could not parse the bucket depth: {:?}", e);
                return;
            }
        };

        // Create the batch
        let batch = batch::Batch::new(
            batch_id,
            0,
            None,
            wallet.address(),
            batch_depth,
            bucket_depth,
            false,
        );

        // Now that we have the batch, we can put Postman Pat to work! 📬
        let mut pat = postage::pat::Pat::new(&batch, 0, false, wallet);

        // Based on the data input, calculate all the chunks
        let chunks = f.leaf_chunks();
        let root_chunk = chunks[0].clone();

        // Calculate the stamp
        wasm_bindgen_futures::spawn_local(async move {
            // iterate over the chunks and stamp them
            for chunk in chunks {
                pat.stamp(chunk, None).await.unwrap();
            }
            // let stamp = pat.stamp(root_chunk, None).await.unwrap();
            // output_clone.set_value(format!("Stamp: {:?}", stamp).as_str());
            pat_clone.set_value(serde_json::to_string(&pat).unwrap().as_str());
        });
    }) as Box<dyn FnMut(Event)>);

    button.set_onclick(Some(calculate_closure.as_ref().unchecked_ref()));
    calculate_closure.forget();

    Ok(())
}
