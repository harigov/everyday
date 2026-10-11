//! The bot's end-to-end encryption identity: cross-signing keys, a recovery
//! key, and a device the account itself vouches for.
//!
//! None of this is needed for the bot to read and write encrypted messages.
//! It is what makes the bot *look* trustworthy to the owner's clients: Element
//! shows a session as verified only when its account has cross-signed it, and
//! some clients (and some settings) will not share message keys with a session
//! that is not. So the bot sets it up, and keeps the one secret it cannot
//! rebuild -- the recovery key -- in its [`ChatSecret`], so that a bot whose
//! local store was lost can sign in as a new device and still be the same
//! verified bot.
//!
//! Nothing here is fatal. A server that will not take the keys, a recovery
//! key that no longer fits, a timeout: each is logged and handed back as a
//! sentence for the status line, and the bot carries on unverified.
//!
//! The flow, in the order it matters:
//!
//! 1. **Let the SDK finish its own setup.** Logging in with a password makes
//!    the SDK create cross-signing keys in the background (it has the
//!    password, which the server asks for again to accept them) and then
//!    set up key backup. Doing it a second time at the same moment would race
//!    it, so wait.
//! 2. **If the account still has no cross-signing keys here, make them.** The
//!    server asks for the password (user-interactive auth); a bot that was
//!    restored rather than logged in no longer has one, and the sentence says
//!    so.
//! 3. **If this device is not yet signed by the account**, sign it with the
//!    keys held here, or, when another session holds the keys, recover them
//!    with the saved recovery key.
//! 4. **If there is no recovery key yet**, create one and save it.

use everyday_core::chat::ChatSecret;
use matrix_sdk::Client;
use matrix_sdk::encryption::Encryption;
use matrix_sdk::ruma::UserId;
use matrix_sdk::ruma::api::client::uiaa::{
    AuthData, MatrixUserIdentifier, Password, UserIdentifier,
};
use std::time::Duration;

/// How long to wait for the SDK's own background setup after signing in.
const SDK_SETUP_PATIENCE: Duration = Duration::from_secs(60);

/// How long all of this may take before the bot gets on with its job.
const TOTAL_PATIENCE: Duration = Duration::from_secs(120);

/// Set up the encryption identity, as far as it can be.
///
/// `secret.password` is used if the server asks for it, and
/// `secret.recovery_key` is read and, when a key is made, written; the caller
/// saves the secret. Returns a sentence about whatever is not as it should be,
/// for the status line, or `None` when all is well.
pub(super) async fn set_up(
    client: &Client,
    bot: &UserId,
    secret: &mut ChatSecret,
) -> Option<String> {
    match tokio::time::timeout(TOTAL_PATIENCE, work(client, bot, secret)).await {
        Ok(problems) => {
            if !problems.is_empty() {
                tracing::warn!(bot = %bot, "encryption identity: {}", problems.join(" "));
            }
            (!problems.is_empty()).then(|| problems.join(" "))
        }
        Err(_) => {
            tracing::warn!(bot = %bot, "setting up the encryption identity timed out");
            Some(
                "Setting up the bot's encryption took too long; Element may show it as \
                 unverified."
                    .into(),
            )
        }
    }
}

/// Do the steps in the module doc; the sentences for whatever failed.
async fn work(client: &Client, bot: &UserId, secret: &mut ChatSecret) -> Vec<String> {
    let encryption = client.encryption();
    let mut problems = Vec::new();

    // 1. Whatever the SDK is already doing in the background.
    let _ =
        tokio::time::timeout(SDK_SETUP_PATIENCE, encryption.wait_for_e2ee_initialization_tasks())
            .await;

    // 2. Cross-signing keys.
    if !holds_cross_signing_keys(&encryption).await
        && let Err(problem) =
            create_cross_signing_keys(&encryption, bot, secret.password.as_deref()).await
    {
        problems.push(problem);
    }
    let holds_keys = holds_cross_signing_keys(&encryption).await;

    // 3. This device, signed by the account.
    if !device_is_signed(&encryption).await {
        if holds_keys {
            if let Err(problem) = sign_this_device(&encryption).await {
                problems.push(problem);
            }
        } else if let Some(key) = secret.recovery_key.clone() {
            // Another session of this account holds the keys; the recovery
            // key fetches them and signs this device with them.
            match encryption.recovery().recover(&key).await {
                Ok(()) => tracing::info!(bot = %bot, "recovered the bot's encryption keys"),
                Err(e) => problems.push(format!(
                    "The bot's saved recovery key was not accepted, so it is unverified ({e})."
                )),
            }
        } else {
            problems.push(
                "The bot's account already has encryption keys held by another session, and no \
                 recovery key is saved for it, so Element will show this session as unverified. \
                 Messages still work."
                    .into(),
            );
        }
    }

    // 4. A recovery key, for the day the local store is lost.
    if secret.recovery_key.is_none() && holds_cross_signing_keys(&encryption).await {
        match encryption.recovery().enable().await {
            Ok(key) => {
                tracing::info!(bot = %bot, "created a recovery key for the bot");
                secret.recovery_key = Some(key);
            }
            Err(e) => problems.push(format!(
                "A recovery key could not be created for the bot, so a lost store would leave it \
                 unverified ({e})."
            )),
        }
    }

    problems
}

/// Whether the private cross-signing keys are all in this client's store.
async fn holds_cross_signing_keys(encryption: &Encryption) -> bool {
    encryption.cross_signing_status().await.is_some_and(|status| status.is_complete())
}

/// Whether this device carries the account's signature.
async fn device_is_signed(encryption: &Encryption) -> bool {
    matches!(
        encryption.get_own_device().await,
        Ok(Some(device)) if device.is_cross_signed_by_owner()
    )
}

/// Sign this device with the cross-signing keys held here.
async fn sign_this_device(encryption: &Encryption) -> Result<(), String> {
    let device = match encryption.get_own_device().await {
        Ok(Some(device)) => device,
        Ok(None) => {
            return Err("The bot's own device is not known yet, so it is unverified.".into());
        }
        Err(e) => return Err(format!("The bot's own device could not be read ({e}).")),
    };
    device
        .verify()
        .await
        .map_err(|e| format!("The bot's session could not be verified by its account ({e})."))
}

/// Make the account's cross-signing keys, if it has none, answering the
/// server's request for the password.
///
/// Succeeds, without making anything, when the account already has keys --
/// held by some other session, which is then step 3's business.
async fn create_cross_signing_keys(
    encryption: &Encryption,
    bot: &UserId,
    password: Option<&str>,
) -> Result<(), String> {
    // The first try is without authentication: a server that does not insist
    // takes the keys at once, and one that does says how.
    let Err(error) = encryption.bootstrap_cross_signing_if_needed(None).await else {
        return Ok(());
    };
    let Some(info) = error.as_uiaa_response() else {
        return Err(format!("The bot's encryption keys could not be created ({error})."));
    };
    let Some(password) = password else {
        return Err("The server wants the bot's password to create its encryption keys. Enter it \
                    in Settings to try again."
            .into());
    };
    let mut auth = Password::new(
        UserIdentifier::Matrix(MatrixUserIdentifier::new(bot.to_string())),
        password.to_owned(),
    );
    auth.session = info.session.clone();
    encryption
        .bootstrap_cross_signing(Some(AuthData::Password(auth)))
        .await
        .map_err(|e| format!("The bot's encryption keys could not be created ({e})."))
}
