//! The rules the vault applies to connections, over a real SQLite backend:
//! the list round-trips tidied and checked, names stay unique, each
//! connection's secret is kept apart from it, and removing one takes its
//! state and its secret with it.

mod support;
use jatayu_core::connection::{
    AuthMethod, Connection, ConnectionSecret, ConnectionState, Transport,
};
use support::vault;

fn linear() -> Connection {
    Connection::new(
        "  Linear ",
        Transport::Http { url: " https://mcp.linear.app/mcp ".into(), headers: vec![] },
        AuthMethod::OAuth {
            client_id: Some("  ".into()),
            scopes: vec![" read ".into()],
            redirect_port: None,
        },
    )
}

#[test]
fn a_connection_is_saved_tidied_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(vault.connection_settings().unwrap().connections.is_empty());

    let saved = vault.save_connection(linear()).unwrap();
    assert_eq!(saved.name, "Linear");
    assert_eq!(
        saved.transport,
        Transport::Http { url: "https://mcp.linear.app/mcp".into(), headers: vec![] }
    );
    assert_eq!(
        saved.auth,
        AuthMethod::OAuth { client_id: None, scopes: vec!["read".into()], redirect_port: None }
    );
    assert_eq!(vault.connection(saved.id).unwrap(), saved);

    let mut renamed = saved.clone();
    renamed.name = "Work Linear".into();
    let renamed = vault.save_connection(renamed).unwrap();
    assert_eq!(renamed.created_at, saved.created_at, "an edit keeps when it was made");
    assert_eq!(vault.connection_settings().unwrap().connections.len(), 1);
}

#[test]
fn two_connections_cannot_share_a_name() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.save_connection(linear()).unwrap();
    let err = vault.save_connection(linear()).expect_err("a second Linear must be refused");
    assert_eq!(err.code(), "invalid");
}

#[test]
fn an_invalid_connection_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let plain = Connection::new(
        "Somewhere",
        Transport::Http { url: "http://example.com/mcp".into(), headers: vec![] },
        AuthMethod::None,
    );
    assert!(vault.save_connection(plain).is_err(), "a token in the clear to another machine");
}

#[test]
fn a_secret_is_kept_apart_and_removing_the_connection_takes_everything() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let saved = vault.save_connection(linear()).unwrap();
    assert_eq!(vault.connection_secret(saved.id).unwrap(), ConnectionSecret::default());

    let secret = ConnectionSecret { bearer: Some("lin_api_secret".into()), ..Default::default() };
    vault.set_connection_secret(saved.id, &secret).unwrap();
    assert_eq!(vault.connection_secret(saved.id).unwrap(), secret);
    let listed = serde_json::to_string(&vault.connection_settings().unwrap()).unwrap();
    assert!(!listed.contains("lin_api_secret"), "the list never carries a secret");

    vault
        .save_connection_state(
            saved.id,
            ConnectionState { last_error: Some("down".into()), ..Default::default() },
        )
        .unwrap();
    assert!(vault.connection_states().unwrap().states.contains_key(&saved.id));

    vault.remove_connection(saved.id).unwrap();
    assert!(vault.connection(saved.id).is_err());
    assert!(!vault.connection_states().unwrap().states.contains_key(&saved.id));
    assert_eq!(vault.connection_secret(saved.id).unwrap(), ConnectionSecret::default());

    // A refresh finishing after the removal does not bring its state back.
    vault.save_connection_state(saved.id, ConnectionState::default()).unwrap();
    assert!(vault.connection_states().unwrap().states.is_empty());
}

#[test]
fn an_empty_secret_is_deleted_rather_than_stored() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let saved = vault.save_connection(linear()).unwrap();
    let secret = ConnectionSecret { bearer: Some("t".into()), ..Default::default() };
    vault.set_connection_secret(saved.id, &secret).unwrap();
    vault.set_connection_secret(saved.id, &ConnectionSecret::default()).unwrap();
    assert_eq!(vault.connection_secret(saved.id).unwrap(), ConnectionSecret::default());
}
