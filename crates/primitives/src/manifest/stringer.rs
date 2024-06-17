use std::fmt::Display;

use alloy_primitives::hex;

use crate::node::Node;

// const TOP: &str = "─";
const TOP_MID: &str = "┬";
// const TOP_LEFT: &str = "┌";
const TOP_RIGHT: &str = "┐";
const BOTTOM: &str = "─";
// const BOTTOM_MID: &str = "┴";
const BOTTOM_LEFT: &str = "└";
// const BOTTOM_RIGHT: &str = "┘";
// const LEFT: &str = "│";
const LEFT_MID: &str = "├";
const MID: &str = "─";
// const MID_MID: &str = "┼";
// const RIGHT: &str = "│";
// const RIGHT_MID: &str = "┤";
const MIDDLE: &str = "│";

impl Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut buf = String::new();

        buf.push_str(BOTTOM_LEFT);
        buf.push_str(BOTTOM);
        buf.push_str(TOP_RIGHT);
        buf.push('\n');

        to_string_with_prefix(self, "  ", &mut buf);

        write!(f, "{}", buf)
    }
}

fn to_string_with_prefix(n: &Node, prefix: &str, buf: &mut String) {
    buf.push_str(prefix);
    buf.push_str(LEFT_MID);
    buf.push_str(format!("r: '{}'\n", hex::encode(n.ref_.clone())).as_str());
    buf.push_str(prefix);
    buf.push_str(LEFT_MID);
    buf.push_str(format!("t: '{}'", n.node_type).as_str());
    buf.push_str(" [");
    if n.is_value_type() {
        buf.push_str(" Value");
    }
    if n.is_edge_type() {
        buf.push_str(" Edge");
    }
    if n.is_with_path_separator_type() {
        buf.push_str(" PathSeparator");
    }
    buf.push_str(" ]");
    buf.push('\n');
    buf.push_str(prefix);
    if !n.forks.is_empty() || !n.metadata.is_empty() {
        buf.push_str(LEFT_MID);
    } else {
        buf.push_str(BOTTOM_LEFT);
    }
    buf.push_str(format!("e: '{}'\n", hex::encode(n.entry.clone())).as_str());
    if !n.metadata.is_empty() {
        buf.push_str(prefix);
        if !n.forks.is_empty() {
            buf.push_str(LEFT_MID);
        } else {
            buf.push_str(BOTTOM_LEFT);
        }
        buf.push_str(format!("m: '{}'\n", serde_json::to_string(&n.metadata).unwrap()).as_str());
    }

    // get the keys of the forks and sort them.
    let mut keys: Vec<&u8> = n.forks.keys().collect();
    keys.sort();

    for (i, key) in keys.iter().enumerate() {
        let f = n.forks.get(key).unwrap();
        let is_last = i == keys.len() - 1;

        buf.push_str(prefix);
        if is_last {
            buf.push_str(LEFT_MID);
        } else {
            buf.push_str(BOTTOM_LEFT);
        }
        buf.push_str(MID);
        buf.push_str(format!("[{}]", key).as_str());
        buf.push_str(MID);
        buf.push_str(TOP_MID);
        buf.push_str(MID);
        buf.push_str(format!("`{}`\n", String::from_utf8(f.prefix.clone()).unwrap()).as_str());
        let mut new_prefix = String::from(prefix);
        if is_last {
            // add MIDDLE to the new prefix
            new_prefix.push_str(MIDDLE);
        } else {
            new_prefix.push(' ');
        }
        new_prefix.push_str("     ");
        to_string_with_prefix(&f.node, &new_prefix, buf);
    }
}
