use crate::persist::DynLoaderSaver;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use serde::*;
use serde_with::serde_as;
use thiserror::Error;

use crate::Result;

use crate::manifest::{
    NODE_OBFUSCATION_KEY_SIZE, NODE_PREFIX_MAX_SIZE, NT_EDGE, NT_MASK, NT_VALUE, NT_WITH_METADATA,
    NT_WITH_PATH_SEPARATOR, PATH_SEPARATOR,
};

#[serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Node {
    pub node_type: u8,
    pub ref_bytes_size: u32,
    #[serde_as(as = "serde_with::hex::Hex")]
    pub obfuscation_key: Vec<u8>,
    #[serde_as(as = "serde_with::hex::Hex")]
    pub ref_: Vec<u8>,
    #[serde_as(as = "serde_with::hex::Hex")]
    pub entry: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
    pub forks: HashMap<u8, Fork>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Fork {
    #[serde_as(as = "serde_with::hex::Hex")]
    pub prefix: Vec<u8>,
    pub node: Node,
}

#[derive(Error, Debug, Clone)]
pub enum MantarayNodeError {
    #[error("No fork found for node: {0}")]
    NoForkForNode(String),
    #[error("No entry found for node: {0}")]
    NoEntryForNode(String),
    #[error("Node entry too large: {0} > {1}")]
    NodeEntryTooLarge(usize, usize),
    #[error("Node entry size mismatch: {0} != {1}")]
    NodeEntrySizeMismatch(usize, usize),
    #[error("Empty path")]
    EmptyPath,
    #[error("Path prefix not found: {0}")]
    PathPrefixNotFound(String),
}

// find the index at which a subslice exists within a slice
fn find_index_of_array(slice: &[u8], subslice: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i <= slice.len() - subslice.len() {
        if slice[i..i + subslice.len()].to_vec() == subslice.to_vec() {
            return Some(i);
        }
        i += 1;
    }
    None
}

// return the common part of two slices starting from index 0
fn common(slice: &[u8], subslice: &[u8]) -> Vec<u8> {
    let mut i = 0;
    while i < slice.len() && i < subslice.len() {
        if slice[i] != subslice[i] {
            break;
        }
        i += 1;
    }
    slice[0..i].to_vec()
}

impl Node {
    pub fn new_node_ref(ref_: &[u8]) -> Node {
        Node {
            ref_: ref_.to_vec(),
            ..Default::default()
        }
    }

    // node type related functions

    // IsValueType returns true if the node contains entry.
    pub fn is_value_type(&self) -> bool {
        (self.node_type & NT_VALUE) == NT_VALUE
    }

    // IsEdgeType returns true if the node forks into other nodes.
    pub fn is_edge_type(&self) -> bool {
        (self.node_type & NT_EDGE) == NT_EDGE
    }

    // IsWithPathSeparatorType returns true if the node path contains separator character.
    pub fn is_with_path_separator_type(&self) -> bool {
        (self.node_type & NT_WITH_PATH_SEPARATOR) == NT_WITH_PATH_SEPARATOR
    }

    // IsWithMetadataType returns true if the node contains metadata.
    pub fn is_with_metadata_type(&self) -> bool {
        (self.node_type & NT_WITH_METADATA) == NT_WITH_METADATA
    }

    fn make_value(&mut self) {
        self.node_type |= NT_VALUE
    }

    pub fn make_edge(&mut self) {
        self.node_type |= NT_EDGE
    }

    fn make_with_path_separator(&mut self) {
        self.node_type |= NT_WITH_PATH_SEPARATOR
    }

    fn make_with_metadata(&mut self) {
        self.node_type |= NT_WITH_METADATA
    }

    pub fn make_not_value(&mut self) {
        self.node_type &= NT_MASK ^ NT_VALUE
    }

    // fn make_not_edge(&mut self) {
    //     self.node_type &= NT_MASK ^ NT_EDGE
    // }

    fn make_not_with_path_separator(&mut self) {
        self.node_type &= NT_MASK ^ NT_WITH_PATH_SEPARATOR
    }

