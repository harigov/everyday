//! Lowercase hexadecimal, the one spelling of bytes as text this crate
//! writes: key digests in a vault header, content addresses, fingerprints.

use std::fmt::Write as _;

/// `bytes` as two lowercase hex digits each.
pub fn encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
