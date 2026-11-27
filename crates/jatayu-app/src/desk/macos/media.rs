//! The music, through the keyboard's own media keys.
//!
//! A Mac has no public way to ask what is playing, or to tell a particular
//! player what to do: `MediaRemote`, which the Control Centre widget and the
//! keys use, is private, and from macOS 15.4 it answers only Apple's own
//! programs. What every Mac does have is the play, next and previous keys,
//! which the system sends to whatever player last had the media -- Music,
//! Spotify, a browser tab -- and that is what "pause the music" means. So the
//! key is pressed, as a person's finger would, through `enigo`, which posts
//! the system-defined event those keys are.
//!
//! # What that cannot do
//!
//! - There is one play/pause key, so [`MediaAction::Play`] and
//!   [`MediaAction::Pause`] are both that key: a Mac that is already quiet
//!   told to pause starts something, and macOS may start Music if nothing
//!   has played. The assistant is told which it did, not what then happened.
//! - What is playing is not known, so [`MediaAction::Status`] is answered
//!   with a plain "I can't tell", and the answer to a key never names a
//!   track.
//!
//! # The first time asks
//!
//! Posting a key press into the system needs the Accessibility permission,
//! the one voice typing already asks for, under System Settings, Privacy &
//! Security. Without it `enigo` refuses to start, and that is said in words.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use jatayu_service::desk::{MediaAction, NowPlaying};
use tauri::AppHandle;

/// What is named as the player: the one the system sends the key to, which
/// is not known here.
/// Read by the model, so it says in words that this was a key press and
/// that `playing` is what the key should have done, not what was seen.
const PLAYER: &str = "whichever app played last (a media key was pressed; macOS does not say \
                      what is playing, so whether it is playing now is a guess)";

/// Do `action` and say what was done. Pressed on the main thread, where the
/// system wants key events to come from.
pub(in crate::desk) async fn control(
    app: &AppHandle,
    action: MediaAction,
) -> Result<Option<NowPlaying>, String> {
    let Some(key) = key_for(action) else {
        return Err("I can't tell what's playing on a Mac, only press play, pause, next \
                    and previous."
            .to_string());
    };
    crate::writing::on_main(app, move || press(key)).await??;
    Ok(Some(NowPlaying {
        player: PLAYER.to_string(),
        playing: playing_after(action),
        title: None,
        artist: None,
    }))
}

/// The key that does `action`, or `None` for one that is not a key.
fn key_for(action: MediaAction) -> Option<Key> {
    match action {
        MediaAction::Play | MediaAction::Pause | MediaAction::Toggle => Some(Key::MediaPlayPause),
        MediaAction::Next => Some(Key::MediaNextTrack),
        MediaAction::Previous => Some(Key::MediaPrevTrack),
        MediaAction::Status => None,
    }
}

/// Whether the music is going after `action`, as far as pressing the key
/// says: a track change plays, and a pause is a pause. A toggle's result
/// depends on what it was and is not known, so it is not claimed to play.
fn playing_after(action: MediaAction) -> bool {
    matches!(action, MediaAction::Play | MediaAction::Next | MediaAction::Previous)
}

fn press(key: Key) -> Result<(), String> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
        tracing::warn!(error = %e, "the media keys could not be pressed");
        "The Mac hasn't allowed Jatayu to press keys. Turn it on in System Settings, \
         under Privacy & Security, Accessibility."
            .to_string()
    })?;
    enigo.key(key, Direction::Click).map_err(|e| {
        tracing::warn!(error = %e, "a media key was not pressed");
        "The media key wouldn't press.".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_and_pause_are_the_one_key_and_status_is_not_a_key() {
        for action in [MediaAction::Play, MediaAction::Pause, MediaAction::Toggle] {
            assert!(matches!(key_for(action), Some(Key::MediaPlayPause)));
        }
        assert!(matches!(key_for(MediaAction::Next), Some(Key::MediaNextTrack)));
        assert!(matches!(key_for(MediaAction::Previous), Some(Key::MediaPrevTrack)));
        assert!(key_for(MediaAction::Status).is_none());
    }

    #[test]
    fn only_what_a_key_guarantees_is_claimed() {
        assert!(playing_after(MediaAction::Next));
        assert!(playing_after(MediaAction::Play));
        assert!(!playing_after(MediaAction::Pause));
        assert!(!playing_after(MediaAction::Toggle));
    }
}