    // fn make_not_with_metadata(&mut self) {
    //     self.node_type &= NT_MASK ^ NT_WITH_METADATA
    // }

    fn set_obfuscation_key(&mut self, key: &[u8]) {
        if key.len() != NODE_OBFUSCATION_KEY_SIZE {
            panic!("Invalid key length");
        }

        self.obfuscation_key = key.to_vec();
    }

    // lookupnode finds the node for a path or returns error if not found.
    #[async_recursion]
    pub async fn lookup_node(
        &mut self,
        path: &[u8],
        l: &mut Option<DynLoaderSaver>,
    ) -> Result<&mut Node> {
        // if forks hashmap is empty, perhaps we haven't loaded the forks yet
        if self.forks.is_empty() {
            self.load(l).await?;
        }

        // if the path is empty return the current node
        if path.is_empty() {
            return Ok(self);
        }

        match self.forks.get_mut(&path[0]) {
            None => Err(
                Box::new(MantarayNodeError::NoForkForNode(hex::encode(&self.ref_)))
                    as Box<dyn Error + Send>,
            ),
            Some(f) => {
                // get the common prefix of the fork and the path
                let c = common(&f.prefix, path);

                // if c is the same length as the fork prefix then recursive lookup node
                if c.len() == f.prefix.len() {
                    f.node.lookup_node(&path[c.len()..], l).await
                } else {
                    Err(
                        Box::new(MantarayNodeError::NoForkForNode(hex::encode(&self.ref_)))
                            as Box<dyn Error + Send>,
                    )
                }
            }
        }
    }

    // lookup finds the entry for a path or returns error if not found
    pub async fn lookup(&mut self, path: &[u8], l: &mut Option<DynLoaderSaver>) -> Result<&[u8]> {
        let node = self.lookup_node(path, l).await?;
        // if node is not value type and path lengther is greater than 0 return error
        if !node.is_value_type() && !path.is_empty() {
            return Err(
                Box::new(MantarayNodeError::NoEntryForNode(hex::encode(&node.ref_)))
                    as Box<dyn Error + Send>,
            );
        }

        Ok(node.entry.as_slice())
    }

