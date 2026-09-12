//! Platform paths and the password rule, independent of any open vault.
//!
//! The pointer to the last vault opened is tested beside these in
//! `everyday_vault::tests` rather than here: it is read and written by
//! private functions this crate does not expose, so an integration test
//! cannot reach them.

use everyday_vault::{default_vault_dir, validate_password};

#[test]
fn the_default_vault_directory_is_absolute_and_named() {
    let dir = default_vault_dir();
    assert!(dir.is_absolute(), "got a relative path: {dir:?}");
    // Each platform spells it differently -- `~/.local/share/everyday`,
    // `~/Library/Application Support/app.Every-Day.EveryDay`,
    // `%APPDATA%\\Every Day\\EveryDay` -- so match case-insensitively.
    let s = dir.to_string_lossy().to_lowercase().replace([' ', '-'], "");
    assert!(s.contains("everyday"), "got {dir:?}");
}

#[test]
fn short_passwords_are_rejected() {
    assert!(validate_password("short").is_err());
    assert!(validate_password("").is_err());
    assert!(validate_password("12345678").is_ok());
    // Length is counted in characters, not bytes.
    assert!(validate_password("\u{1f600}\u{1f600}\u{1f600}").is_err());
}
