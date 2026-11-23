//! The patched `ruma-identifiers-validation` in `vendor/` (see the workspace
//! `Cargo.toml`). Anybody's event can carry these strings, and 0.12.1 as
//! published panics on them; this fails if the patch is dropped while
//! matrix-sdk still pins that line.

use matrix_sdk::ruma::{DeviceKeyId, MxcUri};

#[test]
fn an_mxc_server_name_too_long_to_index_is_refused_not_a_panic() {
    // At 250 bytes 0.12.1 unwrapped a zero; past it, it returned a wrong index.
    for (len, valid) in [(249, true), (250, false), (251, false), (261, false), (300, false)] {
        let uri = format!("mxc://{}/media", "a".repeat(len));
        let mxc = <&MxcUri>::from(uri.as_str());
        assert_eq!(mxc.is_valid(), valid, "a server name of {len} bytes");
        assert_eq!(mxc.parts().is_ok(), valid, "a server name of {len} bytes");
    }
}

#[test]
fn a_key_algorithm_past_255_bytes_parses_not_panics() {
    // A colon at byte 300 wrapped to 44, and 0.12.1 sliced at 45: inside an "é".
    let id = format!("{}:DEVICE", "é".repeat(150));
    let key = DeviceKeyId::parse(id.as_str()).expect("0.14 accepts it too");
    assert_eq!(key.key_name().as_str(), "DEVICE");
    assert_eq!(key.algorithm().as_str(), "é".repeat(150));
}
