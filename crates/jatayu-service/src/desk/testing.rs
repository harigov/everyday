//! A desk for tests: it does nothing to any computer, and writes down what
//! it was asked to do.

use std::sync::Mutex;

use futures::future::BoxFuture;
use jatayu_core::FileId;

use super::{Abilities, Desk, DeskSetting, InstalledApp, MediaAction, NowPlaying};
use super::{Timer, Volume, VolumeChange};

/// What [`FakeDesk`] was asked, in order: `"open_url https://..."`,
/// `"launch org.gnome.Nautilus.desktop"`, `"ring 1"`.
#[derive(Default)]
pub struct FakeDesk {
    pub can: Abilities,
    pub apps: Vec<InstalledApp>,
    pub asked: Mutex<Vec<String>>,
}

impl FakeDesk {
    /// A desk that can do everything.
    pub fn able() -> Self {
        Self {
            can: Abilities {
                apps: true,
                files: true,
                sections: true,
                media: true,
                volume: true,
                settings: DeskSetting::ALL.to_vec(),
                lock: true,
                copy: true,
            },
            ..Self::default()
        }
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn note(&self, what: String) {
        self.asked.lock().unwrap().push(what);
    }
}

fn done<T: Send + 'static>(value: T) -> BoxFuture<'static, Result<T, String>> {
    Box::pin(async move { Ok(value) })
}

impl Desk for FakeDesk {
    fn abilities(&self) -> Abilities {
        self.can.clone()
    }
    fn open_url(&self, url: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("open_url {url}"));
        done(())
    }
    fn open_file(&self, id: FileId) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("open_file {id}"));
        done(())
    }
    fn show_section(&self, section: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("show_section {section}"));
        done(())
    }
    fn apps(&self) -> BoxFuture<'static, Result<Vec<InstalledApp>, String>> {
        done(self.apps.clone())
    }
    fn launch(&self, id: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("launch {id}"));
        done(())
    }
    fn media(&self, action: MediaAction) -> BoxFuture<'static, Result<Option<NowPlaying>, String>> {
        self.note(format!("media {action:?}"));
        done(Some(NowPlaying {
            player: "Music".into(),
            playing: action != MediaAction::Pause,
            title: Some("A song".into()),
            artist: None,
        }))
    }
    fn volume(&self, change: VolumeChange) -> BoxFuture<'static, Result<Volume, String>> {
        self.note(format!("volume {change:?}"));
        done(Volume { percent: 40, muted: change == VolumeChange::Mute })
    }
    fn setting(
        &self,
        setting: DeskSetting,
        on: Option<bool>,
    ) -> BoxFuture<'static, Result<bool, String>> {
        self.note(format!("setting {setting:?} {on:?}"));
        done(on.unwrap_or(false))
    }
    fn lock_screen(&self) -> BoxFuture<'static, Result<(), String>> {
        self.note("lock_screen".into());
        done(())
    }
    fn copy(&self, text: String) -> BoxFuture<'static, Result<(), String>> {
        self.note(format!("copy {text}"));
        done(())
    }
    fn ring(&self, timer: Timer) {
        self.note(format!("ring {}", timer.id));
    }
}
