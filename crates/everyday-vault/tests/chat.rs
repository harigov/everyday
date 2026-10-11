//! The rules the vault applies to chat links, over a real SQLite backend: the
//! links round-trip, and what a link signs in with is kept apart from it and
//! can be forgotten.

mod support;
use everyday_core::chat::{ChatLink, ChatSecret, ChatService, ChatSettings, MatrixLink};
use support::vault;

fn link() -> ChatLink {
    ChatLink {
        service: ChatService::Matrix(MatrixLink {
            homeserver: "http://127.0.0.1:6167".into(),
            user_id: "@everyday:example.org".into(),
            owner: "@me:example.org".into(),
        }),
        ..ChatLink::new_matrix()
    }
}

#[test]
fn chat_links_round_trip_through_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert_eq!(vault.chat_settings().unwrap(), ChatSettings::default());
    let settings = ChatSettings { links: vec![link()] };
    vault.save_chat_settings(&settings).unwrap();
    assert_eq!(vault.chat_settings().unwrap(), settings);
}

#[test]
fn a_chat_secret_round_trips_defaults_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let link = link();
    assert_eq!(vault.chat_secret(link.id).unwrap(), ChatSecret::default());

    let secret = ChatSecret {
        password: Some("hunter2".into()),
        session: Some(serde_json::json!({ "access_token": "t" })),
        recovery_key: Some("EsT0 abcd".into()),
    };
    vault.save_chat_secret(link.id, &secret).unwrap();
    assert_eq!(vault.chat_secret(link.id).unwrap(), secret);

    vault.save_chat_secret(link.id, &ChatSecret::default()).unwrap();
    assert_eq!(vault.chat_secret(link.id).unwrap(), ChatSecret::default());

    vault.save_chat_secret(link.id, &secret).unwrap();
    vault.delete_chat_secret(link.id).unwrap();
    assert_eq!(vault.chat_secret(link.id).unwrap(), ChatSecret::default());
    // "Make sure this is gone" does not have to check first.
    vault.delete_chat_secret(link.id).unwrap();
}
