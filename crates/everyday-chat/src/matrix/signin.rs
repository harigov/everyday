//! Getting a signed-in `matrix_sdk::Client`: log in afresh with the password,
//! or restore the session the last run saved.
//!
//! # The rule: a password means "sign in afresh"
//!
//! `save_chat_link` keeps a link's saved session when the settings form is
//! saved again, and only sends a password when the person typed one -- which
//! they do when the bot needs signing in: after a refused password, an
//! expired token, a lost store. So **a password in the secret is a request to
//! sign in from scratch.** The saved session is not restored, the link's
//! local store is emptied, and the bot logs in as a new device. Only with no
//! password is the saved session restored, and then only over the store it
//! was made with. The connector drops the password again once the new session
//! and the encryption identity are saved.
//!
//! # The rule under it: a session never meets a store it was not made with
//!
//! A Matrix session is a device, and a device's end-to-end keys live in the
//! local store. Restore a session over an empty store and the SDK makes a new
//! set of keys for the old device id; the server then holds one set of keys
//! for that device and the bot another, and every encrypted message to the
//! bot is quietly undecryptable. So a session is restored only when the
//! store the previous run left is there, a session without its store is
//! thrown away, and a new login always starts from an empty store.

use super::errors::{classify, classify_build};
use crate::connector::{ChatError, Setup};
use everyday_core::chat::{ChatSecret, MatrixLink};
use matrix_sdk::Client;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::cross_process_lock::CrossProcessLockConfig;
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use std::path::Path;
use std::time::Duration;

/// What the bot's login shows up as in the owner's list of devices.
const DEVICE_NAME: &str = "Every Day assistant";

/// Every file `matrix-sdk`'s SQLite stores write starts with this
/// (`matrix-sdk-crypto.sqlite3`, `matrix-sdk-state.sqlite3-wal`, ...).
const STORE_FILE_PREFIX: &str = "matrix-sdk-";

/// The crypto store's file, whose presence is what makes a store "the one the
/// saved session was made with". Matched by prefix so a rename within the SDK
/// still counts.
const CRYPTO_STORE_PREFIX: &str = "matrix-sdk-crypto";

/// How long to let a rejected session's background encryption work finish
/// before its store is left alone.
const REJECTED_CLIENT_PATIENCE: Duration = Duration::from_secs(10);

/// A client that is signed in.
pub(super) struct SignedIn {
    pub client: Client,
    /// The bot's id as the server spells it, which is what event senders
    /// will be compared with.
    pub bot: OwnedUserId,
    /// The session this run made by logging in, as it should be saved in the
    /// secret. `None` when the saved session was restored, which is already
    /// saved.
    pub new_session: Option<serde_json::Value>,
}

/// How restoring a saved session failed.
enum RestoreFailure {
    /// The session cannot be used -- the server rejected it, it names another
    /// account, or it does not fit the store. Carries the sentence for
    /// Settings, which ends by asking for the password.
    Unusable(String),
    /// Something that is not about the session: try again later.
    Failed(ChatError),
}

/// Sign in, by the rule in the module doc: with the password if the secret
/// holds one, by restoring the saved session if not.
///
/// Writes through `setup.save_secret` only to forget a saved session that
/// turned out to be unusable, so that Settings stops saying the bot is signed
/// in. A new login's session is *returned*, not saved: the caller saves it
/// together with the encryption identity and the cleared password, in one
/// write, so a crash in between leaves the password in place and the next run
/// simply logs in again.
pub(super) async fn sign_in(
    setup: &Setup,
    link: &MatrixLink,
    secret: &mut ChatSecret,
) -> Result<SignedIn, ChatError> {
    let wanted = UserId::parse(&link.user_id).map_err(|e| {
        ChatError::NeedsSignIn(format!(
            "\"{}\" is not a Matrix ID the bot can sign in as ({e}). Check it in Settings.",
            link.user_id
        ))
    })?;

    if let Some(password) = secret.password.clone() {
        return log_in(setup, link, &wanted, &password).await;
    }
    restore_saved(setup, link, &wanted, secret).await
}

/// Log in with the password as a new device, over an emptied store.
async fn log_in(
    setup: &Setup,
    link: &MatrixLink,
    wanted: &UserId,
    password: &str,
) -> Result<SignedIn, ChatError> {
    // The task is the only user of this directory, and a new login is a new
    // device, which needs a new store -- see the module doc.
    wipe_store(&setup.state_dir)?;
    let client = build_client(setup, link).await?;
    let response = client
        .matrix_auth()
        .login_username(wanted.as_str(), password)
        .initial_device_display_name(DEVICE_NAME)
        .send()
        .await
        .map_err(|e| classify(e.client_api_error_kind(), true, "sign in", &e))?;
    if !same_account(response.user_id.as_str(), wanted.as_str()) {
        return Err(ChatError::NeedsSignIn(format!(
            "The server signed in as {}, not {wanted}. Check the bot's Matrix ID in Settings.",
            response.user_id
        )));
    }
    let session = serde_json::to_value(MatrixSession::from(&response))
        .map_err(|e| ChatError::Transient(format!("Couldn't keep the bot's sign-in: {e}")))?;
    tracing::info!(bot = %response.user_id, device = %response.device_id, "signed in to Matrix");
    Ok(SignedIn { client, bot: response.user_id, new_session: Some(session) })
}

