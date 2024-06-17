#![feature(let_chains)]
use std::collections::BTreeMap;

use node::Node;
use persist::DynLoaderSaver;
use thiserror::Error;

pub mod marshal;
pub mod node;
pub mod persist;
pub mod stringer;
pub mod walker;

const PATH_SEPARATOR: &str = "/";

// node header field constraints
const NODE_OBFUSCATION_KEY_SIZE: usize = 32;
const VERSION_HASH_SIZE: usize = 31;
const NODE_REF_BYTES_SIZE: usize = 1;

// NODE_HEADER_SIZE defines the total size of the header part
const NODE_HEADER_SIZE: usize = NODE_OBFUSCATION_KEY_SIZE + VERSION_HASH_SIZE + NODE_REF_BYTES_SIZE;

// node fork constraints
const NODE_FORK_TYPE_BYTES_SIZE: usize = 1;
const NODE_FORK_PREFIX_BYTES_SIZE: usize = 1;
const NODE_FORK_HEADER_SIZE: usize = NODE_FORK_TYPE_BYTES_SIZE + NODE_FORK_PREFIX_BYTES_SIZE;
const NODE_FORK_PRE_REFERENCE_SIZE: usize = 32;
const NODE_PREFIX_MAX_SIZE: usize = NODE_FORK_PRE_REFERENCE_SIZE - NODE_FORK_HEADER_SIZE;
const NODE_FORK_METADATA_BYTES_SIZE: usize = 2;

const NT_VALUE: u8 = 2;
const NT_EDGE: u8 = 4;
const NT_WITH_PATH_SEPARATOR: u8 = 8;
const NT_WITH_METADATA: u8 = 16;
const NT_MASK: u8 = 255;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send>>;

#[derive(Error, Debug, Clone)]
pub enum MantarayError {
    #[error("Not a value type")]
    NotValueType,
}

pub struct Manifest {
    pub trie: Node,
    ls: Option<DynLoaderSaver>,
}

impl Manifest {
    // new manataray manifest creates a new mantaray-based manifest.
    pub fn new(ls: DynLoaderSaver, encrypted: bool) -> Manifest {
        let mut mm = Manifest {
            ls: Some(ls),
            trie: Node::default(),
        };

        // use emtpy obfuscation key if encryption is not enabled
        if !encrypted {
            mm.trie.obfuscation_key = [0u8; NODE_OBFUSCATION_KEY_SIZE].to_vec();
        }

        mm
    }

    // new_manifest_reference loads existing mantaray-based manifest.
    pub fn new_manifest_reference(reference: Reference, ls: DynLoaderSaver) -> Result<Manifest> {
        let mm = Manifest {
            ls: Some(ls),
            trie: Node::new_node_ref(&reference),
        };

        Ok(mm)
    }

    // add a path and entry to the manifest.
    pub async fn add(&mut self, path: &str, entry: Entry) -> Result<()> {
        self.trie
            .add(
                path.as_bytes(),
                &entry.reference,
                entry.metadata,
                &mut self.ls,
            )
            .await
    }

    // remove a path from the manifest.
    pub async fn remove(&mut self, path: &str) -> Result<()> {
        self.trie.remove(path.as_bytes(), &mut self.ls).await
    }

    // lookup a path in the manifest.
    pub async fn lookup(&mut self, path: &str) -> Result<Entry> {
        let n = self.trie.lookup_node(path.as_bytes(), &mut self.ls).await?;

        // if the node is not a value type, return not found.
        if !n.is_value_type() {
            return Err(Box::new(MantarayError::NotValueType));
        }

        // copy the metadata from the node.
        let metadata = n.metadata.clone();

        Ok(Entry {
            reference: n.entry.clone(),
            metadata,
        })
    }

    // determine if the manifest has a specified prefix.
    pub async fn has_prefix(&mut self, prefix: &str) -> Result<bool> {
        self.trie.has_prefix(prefix.as_bytes(), &mut self.ls).await
    }

    pub async fn set_root(&mut self, metadata: BTreeMap<String, String>) -> Result<()> {
        self.trie
            .add(
                "/".as_bytes(),
                &vec![0; 32].to_vec(),
                metadata,
                &mut self.ls,
            )
            .await?;
        let root_node = self.trie.lookup_node("/".as_bytes(), &mut self.ls).await?;
        let mut type_ = root_node.node_type;
        type_ |= NT_VALUE;
        type_ &= NT_MASK ^ NT_WITH_PATH_SEPARATOR;
        root_node.node_type = type_;
        Ok(())
    }

    pub async fn store(&mut self) -> Result<Vec<u8>> {
        self.trie.save(&Box::new(&self.ls)).await?;

        Ok(self.trie.ref_.clone())
    }

    // todo!{"Finish manifest implementation"}
}

// define a trait that represents a single manifest entry.
pub struct Entry {
    pub reference: Reference,
    pub metadata: BTreeMap<String, String>,
}

type Reference = Vec<u8>;

pub fn keccak256<S>(bytes: S) -> [u8; 32]
where
    S: AsRef<[u8]>,
{
    let mut output = [0u8; 32];
    let mut hasher = Keccak::v256();
    hasher.update(bytes.as_ref());
    hasher.finalize(&mut output);
    output
}
