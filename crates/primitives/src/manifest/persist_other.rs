pub struct RecursiveSaveReturnType {
    pub reference: Vec<u8>,
    pub changed: bool,
}

pub struct UploadOptions {
    pub host: String,
    pub stamp: String,
    pub tag: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct UploadedReference {
    reference: String,
}

pub async fn load<F>(storage_loader: F, reference: &[u8]) -> Result<Node, String>
where
    F: Fn(&[u8]) -> Vec<u8>,
{
    if !is_reference(reference) {
        panic!("Wrong reference length. Entry only can be 32 or 64 length in bytes");
    }

    let mut data = storage_loader(reference);
    let mut node: Node = Node::unmarshal_binary(data.as_mut_slice())?;

    node.set_content_address(reference);

    Ok(node)
}

#[async_recursion]
pub async fn upload(&self, options: &UploadOptions) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let request_url = format!("http://{}:1633/bytes", options.host);
    println!("URL: {}", request_url);

    let payload = self.marshal_binary();
    match payload {
        Ok(payload) => {
            let res = client
                .post(request_url)
                .body(payload)
                .header("swarm-postage-batch-id", &options.stamp)
                .send();
            let reference: UploadedReference = res.await.unwrap().json().await.unwrap();
            println!("Reference: {}", reference.reference);
            Ok(hex::decode(reference.reference).unwrap())
        }
        Err(e) => Err(e),
    }
}

pub async fn save(&mut self, options: &UploadOptions) -> Result<Vec<u8>, String> {
    // use recursive save and get the reference of the root node
    let reference = self.recursive_save(options).await?.reference;

    // return reference as slice
    Ok(reference.to_vec())
}

// asynchronous function to save the node and all its forks recursively
#[async_recursion]
async fn recursive_save(
    &mut self,
    options: &UploadOptions,
) -> Result<RecursiveSaveReturnType, String> {
    // async in box to avoid blocking main thread
    // create thread for saving each fork and wait for them to finish
    let mut threads = Vec::new();
    for fork in self.forks.values_mut() {
        threads.push(async move { fork.node.recursive_save(options).await });
    }

    // wait for all forks to finish saving
    // collect all the results and check if any changed
    let mut changed = false;
    for thread in threads {
        let result = thread.await;
        if result.unwrap().changed {
            changed = true;
        }
    }

    if self.ref_.len() > 0 && !changed {
        eprintln!("No changes detected");
        return Ok(RecursiveSaveReturnType {
            reference: self.ref_.clone(),
            changed: false,
        });
    }

    // save the actual manifest as well
    let reference = self.upload(options).await.unwrap();
    self.set_content_address(reference.as_slice());

    Ok(RecursiveSaveReturnType {
        reference,
        changed: true,
    })
}

#[cfg(test)]
mod tests_integration {

    use super::*;
    use reqwest::Error;

    #[derive(Debug, Deserialize, Clone, Default)]
    struct Stamps {
        stamps: Vec<Stamp>,
    }

    #[derive(Debug, Deserialize, Clone, Default)]
    pub struct Stamp {
        #[serde(rename(deserialize = "batchID"))]
        pub batch_id: String,
        #[serde(default)]
        pub utilization: u32,
        #[serde(default)]
        pub usable: bool,
        #[serde(default)]
        pub label: String,
        #[serde(default)]
        pub depth: u8,
        #[serde(default)]
        pub amount: String,
        #[serde(rename(deserialize = "bucketDepth"), default)]
        pub bucket_depth: u8,
        #[serde(rename(deserialize = "blockNumber"), default)]
        pub block_number: u32,
        #[serde(rename(deserialize = "immutableFlag"), default)]
        pub immutable_flag: bool,
        #[serde(default)]
        pub exists: bool,
        #[serde(rename(deserialize = "batchTTL"), default)]
        pub batch_ttl: i32,
    }