/// Restore the session the last run saved, over the store it was made with.
///
/// With nothing usable to restore there is nothing to do but ask for the
/// password: the session is forgotten (here and in the saved secret) and the
/// error says why.
async fn restore_saved(
    setup: &Setup,
    link: &MatrixLink,
    wanted: &UserId,
    secret: &mut ChatSecret,
) -> Result<SignedIn, ChatError> {
    let Some(saved) = secret.session.clone() else {
        return Err(ChatError::NeedsSignIn(
            "Enter the bot's password in Settings to sign it in.".into(),
        ));
    };
    let session = match usable_session(&saved, wanted, store_exists(&setup.state_dir)) {
        Ok(session) => session,
        Err(reason) => {
            tracing::warn!(bot = %link.user_id, "discarding the saved Matrix session: {reason}");
            forget_session(setup, secret)?;
            return Err(ChatError::NeedsSignIn(cannot_restore(&reason)));
        }
    };

    let client = build_client(setup, link).await?;
    match restore(&client, session, wanted).await {
        Ok(bot) => Ok(SignedIn { client, bot, new_session: None }),
        Err(RestoreFailure::Failed(error)) => Err(error),
        Err(RestoreFailure::Unusable(sentence)) => {
            tracing::warn!(bot = %link.user_id, "the saved Matrix session was refused: {sentence}");
            retire(client).await;
            forget_session(setup, secret)?;
            Err(ChatError::NeedsSignIn(sentence))
        }
    }
}

/// Drop the saved session from the secret, and write that down.
fn forget_session(setup: &Setup, secret: &mut ChatSecret) -> Result<(), ChatError> {
    secret.session = None;
    (setup.save_secret)(secret)
}

/// The sentence for a session that cannot be restored, whatever the reason.
fn cannot_restore(reason: &str) -> String {
    format!(
        "The bot's saved sign-in can't be used: {reason}. Enter its password in Settings to sign \
         it in again."
    )
}

/// The saved session, if it is one this run may restore.
///
/// It must parse, belong to the account the link names, and have the store it
/// was made with; the `Err` says which of those failed.
fn usable_session(
    saved: &serde_json::Value,
    wanted: &UserId,
    store_was_there: bool,
) -> Result<MatrixSession, String> {
    let session: MatrixSession =
        serde_json::from_value(saved.clone()).map_err(|e| format!("it could not be read ({e})"))?;
    if !same_account(session.meta.user_id.as_str(), wanted.as_str()) {
        return Err(format!("it is for {}, not {wanted}", session.meta.user_id));
    }
    if !store_was_there {
        return Err("its local encryption store is gone".into());
    }
    Ok(session)
}

/// Whether two Matrix ids name the same account as far as this check goes.
///
/// A server lower-cases the local part it was given, so a bot typed into
/// Settings as `@Bot:example.org` signs in as `@bot:example.org`, and treating
/// that as a different account would discard a good session on every start.
/// This is only used to notice the wrong account, never to decide who may talk
/// to the assistant: that comparison is exact.
fn same_account(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Build a client for the link, opening its store.
async fn build_client(setup: &Setup, link: &MatrixLink) -> Result<Client, ChatError> {
    // The store holds the bot's end-to-end keys. Never open it unprotected
    // because something upstream forgot the passphrase.
    if setup.store_passphrase.is_empty() {
        return Err(ChatError::Transient(
            "The chat store has no passphrase, so the bot's keys cannot be kept safely.".into(),
        ));
    }
    Client::builder()
        // Used exactly as given: the person running the server knows where it
        // is, and a bot beside it should not depend on its public name
        // resolving. No `.well-known` lookup now, and none when logging in
        // that could move the client somewhere else.
        .homeserver_url(&link.homeserver)
        .disable_well_known_lookup(true)
        .respect_login_well_known(false)
        .sqlite_store(&setup.state_dir, Some(setup.store_passphrase.as_str()))
        .with_encryption_settings(EncryptionSettings {
            // Cross-signing and key backup are what make Element show the bot
            // as verified, and have clients that hold keys back from
            // unverified devices share them with it. See `identity`.
            auto_enable_cross_signing: true,
            auto_enable_backups: true,
            ..Default::default()
        })
        // Nothing else opens this store, and the lock a second process would
        // need only gets in the way of a restart that begins before the last
        // client has finished dropping.
        .cross_process_store_config(CrossProcessLockConfig::SingleProcess)
        .build()
        .await
        .map_err(classify_build)
}

/// Restore `session` onto a client built over the store it was made with, and
/// ask the server whether it still stands.
///
/// Returns the bot's id as the server spells it.
async fn restore(
    client: &Client,
    session: MatrixSession,
    wanted: &UserId,
) -> Result<OwnedUserId, RestoreFailure> {
    // Failing here is local -- the store does not match the session -- so
    // there is nothing to retry: a fresh login is the cure.
    client.restore_session(session).await.map_err(|e| {
        RestoreFailure::Unusable(cannot_restore(&format!("it does not fit its store ({e})")))
    })?;

    // Restoring touches no network. Without this the first anybody would know
    // of a dead token is the first sync failing, with the same cure but a
    // more confusing way of getting there.
    match client.whoami().await {
        Ok(me) if same_account(me.user_id.as_str(), wanted.as_str()) => Ok(me.user_id),
        Ok(me) => Err(RestoreFailure::Unusable(cannot_restore(&format!(
            "it is for {}, not {wanted}",
            me.user_id
        )))),
        Err(error) => {
            let kind = error.client_api_error_kind();
            match classify(kind, false, "check the bot's sign-in", &error) {
                ChatError::NeedsSignIn(sentence) => Err(RestoreFailure::Unusable(sentence)),
                other => Err(RestoreFailure::Failed(other)),
            }
        }
    }
}

/// Let go of a client whose session was rejected.
///
/// Restoring a session starts end-to-end encryption housekeeping in the
/// background; it fails at once against a dead token, but it holds the client
/// (and so the store's files) until it does.
async fn retire(client: Client) {
    let _ = tokio::time::timeout(
        REJECTED_CLIENT_PATIENCE,
        client.encryption().wait_for_e2ee_initialization_tasks(),
    )
    .await;
    drop(client);
}

/// Whether `dir` already holds a crypto store, left by an earlier run.
///
/// Opening a client creates the store, so this must be asked *before*
/// building one.
fn store_exists(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(CRYPTO_STORE_PREFIX))
    })
}

