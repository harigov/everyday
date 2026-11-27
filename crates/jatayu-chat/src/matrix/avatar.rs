//! The bot's picture: Jatayu's wing, the application's mark, so the
//! assistant has a face in the owner's chat app rather than a letter.
//!
//! Given once, and only to an account with no picture: one somebody set on
//! the bot themselves, from any client, is theirs and is left alone. Once
//! given, a mark in the bot's own store says so, and it is never given again
//! -- so a picture the owner later takes away stays away. Nothing waits on
//! it and nothing fails because of it: a server that refuses the upload costs
//! a line in the log, and it is tried again on the next start.
//!
//! The picture is the Android adaptive icon's two layers drawn together
//! (`crates/jatayu-app/icons/icon-android-*.svg`): the wing on its midnight,
//! inside the circle every chat app crops an avatar to.

use matrix_sdk::Client;
use matrix_sdk::ruma::api::error::ErrorKind;

const PICTURE: &[u8] = include_bytes!("../../assets/avatar.png");

/// The mark in the bot's store that the picture has been given.
const GIVEN: &[u8] = b"jatayu.avatar.given";

/// Set the bot's picture if it has none and never had this one, in the
/// background.
pub(super) fn give_if_missing(client: &Client) {
    let client = client.clone();
    tokio::spawn(async move {
        let store = client.state_store();
        if matches!(store.get_custom_value(GIVEN).await, Ok(Some(_))) {
            return;
        }
        let account = client.account();
        let has_one = match account.get_avatar_url().await {
            Ok(url) => url.is_some(),
            // A server is allowed to answer an account with no picture with
            // "not found" rather than an empty one.
            Err(error) if matches!(error.client_api_error_kind(), Some(ErrorKind::NotFound)) => {
                false
            }
            Err(error) => {
                tracing::warn!("could not read the bot's picture: {error}");
                return;
            }
        };
        if !has_one
            && let Err(error) = account.upload_avatar(&mime::IMAGE_PNG, PICTURE.to_vec()).await
        {
            tracing::warn!("could not give the bot its picture: {error}");
            return;
        }
        // Whether given now or already there, it is not to be given again.
        if let Err(error) = store.set_custom_value(GIVEN, vec![1]).await {
            tracing::warn!("could not note that the bot has its picture: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::PICTURE;

    #[test]
    fn the_picture_is_a_png() {
        assert_eq!(&PICTURE[..8], b"\x89PNG\r\n\x1a\n");
    }
}
