//! What the transport's headers should say, derived from the body.
//!
//! The Streamable HTTP binding mirrors three fields of the JSON-RPC body
//! into headers — `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` — and
//! must refuse a request where the header disagrees with the body. That
//! validation needs an HTTP layer (it reads real headers out of a real
//! request), so it cannot live here. What *can* live here is working out
//! what the headers are supposed to say, which is pure function of the
//! JSON body and belongs with the rest of the protocol's knowledge rather
//! than duplicated in `everyday-server`.
//!
//! This keeps the mismatch check itself honest, too: the binding compares
//! two values it was handed rather than re-deriving one of them from the
//! body in its own way, which is exactly the kind of place two
//! implementations of "the same" rule quietly drift apart.

use std::borrow::Cow;

use serde_json::Value;

/// The header values a request body implies.
///
/// Any field that is `None` means the body does not constrain that header
/// — most often because the message is legacy and simply has no per-request
/// `_meta` to read a protocol version from. A binding should only compare
/// fields that are `Some`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExpectedHeaders {
    /// What `MCP-Protocol-Version` should say, from
    /// `params._meta["io.modelcontextprotocol/protocolVersion"]`.
    pub protocol_version: Option<String>,
    /// What `Mcp-Method` should say: the JSON-RPC `method`, whenever the
    /// body has one at all.
    pub method: Option<String>,
    /// What `Mcp-Name` should say. Only set for methods that name a
    /// specific thing they act on — for us, `tools/call`'s `params.name`.
    pub name: Option<String>,
}

/// Work out what a request's headers should say, from its body alone.
///
/// Malformed bodies are not this function's problem: given something that
/// is not a well-formed request it simply finds nothing to require, and
/// [`handle`](crate::handle) will separately reject the body on its own
/// terms with `-32600`.
pub fn expected_headers(message: &Value) -> ExpectedHeaders {
    let method = message.get("method").and_then(Value::as_str).map(str::to_string);

    let params = message.get("params");

    let protocol_version =
        params.and_then(|p| crate::meta_str(p, crate::META_PROTOCOL_VERSION)).map(str::to_string);

    let name = match (method.as_deref(), params) {
        (Some(method), Some(params)) => crate::call_target(method, params).map(str::to_string),
        _ => None,
    };

    ExpectedHeaders { protocol_version, method, name }
}

/// Undo the `=?base64?<b64>?=` sentinel a header value may arrive wrapped
/// in, so a name containing characters a header cannot carry safely
/// (newlines, non-ASCII) can still be compared.
///
/// Anything that is not that exact wrapper, or that fails to decode as
/// base64, or that does not decode to valid UTF-8, is returned unchanged.
/// There is no error path here on purpose: a header we cannot decode is
/// simply compared literally against the expected value, which fails the
/// comparison rather than the request parse. That is the right failure —
/// a mismatch a caller can read about, not a second reason to reject.
pub fn decode_header_value(raw: &str) -> Cow<'_, str> {
    const PREFIX: &str = "=?base64?";
    const SUFFIX: &str = "?=";

    let Some(inner) = raw.strip_prefix(PREFIX).and_then(|s| s.strip_suffix(SUFFIX)) else {
        return Cow::Borrowed(raw);
    };

    match base64_decode(inner).and_then(|bytes| String::from_utf8(bytes).ok()) {
        Some(decoded) => Cow::Owned(decoded),
        None => Cow::Borrowed(raw),
    }
}

/// A minimal standard-alphabet base64 decoder.
///
/// Header decoding is the only place this crate needs base64, and it is
/// a dozen lines against a whole dependency — pulling in the `base64`
/// crate for one call site would be a stranger trade than writing it,
/// especially for a crate whose whole point is to carry as little as
/// possible. Padding (`=`) is tolerated but not required.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let input = input.trim_end_matches('=');
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input.bytes() {
        let v = value(byte)?;
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_wrapped_form() {
        // "task list" base64-encoded.
        assert_eq!(decode_header_value("=?base64?dGFzayBsaXN0?="), "task list");
    }

    #[test]
    fn leaves_plain_values_alone() {
        assert_eq!(decode_header_value("tools/call"), "tools/call");
    }

    #[test]
    fn falls_back_on_bad_base64() {
        assert_eq!(
            decode_header_value("=?base64?not-valid-base64!?="),
            "=?base64?not-valid-base64!?="
        );
    }
}