/// Delete the SDK's store files from `dir`, so a new login starts clean.
///
/// Only files the SDK names itself are touched; the directory stays.
fn wipe_store(dir: &Path) -> Result<(), ChatError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(ChatError::Transient(format!(
                "Couldn't look at the bot's saved data: {e}"
            )));
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(STORE_FILE_PREFIX) && entry.path().is_file() {
            std::fs::remove_file(entry.path()).map_err(|e| {
                ChatError::Transient(format!("Couldn't clear the bot's old saved data: {e}"))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::SessionMeta;
    use matrix_sdk::SessionTokens;
    use matrix_sdk::ruma::{owned_device_id, owned_user_id};

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    fn session_for(user: OwnedUserId) -> serde_json::Value {
        let session = MatrixSession {
            meta: SessionMeta { user_id: user, device_id: owned_device_id!("DEVICE") },
            tokens: SessionTokens { access_token: "token".into(), refresh_token: None },
        };
        serde_json::to_value(session).unwrap()
    }

    #[test]
    fn an_empty_directory_has_no_store() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!store_exists(dir.path()));
        assert!(!store_exists(&dir.path().join("missing")));
    }

    #[test]
    fn a_crypto_store_file_means_the_store_was_there() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "matrix-sdk-state.sqlite3");
        assert!(!store_exists(dir.path()), "a state store alone is not the keys");
        touch(dir.path(), "matrix-sdk-crypto.sqlite3");
        assert!(store_exists(dir.path()));
    }

    #[test]
    fn wiping_removes_the_stores_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "matrix-sdk-crypto.sqlite3",
            "matrix-sdk-crypto.sqlite3-wal",
            "matrix-sdk-state.sqlite3",
            "notes.txt",
        ] {
            touch(dir.path(), name);
        }
        wipe_store(dir.path()).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt"]);
        assert!(wipe_store(&dir.path().join("missing")).is_ok());
    }

    #[test]
    fn a_session_is_usable_only_for_its_own_account_with_its_own_store() {
        let bot = owned_user_id!("@bot:example.org");
        let saved = session_for(bot.clone());

        assert!(usable_session(&saved, &bot, true).is_ok());
        // The server spells the account in lower case; Settings may not.
        assert!(usable_session(&saved, &owned_user_id!("@BOT:example.org"), true).is_ok());

        let no_store = usable_session(&saved, &bot, false).unwrap_err();
        assert!(no_store.contains("store"), "{no_store}");

        let other =
            usable_session(&saved, &owned_user_id!("@other:example.org"), true).unwrap_err();
        assert!(other.contains("@other:example.org"), "{other}");

        let garbage = usable_session(&serde_json::json!({"nope": 1}), &bot, true).unwrap_err();
        assert!(garbage.contains("could not be read"), "{garbage}");
    }

    #[test]
    fn the_sentence_for_an_unusable_session_asks_for_the_password() {
        let sentence = cannot_restore("its local encryption store is gone");
        assert!(sentence.contains("store is gone"), "{sentence}");
        assert!(sentence.contains("password in Settings"), "{sentence}");
    }
}