    // Add adds an entry to the path with metadata
    #[async_recursion]
    pub async fn add(
        &mut self,
        path: &[u8],
        entry: &[u8],
        metadata: BTreeMap<String, String>,
        ls: &mut Option<DynLoaderSaver>,
    ) -> Result<()> {
        if self.ref_bytes_size == 0 {
            if entry.len() > 256 {
                return Err(
                    Box::new(MantarayNodeError::NodeEntryTooLarge(entry.len(), 256))
                        as Box<dyn Error + Send>,
                );
            }
            // empty entry for directories
            if !entry.is_empty() {
                self.ref_bytes_size = entry.len() as u32;
            }
        } else if !entry.is_empty() && entry.len() != self.ref_bytes_size as usize {
            return Err(Box::new(MantarayNodeError::NodeEntrySizeMismatch(
                entry.len(),
                self.ref_bytes_size as usize,
            )) as Box<dyn Error + Send>);
        }

        // if path is empty then set entry and return
        if path.is_empty() {
            self.entry = entry.to_vec();
            self.make_value();

            // if metadata is not empty then set metadata and type flag then return
            if !metadata.is_empty() {
                self.metadata = metadata;
                self.make_with_metadata();
            }

            // set self ref to empty vec
            self.ref_ = vec![];
            return Ok(());
        }

        // if forks hashmap is empty, perhaps we haven't loaded the forks yet
        if self.forks.is_empty() {
            self.load(ls).await?;
            self.ref_ = vec![];
        }

        // try get the fork at the first character of the path
        let mut f = self.forks.get_mut(&path[0]);
        if f.is_none() {
            // create a new node
            let mut nn = Node::default();

            // if an obfuscation key is set then set it to the new node
            if !self.obfuscation_key.is_empty() {
                nn.set_obfuscation_key(&self.obfuscation_key);
            }

            nn.ref_bytes_size = self.ref_bytes_size;

            // check the prefix size limit
            if path.len() > NODE_PREFIX_MAX_SIZE {
                // split the path into two parts
                let (prefix, rest) = path.split_at(NODE_PREFIX_MAX_SIZE);

                // add rest to the new node
                nn.add(rest, entry, metadata, ls).await?;
                nn.update_is_with_path_separator(prefix);

                // add the new node to the forks hashmap
                self.forks.insert(
                    path[0],
                    Fork {
                        prefix: prefix.to_vec(),
                        node: nn,
                    },
                );
                self.make_edge();

                // return
                return Ok(());
            }

            nn.entry = entry.to_vec();

            // if metadata is not empty then set metadata and type flag
            if !metadata.is_empty() {
                nn.metadata = metadata;
                nn.make_with_metadata();
            }

            nn.make_value();
            nn.update_is_with_path_separator(path);
            self.forks.insert(
                path[0],
                Fork {
                    prefix: path.to_vec(),
                    node: nn,
                },
            );
            self.make_edge();
            return Ok(());
        }

        // get the common prefix of the fork and the path, then get the rest of the path
        let c = common(&f.as_ref().unwrap().prefix, path);
        let rest = f.as_ref().unwrap().prefix[c.len()..].to_vec();

        // get mutable reference to the fork node
        let mut nn = f.as_ref().unwrap().node.clone();

        // if the rest of the path is not empty move current common prefix node
        if !rest.is_empty() {
            // move current common prefix ndoe
            nn = Node::default();

            // if an obfuscation key is set then set it to the new node
            if !self.obfuscation_key.is_empty() {
                nn.set_obfuscation_key(&self.obfuscation_key);
            }

            nn.ref_bytes_size = self.ref_bytes_size;

            // update the fork node with the rest of the path
            f.as_mut()
                .unwrap()
                .node
                .update_is_with_path_separator(&rest);

            // add the fork node to the new node forks hashmap
            nn.forks.insert(
                rest[0],
                Fork {
                    prefix: rest.to_vec(),
                    node: f.unwrap().node.clone(),
                },
            );
            nn.make_edge();

            // if common path is full path new node is value type
            if c.len() == path.len() {
                nn.make_value();
            }
        }
        // note: special case on edge split
        nn.update_is_with_path_separator(path);

        // add new node for shared prefix
        nn.add(&path[c.len()..], entry, metadata, ls).await?;

        // add the new node to the forks hashmap
        self.forks.insert(
            path[0],
            Fork {
                prefix: c.to_vec(),
                node: nn,
            },
        );
        self.make_edge();

        // return
        Ok(())
    }

    fn update_is_with_path_separator(&mut self, path: &[u8]) {
        // if path conatins a path separator at an index greater than 0 then set is_with_path_separator flag
        for i in path.iter().skip(1) {
            if *i == PATH_SEPARATOR.as_bytes()[0] {
                self.make_with_path_separator();
                return;
            }
        }

        self.make_not_with_path_separator();
    }

    // remove removes a path from the node
    #[async_recursion]
    pub async fn remove(&mut self, path: &[u8], ls: &mut Option<DynLoaderSaver>) -> Result<()> {
        // if path is empty then return error
        if path.is_empty() {
            return Err(Box::new(MantarayNodeError::EmptyPath) as Box<dyn Error + Send>);
        }

        // if forks is empty then load
        if self.forks.is_empty() {
            self.load(ls).await?;
        }

        // if path is not empty then get the fork at the first character of the path
        let f = self.forks.get_mut(&path[0]);
        if f.is_none() {
            return Err(Box::new(MantarayNodeError::PathPrefixNotFound(
                String::from_utf8(vec![path[0]]).unwrap(),
            )) as Box<dyn Error + Send>);
        }

        // returns the index of the first instance of sep in s, or -1 if sep is not present in s.
        let prefix_index = find_index_of_array(path, &f.as_ref().unwrap().prefix);
        if prefix_index.is_none() {
            return Err(Box::new(MantarayNodeError::PathPrefixNotFound(
                String::from_utf8(path.to_vec()).unwrap(),
            )) as Box<dyn Error + Send>);
        }

        let rest = &path[f.as_ref().unwrap().prefix.len()..];
        if rest.is_empty() {
            // full path matched
            self.forks.remove(&path[0]);
            self.ref_ = vec![];
            return Ok(());
        }

        f.unwrap().node.remove(rest, ls).await
    }

