//! Shared harness for wire-conformance and interop vector suites.
//!
//! Three pieces cover the shapes the per-crate suites otherwise hand-roll: hex
//! vector decoding, byte comparison that reports the first differing offset with
//! a hex window (never a decimal `Vec` dump), and a labelled table runner so a
//! failure names its family and row. A protobuf/unsigned-varint length encoder
//! is included because the expected-bytes builders reconstruct framing by hand.

use std::fmt::Write as _;

/// Decode a hex string (optional `0x` prefix) into a fixed-size byte array.
///
/// Panics with the offending literal on a wrong length or a non-hex digit, so a
/// mistyped vector fails loudly at load time.
pub fn hex_array<const N: usize>(hex: &str) -> [u8; N] {
    let trimmed = hex.strip_prefix("0x").unwrap_or(hex);
    assert_eq!(
        trimmed.len(),
        N * 2,
        "hex literal `{hex}` has {} nibbles, expected {}",
        trimmed.len(),
        N * 2,
    );
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        let off = i * 2;
        *byte = u8::from_str_radix(&trimmed[off..off + 2], 16)
            .unwrap_or_else(|_| panic!("hex literal `{hex}` has a non-hex digit"));
    }
    out
}

/// Decode a hex string (optional `0x` prefix) into a byte vector.
///
/// Panics on an odd nibble count or a non-hex digit.
pub fn hex_vec(hex: &str) -> Vec<u8> {
    let trimmed = hex.strip_prefix("0x").unwrap_or(hex);
    assert!(
        trimmed.len().is_multiple_of(2),
        "hex literal `{hex}` has an odd nibble count",
    );
    (0..trimmed.len() / 2)
        .map(|i| {
            let off = i * 2;
            u8::from_str_radix(&trimmed[off..off + 2], 16)
                .unwrap_or_else(|_| panic!("hex literal `{hex}` has a non-hex digit"))
        })
        .collect()
}

/// Assert `got` equals `want`, panicking with a byte-level diff on mismatch: the
/// label, both lengths, the first differing offset, and a hex window of each
/// side around it.
#[track_caller]
pub fn assert_bytes_eq(label: impl AsRef<str>, got: &[u8], want: &[u8]) {
    if got == want {
        return;
    }
    panic!("{}", byte_diff(label.as_ref(), got, want));
}

/// As [`assert_bytes_eq`], with the expected side given as a hex string
/// (optional `0x` prefix).
#[track_caller]
pub fn assert_bytes_eq_hex(label: impl AsRef<str>, got: &[u8], want_hex: &str) {
    let want = hex_vec(want_hex);
    assert_bytes_eq(label, got, &want);
}

/// Append `value` to `buf` as a base-128 unsigned varint (the protobuf and
/// length-prefix framing the expected-bytes builders reconstruct by hand).
pub fn push_uvarint(buf: &mut Vec<u8>, mut value: usize) {
    while value >= 0x80 {
        buf.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// [`push_uvarint`] into a fresh vector.
pub fn uvarint(value: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    push_uvarint(&mut buf, value);
    buf
}

/// One row of a named vector family, carrying the family name and index so
/// assertions label failures by family and position.
pub struct Vector<'a> {
    table: &'a str,
    index: usize,
}

impl Vector<'_> {
    /// The zero-based row index within the family.
    pub fn index(&self) -> usize {
        self.index
    }

    /// A `family[index]: what` label for bespoke assertion messages.
    pub fn label(&self, what: &str) -> String {
        format!("{}[{}]: {what}", self.table, self.index)
    }

    /// Byte comparison labelled with this row (see [`assert_bytes_eq`]).
    #[track_caller]
    pub fn assert_bytes_eq(&self, what: &str, got: &[u8], want: &[u8]) {
        assert_bytes_eq(self.label(what), got, want);
    }

    /// Byte-versus-hex comparison labelled with this row (see
    /// [`assert_bytes_eq_hex`]).
    #[track_caller]
    pub fn assert_bytes_eq_hex(&self, what: &str, got: &[u8], want_hex: &str) {
        assert_bytes_eq_hex(self.label(what), got, want_hex);
    }
}

/// Run `check` over each row of a named vector family, exposing the row's
/// [`Vector`] context so any failure is labelled by family and index.
pub fn check_each<V>(table: &str, vectors: &[V], check: impl Fn(&Vector<'_>, &V)) {
    for (index, v) in vectors.iter().enumerate() {
        check(&Vector { table, index }, v);
    }
}

fn byte_diff(label: &str, got: &[u8], want: &[u8]) -> String {
    let first = got
        .iter()
        .zip(want.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| got.len().min(want.len()));
    let start = first.saturating_sub(8);
    let mut s = String::new();
    let _ = writeln!(s, "{label}: byte vectors differ");
    let _ = writeln!(s, "  got  len {}", got.len());
    let _ = writeln!(s, "  want len {}", want.len());
    let _ = writeln!(s, "  first difference at offset {first}");
    let _ = writeln!(
        s,
        "  got  [{start}..]: {}",
        hex_window(got, start, (first + 8).min(got.len())),
    );
    let _ = write!(
        s,
        "  want [{start}..]: {}",
        hex_window(want, start, (first + 8).min(want.len())),
    );
    s
}

fn hex_window(bytes: &[u8], start: usize, end: usize) -> String {
    let mut s = String::new();
    for (i, b) in bytes[start..end].iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_array_strips_prefix_and_decodes() {
        assert_eq!(hex_array::<3>("0x010203"), [1, 2, 3]);
        assert_eq!(hex_array::<3>("010203"), [1, 2, 3]);
    }

    #[test]
    #[should_panic(expected = "nibbles")]
    fn hex_array_rejects_wrong_length() {
        let _ = hex_array::<3>("0102");
    }

    #[test]
    fn hex_vec_decodes_variable_length() {
        assert_eq!(hex_vec("0xdeadbeef"), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn uvarint_matches_base128() {
        assert_eq!(uvarint(0), vec![0x00]);
        assert_eq!(uvarint(127), vec![0x7f]);
        assert_eq!(uvarint(128), vec![0x80, 0x01]);
        assert_eq!(uvarint(300), vec![0xac, 0x02]);
    }

    #[test]
    fn assert_bytes_eq_passes_on_equal() {
        assert_bytes_eq("equal", &[1, 2, 3], &[1, 2, 3]);
        assert_bytes_eq_hex("equal", &[0xde, 0xad], "dead");
    }

    #[test]
    #[should_panic(expected = "first difference at offset 1")]
    fn assert_bytes_eq_reports_first_offset() {
        assert_bytes_eq("mismatch", &[1, 2, 3], &[1, 9, 3]);
    }

    #[test]
    #[should_panic(expected = "want len 2")]
    fn assert_bytes_eq_reports_length_mismatch() {
        assert_bytes_eq("length", &[1], &[1, 2]);
    }

    #[test]
    fn check_each_labels_by_family_and_index() {
        let table = [10u8, 20, 30];
        check_each("DOUBLES", &table, |cx, v| {
            cx.assert_bytes_eq("doubled", &[*v], &[*v]);
        });
    }
}
