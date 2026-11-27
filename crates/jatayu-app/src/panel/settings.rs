//! What goes to the panel and what stays a system notification, per machine
//! (`panel.json`), and how long cards stay up.
//!
//! Per machine rather than in the vault because it is about this desktop:
//! somebody who keeps the panel for new mail on the laptop may want the
//! desktop's own banners on the computer they share a screen from. Read once
//! and kept in memory, since every new message and every meeting's notes ask
//! for it ([`get`]); Settings is the one thing that changes it.

use std::ops::RangeInclusive;
use std::sync::Mutex;

use jatayu_service::error::{CommandError, CommandResult, codes};
use serde::{Deserialize, Serialize};

/// The file the settings are kept in, in the per-machine config folder.
pub const FILE: &str = "panel.json";

/// Where something that wants the person's attention goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    /// A card on the panel, with what it needs to be answered there.
    #[default]
    Panel,
    /// The desktop's own notification, as before the panel took it.
    Notification,
}

/// The panel's settings on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PanelSettings {
    /// New mail and invitations (those the account's "Notify me about" lets
    /// through at all).
    pub mail: Route,
    /// A meeting's notes written, with the action items found in them.
    pub notes: Route,
    /// The assistant asking for a yes, a no or an answer while it works on
    /// its own -- routines, projects, jobs -- and a question from a
    /// conversation in the main window while that window is out of sight.
    pub asks: Route,
    /// How long a mail card stays up unread, in seconds.
    pub mail_seconds: u32,
    /// How long the action items card stays up before what was not decided
    /// is left as proposals, in seconds.
    pub notes_seconds: u32,
    /// The live card while a meeting is recorded: your notes, questions.
    pub recording_card: bool,
}

impl Default for PanelSettings {
    fn default() -> Self {
        PanelSettings {
            mail: Route::Panel,
            notes: Route::Panel,
            asks: Route::Panel,
            mail_seconds: 45,
            notes_seconds: 120,
            recording_card: true,
        }
    }
}

/// How long a mail card may be set to stay up: long enough to be read, short
/// enough that a card nobody saw does not sit over the screen all afternoon.
const MAIL_SECONDS: RangeInclusive<u32> = 10..=600;
/// The same for the action items card, which takes longer to go through.
const NOTES_SECONDS: RangeInclusive<u32> = 15..=1800;

/// The settings as last read or saved: read from the file once, then kept.
static KEPT: Mutex<Option<PanelSettings>> = Mutex::new(None);

/// `settings` with its times brought within [`MAIL_SECONDS`] and
/// [`NOTES_SECONDS`]: a file edited by hand, or written by a newer build,
/// cannot make a card that goes before it is seen or never goes.
fn checked(mut settings: PanelSettings) -> PanelSettings {
    settings.mail_seconds = settings.mail_seconds.clamp(*MAIL_SECONDS.start(), *MAIL_SECONDS.end());
    settings.notes_seconds =
        settings.notes_seconds.clamp(*NOTES_SECONDS.start(), *NOTES_SECONDS.end());
    settings
}

/// The settings as they are now. Cheap: kept in memory after the first read.
pub fn get() -> PanelSettings {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.get_or_insert_with(|| checked(crate::machine_config::load(FILE))).clone()
}

/// The panel's settings, for Settings.
#[tauri::command]
pub async fn panel_settings() -> CommandResult<PanelSettings> {
    Ok(get())
}

/// Change the panel's settings; answers with them as kept, which is with the
/// times brought within what they may be.
#[tauri::command]
pub async fn set_panel_settings(settings: PanelSettings) -> CommandResult<PanelSettings> {
    let settings = checked(settings);
    crate::machine_config::save(FILE, &settings).map_err(|e| CommandError::new(codes::IO, e))?;
    *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(settings.clone());
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_use_the_names_settings_sends() {
        let json = serde_json::to_value(PanelSettings::default()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "mail": "panel",
                "notes": "panel",
                "asks": "panel",
                "mailSeconds": 45,
                "notesSeconds": 120,
                "recordingCard": true,
            })
        );
    }

    #[test]
    fn a_file_with_only_some_settings_reads_with_the_rest_as_they_were() {
        let read: PanelSettings =
            serde_json::from_value(serde_json::json!({ "mail": "notification" })).unwrap();
        assert_eq!(read, PanelSettings { mail: Route::Notification, ..PanelSettings::default() });
    }

    #[test]
    fn the_defaults_are_within_bounds() {
        assert_eq!(checked(PanelSettings::default()), PanelSettings::default());
    }

    #[test]
    fn times_are_kept_within_what_they_may_be() {
        let wild = PanelSettings { mail_seconds: 0, notes_seconds: 99_999, ..Default::default() };
        let kept = checked(wild);
        assert_eq!(kept.mail_seconds, 10);
        assert_eq!(kept.notes_seconds, 1800);
        let other =
            checked(PanelSettings { mail_seconds: 9_000, notes_seconds: 1, ..Default::default() });
        assert_eq!((other.mail_seconds, other.notes_seconds), (600, 15));
        let fine =
            checked(PanelSettings { mail_seconds: 90, notes_seconds: 300, ..Default::default() });
        assert_eq!((fine.mail_seconds, fine.notes_seconds), (90, 300));
    }

    #[test]
    fn the_other_settings_are_left_as_they_are() {
        let asked = PanelSettings {
            mail: Route::Notification,
            notes: Route::Notification,
            asks: Route::Notification,
            mail_seconds: 0,
            notes_seconds: 120,
            recording_card: false,
        };
        let kept = checked(asked.clone());
        assert_eq!(kept, PanelSettings { mail_seconds: 10, ..asked });
    }
}