    // hasprefix tests whether the node contains prefix path
    #[async_recursion]
    pub async fn has_prefix(
        &mut self,
        path: &[u8],
        l: &mut Option<DynLoaderSaver>,
    ) -> Result<bool> {
        // if path is empty then return false
        if path.is_empty() {
            return Ok(true);
        }

        // if forks is empty then load
        if self.forks.is_empty() {
            self.load(l).await?;
        }

        // if path is not empty then get the fork at the first character of the path
        let fork = self.forks.get_mut(&path[0]);
        if fork.is_none() {
            return Ok(false);
        }

        // returns the index of the first instance of sep in s, or -1 if sep is not present in s.
        let c = common(&fork.as_ref().unwrap().prefix, path);

        // if common prefix is full path then return true
        if c.len() == fork.as_ref().unwrap().prefix.len() {
            return fork.unwrap().node.has_prefix(&path[c.len()..], l).await;
        }

        // determine if a fork prefix begins with the byte slice t.
        if fork.unwrap().prefix.starts_with(path) {
            return Ok(true);
        }

        Ok(false)
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;
    use tokio::sync::Mutex;

    use crate::persist::MockLoadSaver;

    use super::*;
    use test_case::test_case;

    struct TestCase<'a> {
        _name: String,
        items: Vec<&'a str>,
    }

    #[derive(Default, Clone)]
    struct RemoveTestCaseItem {
        path: String,
        metadata: BTreeMap<String, String>,
    }

    #[derive(Default, Clone)]
    struct HasPrefixTestCase {
        _name: String,
        paths: Vec<String>,
        test_paths: Vec<String>,
        should_exist: Vec<bool>,
    }

    #[derive(Clone)]
    struct RemoveTestCase {
        _name: String,
        items: Vec<RemoveTestCaseItem>,
        remove: Vec<String>,
    }

    #[tokio::test]
    async fn nil_path() {
        let mut n = Node::default();
        assert_eq!(n.lookup("".as_bytes(), &mut None).await.is_ok(), true);
    }

    // test data
    fn test_case_data() -> [TestCase<'static>; 6] {
        [
            TestCase {
                _name: "a".to_string(),
                items: vec![
                    "aaaaaa", "aaaaab", "abbbb", "abbba", "bbbbba", "bbbaaa", "bbbaab", "aa", "b",
                ],
            },
            TestCase {
                _name: "simple".to_string(),
                items: vec!["/", "index.html", "img/1.png", "img/2.png", "robots.txt"],
            },
            TestCase {
                _name: "nested-value-node-is-recognized".to_string(),
                items: vec![
                    "..............................@",
                    "..............................",
                ],
            },
            TestCase {
                _name: "nested-prefix-is-not-collapsed".to_string(),
                items: vec![
                    "index.html",
                    "img/1.png",
                    "img/2/test1.png",
                    "img/2/test2.png",
                    "robots.txt",
                ],
            },
            TestCase {
                _name: "conflicting-path".to_string(),
                items: vec!["app.js.map", "app.js"],
            },
            TestCase {
                _name: "spa-website".to_string(),
                items: vec![
                    "css/",
                    "css/app.css",
                    "favicon.ico",
                    "img/",
                    "img/logo.png",
                    "index.html",
                    "js/",
                    "js/chunk-vendors.js.map",
                    "js/chunk-vendors.js",
                    "js/app.js.map",
                    "js/app.js",
                ],
            },
        ]
    }

