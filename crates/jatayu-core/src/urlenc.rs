//! Percent-encoding for a URL's query and path, shared by every request
//! builder in this crate (search, maps, weather, the desk's site search).
//!
//! Only encoding lives here. The decoders elsewhere differ in what they do
//! with a stray `%` and with `+`, so each stays beside the code it serves.

/// Percent-encode for a query string or a path segment. Unreserved
/// characters (letters, digits, `-` `_` `.` `~`) survive; everything else,
/// spaces and ampersands very much included, becomes `%XX` of its UTF-8
/// bytes. A space is `%20` rather than `+`, which is right in a path as well
/// as in a query.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_but_the_unreserved_is_escaped_by_byte() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("a b&c"), "a%20b%26c");
        assert_eq!(encode("caf\u{e9}"), "caf%C3%A9");
        assert_eq!(encode("100%/+"), "100%25%2F%2B");
    }
}