    async fn postage_stamp() -> Result<Stamp, Error> {
        let client = reqwest::Client::new();

        // first see if there is a stamp available
        let request_url = format!("http://127.0.0.1:1635/stamps");
        let res = client.get(request_url).send().await.unwrap();

        let stamps: Stamps = res.json().await.unwrap();
        if !stamps.stamps.is_empty() {
            // just blindly return the first stamp
            return Ok(stamps.stamps[0].clone());
        } else {
            let request_url = format!(
                "http://127.0.0.1:1635/stamps/{amount}/{depth}",
                amount = "100000",
                depth = 30
            );
            println!("{}", request_url);

            let res = client.post(request_url).send().await.unwrap();

            let stamp: Stamp = res.json().await.unwrap();
            Ok(stamp)
        }
    }

    #[tokio::test]
    pub async fn fork_removal() {
        let (mut node, paths) = crate::node::tests::get_sample_mantaray_node().unwrap();
        eprintln!("Using stamp: {}", postage_stamp().await.unwrap().batch_id);

        let options = crate::node::UploadOptions {
            host: String::from("127.0.0.1"),
            stamp: postage_stamp().await.unwrap().batch_id,
            tag: String::from("test"),
        };

        // before save, there shouldn't be any content addresses
        eprintln!(
            "Before save: {}",
            serde_json::to_string_pretty(&node)
                .unwrap()
                .replace("\\", "")
        );

        // save
        let original_reference = node.save(&options).await.unwrap().clone();

        // before save, there shouldn't be any content addresses
        eprintln!(
            "After save: {}",
            serde_json::to_string_pretty(&node)
                .unwrap()
                .replace("\\", "")
        );

        eprintln!("Original reference: {}", hex::encode(&original_reference));

        // let get_check_node = || -> &MantarayNode {
        //     &node.get_fork_at_path("path1/valami/".as_bytes()).node
        // };

        // get node at fork at path1/valami/
        // let check_node_1 = &node.get_fork_at_path("path1/valami/".as_bytes()).node;

        // let mut check_node_1_keys =
        //     check_node_1.forks.keys().cloned().collect::<Vec<u8>>();
        // check_node_1_keys.sort();

        // // current forks of node
        // assert_eq!(check_node_1_keys, vec![paths[0][13], paths[1][13]]);

        // remove path_1_clone from node's path
        // node.remove_path(paths[1].clone());

        let check_node_1 = node
            .lookup_node(&Vec::from("path1/valami/".as_bytes()))
            .unwrap();

        assert_eq!(check_node_1.ref_.len() == 0, false);

        node.remove(&paths[1][..].to_vec());

        let check_node_1 = node
            .lookup_node(&Vec::from("path1/valami/".as_bytes()))
            .unwrap();

        // assert_eq!(check_node_1.ref_.len() == 0, true);

        let deleted_reference = node.save(&options).await.unwrap();

        eprintln!(
            "After deletion: {}",
            serde_json::to_string_pretty(&node)
                .unwrap()
                .replace("\\", "")
        );

        eprintln!("Deleted reference: {:?}", hex::encode(&deleted_reference));

        // root reference should not remain the same
        // assert_eq!(original_reference.eq(&deleted_reference), false);
        // assert_ne!(original_reference, deleted_reference);

        // // node.removePath(path2)
        // // const refDeleted = await node.save(saveFunction)
        // // // root reference should not remain the same
        // // expect(refDeleted).not.toStrictEqual(refOriginal)
        // // node.load(loadFunction, refDeleted)
        // // // 'm' key of prefix table disappeared
        // // const checkNode2 = getCheckNode()
        // // expect(Object.keys(checkNode2.forks)).toStrictEqual([String(path1[13])])
        // // // reference should differ because the changed fork set
        // // const refCheckNode2 = checkNode2.getContentAddress
        // // expect(refCheckNode2).not.toStrictEqual(refCheckNode1)
        // eprintln!("Manifest root: {:?}", hex::encode(node.content_address));
    }
}