    fn remove_test_case_data() -> Vec<RemoveTestCase> {
        vec![
            RemoveTestCase {
                _name: "simple".to_string(),
                items: vec![
                    RemoveTestCaseItem {
                        path: "/".to_string(),
                        metadata: serde_json::from_str(
                            r#"{
                            "index-document": "index.html"
                        }"#,
                        )
                        .unwrap(),
                    },
                    RemoveTestCaseItem {
                        path: "index.html".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "img/1.png".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "img/2.png".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "robots.txt".to_string(),
                        ..Default::default()
                    },
                ],
                remove: vec!["img/2.png".to_string()],
            },
            RemoveTestCase {
                _name: "nested-prefix-is-not-collapsed".to_string(),
                items: vec![
                    RemoveTestCaseItem {
                        path: "index.html".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "img/1.png".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "img/2/test1.png".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "img/2/test2.png".to_string(),
                        ..Default::default()
                    },
                    RemoveTestCaseItem {
                        path: "robots.txt".to_string(),
                        ..Default::default()
                    },
                ],
                remove: vec!["img/2/test1.png".to_string()],
            },
        ]
    }

    fn has_prefix_test_case_data() -> Vec<HasPrefixTestCase> {
        vec![
            HasPrefixTestCase {
                _name: "simple".to_string(),
                paths: vec![
                    "index.html".to_string(),
                    "img/1.png".to_string(),
                    "img/2.png".to_string(),
                    "robots.txt".to_string(),
                ],
                test_paths: vec!["img/".to_string(), "images/".to_string()],
                should_exist: vec![true, false],
            },
            HasPrefixTestCase {
                _name: "nested-single".to_string(),
                paths: vec!["some-path/file.ext".to_string()],
                test_paths: vec![
                    "some-path".to_string(),
                    "some-path/file".to_string(),
                    "some-other-path/".to_string(),
                ],
                should_exist: vec![true, true, false],
            },
        ]
    }

    #[tokio::test]
    async fn add_and_lookup() {
        let mut n = Node::default();
        for (i, c) in test_case_data()[0].items.iter().enumerate() {
            // create a vector from the string c zero padded to the left to 32 bytes
            let e = vec![0; 32 - c.len()]
                .iter()
                .chain(c.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(
                n.add(c.as_bytes(), &e, BTreeMap::new(), &mut None)
                    .await
                    .unwrap(),
                ()
            );

            for j in 0..i {
                let d = test_case_data()[0].items[j].as_bytes();
                let r = n.lookup(d, &mut None).await;
                assert_eq!(r.is_ok(), true);
                let de = vec![0; 32 - d.len()]
                    .iter()
                    .chain(d.iter())
                    .cloned()
                    .collect::<Vec<u8>>();
                assert_eq!(r.unwrap(), de);
            }
        }
    }

    #[test_case(test_case_data()[0].items.clone() ; "a")]
    #[test_case(test_case_data()[1].items.clone() ; "simple")]
    #[test_case(test_case_data()[2].items.clone() ; "nested-value-node-is-recognized")]
    #[test_case(test_case_data()[3].items.clone() ; "nested-prefix-is-not-collapsed")]
    #[test_case(test_case_data()[4].items.clone() ; "conflicting-path")]
    #[test_case(test_case_data()[5].items.clone() ; "spa-website")]
    #[tokio::test]
    async fn add_and_lookup_node(tc: Vec<&str>) {
        let mut n = Node::default();

        for (i, c) in tc.iter().enumerate() {
            // create a vector from the string c zero padded to the left to 32 bytes
            let e = vec![0; 32 - c.len()]
                .iter()
                .chain(c.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(
                n.add(c.as_bytes(), &e, BTreeMap::new(), &mut None)
                    .await
                    .unwrap(),
                ()
            );

            for j in 0..i {
                let d = tc[j];
                let node = n.lookup_node(d.as_bytes(), &mut None).await.unwrap();
                assert_eq!(node.is_value_type(), true);
                let de = vec![0; 32 - d.len()]
                    .iter()
                    .chain(d.as_bytes().iter())
                    .cloned()
                    .collect::<Vec<u8>>();
                assert_eq!(node.entry, de);
            }
        }
    }

    #[test_case(test_case_data()[0].items.clone() ; "a")]
    #[test_case(test_case_data()[1].items.clone() ; "simple")]
    #[test_case(test_case_data()[2].items.clone() ; "nested-value-node-is-recognized")]
    #[test_case(test_case_data()[3].items.clone() ; "nested-prefix-is-not-collapsed")]
    #[test_case(test_case_data()[4].items.clone() ; "conflicting-path")]
    #[test_case(test_case_data()[5].items.clone() ; "spa-website")]
    #[tokio::test]
    async fn add_and_lookup_node_with_load_save(tc: Vec<&str>) {
        let mut n = Node::default();

        for c in &tc {
            // create a vector from the string c zero padded to the left to 32 bytes
            let e = vec![0; 32 - c.len()]
                .iter()
                .chain(c.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(
                n.add(c.as_bytes(), &e, BTreeMap::new(), &mut None)
                    .await
                    .unwrap(),
                ()
            );
        }

        let ls = Arc::new(Mutex::new(MockLoadSaver::new()));

        let save = n.save(&Some(Box::new(ls.clone()))).await;
        assert_eq!(save.is_ok(), true);

        let mut n2 = Node::new_node_ref(&n.ref_);

        for d in tc {
            let node = n2
                .lookup_node(d.as_bytes(), &mut Some(Box::new(ls.clone())))
                .await
                .unwrap();
            assert_eq!(node.is_value_type(), true);
            let de = vec![0; 32 - d.len()]
                .iter()
                .chain(d.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(node.entry, de);
        }
    }

    #[test_case(remove_test_case_data()[0].clone() ; "simple")]
    #[test_case(remove_test_case_data()[1].clone() ; "nested-prefix-is-not-collapsed")]
    #[tokio::test]
    async fn test_remove(tc: RemoveTestCase) {
        let mut n = Node::default();
        for (i, c) in tc.items.iter().enumerate() {
            // create a vector from the string c zero padded to the left to 32 bytes
            let e = vec![0; 32 - c.path.len()]
                .iter()
                .chain(c.path.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(
                n.add(c.path.as_bytes(), &e, c.metadata.clone(), &mut None)
                    .await
                    .unwrap(),
                ()
            );

            for j in 0..i {
                let d = &tc.items[j].path;
                let r = n.lookup(d.as_bytes(), &mut None).await;
                assert_eq!(r.is_ok(), true);
                let de = vec![0; 32 - d.len()]
                    .iter()
                    .chain(d.as_bytes().iter())
                    .cloned()
                    .collect::<Vec<u8>>();
                assert_eq!(r.unwrap(), de);
            }
        }

        for c in tc.remove.iter() {
            // create a vector from the string c zero padded to the left to 32 bytes
            assert_eq!(n.remove(c.as_bytes(), &mut None).await.unwrap(), ());

            assert_eq!(n.lookup(c.as_bytes(), &mut None).await.is_err(), true);
        }
    }

    #[test_case(has_prefix_test_case_data()[0].clone() ; "simple")]
    #[test_case(has_prefix_test_case_data()[1].clone() ; "nested-single")]
    #[tokio::test]
    async fn test_has_prefix(tc: HasPrefixTestCase) {
        let mut n = Node::default();

        for c in tc.paths.iter() {
            // create a vector from the string c zero padded to the left to 32 bytes
            let e = vec![0; 32 - c.len()]
                .iter()
                .chain(c.as_bytes().iter())
                .cloned()
                .collect::<Vec<u8>>();
            assert_eq!(
                n.add(c.as_bytes(), &e, Default::default(), &mut None)
                    .await
                    .unwrap(),
                ()
            );
        }

        for (i, test_prefix) in tc.test_paths.iter().enumerate() {
            assert_eq!(
                n.has_prefix(test_prefix.as_bytes(), &mut None)
                    .await
                    .unwrap(),
                tc.should_exist[i]
            );
        }
    }
}
