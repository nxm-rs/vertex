use crate::Result;
// use async_recursion::async_recursion;
// use async_trait::async_trait;
use std::{future::Future, pin::Pin};
use bee_api::BeeConfig;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{keccak256, marshal::Marshal, node::Node};

#[derive(Error, Debug, Clone)]
pub enum MantarayPersistError {
    #[error("No loader provided")]
    NoLoaderError,
}

// loader defines a trait that retrieves nodes by reference from a storage backend.
pub trait Loader {
    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>>;
}

// saver defines a trait that stores nodes by reference to a storage backend.
pub trait Saver {
    async fn save(&self, data: &[u8]) -> Result<Vec<u8>>;
}

pub trait LoaderSaver: Debug + Sync {
    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>>;
    async fn save(&self, data: &[u8]) -> Result<Vec<u8>>;
    async fn as_dyn(&self) -> &Self;
}

impl Node {
    // a load function for nodes
    pub async fn load(&mut self, l: &mut Option<impl LoaderSaver>) -> Result<()> {
        // if ref_ is not a reference, return Ok
        if self.ref_.is_empty() {
            return Ok(());
        }

        // if l is not a loader, return no loader error
        if l.is_none() {
            return Err(Box::new(MantarayPersistError::NoLoaderError));
        }

        // load the node from the storage backend
        let ref_ = self.ref_.clone();
        let t = l.as_mut().unwrap();
        let mut data = t.load(&ref_).await?;
        // let t = l.as_mut().unwrap().load(&self.ref_).await?;
        // let mut data = l.as_ref().unwrap().load(&ref_).await?;

        // unmarshall the node from dta into self
        self.unmarshal_binary(&mut data)?;

        // return success
        Ok(())
    }

    // save persists a trie recursively traversing the nodes
    pub async fn save(&mut self, s: &impl LoaderSaver) -> Result<()> {
        self.save_recursive(Box::pin(s)).await
    }

    pub async fn save_recursive(&mut self, s: Pin<Box<&impl LoaderSaver>>) -> Pin<Box<dyn Future<Output = Result<()>>>> {
        // if ref_ is already a reference, return
        if !self.ref_.is_empty() {
            return Box::pin(Ok(()));
        }

        // recurse through the fork values of the node and save them
        // TODO! This is the area in which we can optimize the saving process.
        for fork in self.forks.values_mut() {
            fork.node.save_recursive(s.clone()).await?;
        }

        // marshal the node to a slice of bytes
        let slice = self.marshal_binary()?;

        // save the node to the storage backend
        self.ref_ = s.as_ref().save(&slice).await?;

        self.forks.clear();

        Ok(())
    }
}

pub type Address = [u8; 32];

#[derive(Debug, Default)]
pub struct MockLoadSaver {
    store: Arc<Mutex<HashMap<Address, Vec<u8>>>>,
}

impl MockLoadSaver {
    pub fn new() -> MockLoadSaver {
        MockLoadSaver {
            store: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl LoaderSaver for MockLoadSaver {
    async fn as_dyn(&self) -> &MockLoadSaver {
        self
    }

    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>> {
        let store = self.store.lock().await;
        let data = store.get(ref_).unwrap();
        Ok(data.clone())
    }

    async fn save(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut store = self.store.lock().await;
        let ref_ = keccak256(data);
        store.insert(ref_, data.to_vec());
        Ok(ref_.to_vec())
    }
}

#[derive(Debug)]
pub struct BeeLoadSaver {
    pub uri: String,
    pub config: BeeConfig,
    pub client: reqwest::Client,
}

impl BeeLoadSaver {
    pub fn new(uri: String, config: BeeConfig) -> BeeLoadSaver {
        BeeLoadSaver {
            uri,
            config,
            client: reqwest::Client::new(),
        }
    }
}

// #[async_trait]
// impl LoaderSaver for Box<dyn LoaderSaver + Send> {
//     async fn as_dyn(&self) -> &dyn LoaderSaver {
//         self.as_ref()
//     }

//     async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>> {
//         self.as_ref().load(ref_).await
//     }

//     async fn save(&self, data: &[u8]) -> Result<Vec<u8>> {
//         self.as_ref().save(data).await
//     }
// }

impl LoaderSaver for Arc<BeeLoadSaver> {
    async fn as_dyn(&self) -> &Arc<BeeLoadSaver> {
        self
    }

    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>> {
        todo!("Fix bee_api")
        // Ok(
        //     bee_api::bytes_get(&self.client, self.uri.clone(), hex::encode(ref_))
        //         .await?
        //         .0,
        // )
    }

    async fn save(&self, data: &[u8]) -> Result<Vec<u8>> {
        todo!("Fix bee_api")
        // match hex::decode(
        //     bee_api::bytes_post(
        //         &self.client,
        //         self.uri.clone(),
        //         data.to_vec(),
        //         self.config
        //             .upload
        //             .as_ref()
        //             .expect("UploadConfig not specified"),
        //     )
        //     .await?
        //     .ref_,
        // ) {
        //     Ok(ref_) => Ok(ref_),
        //     Err(e) => Err(Box::new(e)),
        // }
    }
}

impl LoaderSaver for Mutex<MockLoadSaver> {
    async fn as_dyn(&self) -> &Mutex<MockLoadSaver> {
        &self
    }

    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>> {
        self.lock().await.load(ref_).await
    }

    async fn save(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.lock().await.save(data).await
    }
}

impl LoaderSaver for Arc<Mutex<MockLoadSaver>> {
    async fn as_dyn(&self) -> &Arc<Mutex<MockLoadSaver>> {
        &self
    }

    async fn load(&mut self, ref_: &[u8]) -> Result<Vec<u8>> {
        self.lock().await.load(ref_).await
    }

    async fn save(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.lock().await.save(data).await
    }
}

// tests
#[cfg(test)]
mod tests {
    use crate::{Entry, Manifest};

    use super::*;

    use std::{collections::BTreeMap, sync::Arc};
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn persist_idempotence() {
        let ls = Arc::new(Mutex::new(MockLoadSaver::new()));

        let mut n = Manifest::new(Box::new(ls), false);

        // declare a vector of byte strings
        let paths = vec![
            "aa", "b", "aaaaaa", "aaaaab", "abbbb", "abbba", "bbbbba", "bbbaaa", "bbbaab",
        ];

        for path in &paths {
            let c = path;
            n.store().await.unwrap();
            // create a variable v that is a clone of the byte string c padded to 32 bytes
            let mut v = c.as_bytes().to_vec();
            v.resize(32, 0);
            n.add(
                path,
                Entry {
                    reference: v.clone(),
                    metadata: BTreeMap::new(),
                },
            )
            .await
            .unwrap();
        }

        n.store().await.unwrap();

        for path in &paths {
            let c = path;
            let entry = n.lookup(c).await.unwrap();
            let mut v = c.as_bytes().to_vec();
            println!("{:?}", entry.reference);
            v.resize(32, 0);
            assert_eq!(entry.reference, v);
        }
    }
}
