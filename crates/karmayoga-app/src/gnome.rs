//! GNOME's own settings, for the few places this app follows the desktop:
//! the meeting card's light or dark, and how a held key repeats.
//!
//! A schema that is not installed is not asked for: `GSettings` ends the
//! process when it is, and a desktop without GNOME's schemas is an ordinary
//! one. An installed schema is not proof that GNOME is running either --
//! other desktops pull it in -- so a setting that only GNOME acts on is read
//! only under GNOME ([`running`]).

/// Whether this session is GNOME's: `XDG_CURRENT_DESKTOP` names it, as
/// Ubuntu's `ubuntu:GNOME` does.
pub fn running() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktops| desktops.split(':').any(|d| d.eq_ignore_ascii_case("gnome")))
}

/// `schema`'s settings, if it is installed and has every one of `keys`.
pub fn settings(schema: &str, keys: &[&str]) -> Option<gtk::gio::Settings> {
    let source = gtk::gio::SettingsSchemaSource::default()?;
    let found = source.lookup(schema, true)?;
    keys.iter().all(|key| found.has_key(key)).then(|| gtk::gio::Settings::new(schema))
}
