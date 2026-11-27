//! The bot's picture: Jatayu's wing, the application's mark, so the
//! assistant has a face in the owner's chat app rather than a letter.
//!
//! Given once, and only to an account with no picture at all: one somebody
//! set on the bot themselves, from any client, is theirs and is left alone,
//! and a picture this gave on an earlier start is a picture, so it is not
//! uploaded again. Nothing waits on it and nothing fails because of it -- a
//! server that refuses the upload costs a line in the log.
//!
//! The picture is the Android adaptive icon's two layers drawn together
//! (`crates/jatayu-app/icons/icon-android-*.svg`): the wing on its midnight,
//! inside the circle every chat app crops an avatar to.

use matrix_sdk::Client;

const PICTURE: &[u8] = include_bytes!("../../assets/avatar.png");

/// Set the bot's picture if it has none, in the background.
pub(super) fn give_if_missing(client: &Client) {
    let client = client.clone();
    tokio::spawn(async move {
        let account = client.account();
        match account.get_avatar_url().await {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = account.upload_avatar(&mime::IMAGE_PNG, PICTURE.to_vec()).await
                {
                    tracing::warn!("could not give the bot its picture: {error}");
                }
            }
            Err(error) => tracing::warn!("could not read the bot's picture: {error}"),
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
