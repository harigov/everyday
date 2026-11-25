//! Three of GNOME's switches -- Do Not Disturb, the dark style, Night Light --
//! through GSettings.
//!
//! Each is one key in the settings database, and the Shell and the apps
//! watch it: writing the key *is* flipping the switch, which is how the
//! Settings panel and the quick-settings menu do it too. Nothing here is a
//! private interface.
//!
//! | Switch | Schema | Key | On is |
//! |---|---|---|---|
//! | Do Not Disturb | `org.gnome.desktop.notifications` | `show-banners` | `false` |
//! | Dark mode | `org.gnome.desktop.interface` | `color-scheme` | `'prefer-dark'` |
//! | Night Light | `org.gnome.settings-daemon.plugins.color` | `night-light-enabled` | `true` |
//!
//! Do Not Disturb is the odd one: GNOME has a switch for *showing* banners,
//! so "on" means that one is off. Dark mode is a choice of three strings,
//! `default`, `prefer-dark` and `prefer-light`, and turning it off goes back
//! to `default` -- the system's own light -- rather than choosing
//! `prefer-light`, which is a preference the person never expressed.
//!
//! # Only what is installed, and only under GNOME
//!
//! GSettings ends the process when it is asked for a schema that is not
//! installed -- see [`crate::gnome`] -- so a key is looked up in the schema
//! source before it is used, and a switch whose key is not there is not
//! offered: an older GNOME with no `color-scheme`, a Shell with the Night
//! Light plugin removed. The schemas are installed on desktops that are not
//! GNOME too (the libraries pull them in), where writing them does nothing
//! the person would see, so none is offered unless the session is GNOME's.
//!
//! GSettings is read and written on a worker thread of the blocking pool, as
//! these calls wait on the settings daemon.

use gtk::gio::prelude::*;
use karmayoga_service::desk::DeskSetting;

/// The schema and key a switch lives in.
fn location(setting: DeskSetting) -> (&'static str, &'static str) {
    match setting {
        DeskSetting::DoNotDisturb => ("org.gnome.desktop.notifications", "show-banners"),
        DeskSetting::DarkMode => ("org.gnome.desktop.interface", "color-scheme"),
        DeskSetting::NightLight => {
            ("org.gnome.settings-daemon.plugins.color", "night-light-enabled")
        }
    }
}

/// Which switches this desktop has.
pub(in crate::desk) fn available() -> Vec<DeskSetting> {
    if !crate::gnome::running() {
        return Vec::new();
    }
    DeskSetting::ALL
        .into_iter()
        .filter(|setting| {
            let (schema, key) = location(*setting);
            has_key(schema, key)
        })
        .collect()
}

/// Is `key` in `schema`, and is `schema` installed? Asked of the schema
/// source, which answers `None` for a schema that is not there instead of
/// ending the process as a `GSettings` built from its name would.
fn has_key(schema: &str, key: &str) -> bool {
    gtk::gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(schema, true))
        .is_some_and(|schema| schema.has_key(key))
}

/// Set `setting` (when `on` is given) and say whether it is on afterwards.
/// Blocking.
pub(in crate::desk) fn apply(setting: DeskSetting, on: Option<bool>) -> Result<bool, String> {
    let (schema, key) = location(setting);
    if !crate::gnome::running() {
        return Err(format!("{} can't be changed on this desktop.", capitalised(setting)));
    }
    let settings = crate::gnome::settings(schema, &[key])
        .ok_or_else(|| format!("{} isn't available on this computer.", capitalised(setting)))?;
    if let Some(on) = on {
        write(&settings, setting, key, on).map_err(|e| {
            tracing::warn!(setting = setting.key(), error = %e, "a setting could not be written");
            format!("{} is locked on this computer.", capitalised(setting))
        })?;
        // The write goes to the settings daemon, and returns before it is
        // done; the read back below would find the old value without this.
        gtk::gio::Settings::sync();
    }
    Ok(read(&settings, setting, key))
}

fn read(settings: &gtk::gio::Settings, setting: DeskSetting, key: &str) -> bool {
    match setting {
        DeskSetting::DoNotDisturb => do_not_disturb(settings.boolean(key)),
        DeskSetting::DarkMode => is_dark(&settings.string(key)),
        DeskSetting::NightLight => settings.boolean(key),
    }
}

fn write(
    settings: &gtk::gio::Settings,
    setting: DeskSetting,
    key: &str,
    on: bool,
) -> Result<(), String> {
    match setting {
        DeskSetting::DoNotDisturb => settings.set_boolean(key, show_banners(on)),
        DeskSetting::DarkMode => settings.set_string(key, scheme(on)),
        DeskSetting::NightLight => settings.set_boolean(key, on),
    }
    .map_err(|e| e.to_string())
}

/// Do Not Disturb as a person says it, from GNOME's "show banners".
fn do_not_disturb(show_banners: bool) -> bool {
    !show_banners
}

/// GNOME's "show banners" for Do Not Disturb as a person asked for it: the
/// other way round, and written as its own function so that the write is
/// tested as well as the read.
fn show_banners(do_not_disturb: bool) -> bool {
    !do_not_disturb
}

/// Is the style `scheme` the dark one?
fn is_dark(scheme: &str) -> bool {
    scheme == "prefer-dark"
}

/// The style to ask for: the dark one, or the system's own.
fn scheme(dark: bool) -> &'static str {
    if dark { "prefer-dark" } else { "default" }
}

/// "Do Not Disturb", "Dark mode", "Night Light", to begin a sentence with.
fn capitalised(setting: DeskSetting) -> String {
    let label = setting.label();
    let mut chars = label.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn do_not_disturb_is_the_banners_being_off() {
        assert!(do_not_disturb(false));
        assert!(!do_not_disturb(true));
        // Turning it on hides the banners, and reads back as on.
        assert!(!show_banners(true));
        assert!(show_banners(false));
        for asked in [true, false] {
            assert_eq!(do_not_disturb(show_banners(asked)), asked);
        }
    }

    #[test]
    fn dark_is_the_one_style_that_says_so() {
        assert!(is_dark("prefer-dark"));
        assert!(!is_dark("default"));
        assert!(!is_dark("prefer-light"));
        assert!(!is_dark(""));
        assert_eq!(scheme(true), "prefer-dark");
        assert_eq!(scheme(false), "default");
        assert!(is_dark(scheme(true)) && !is_dark(scheme(false)));
    }

    #[test]
    fn every_switch_is_in_a_schema_of_its_own_kind() {
        // The three the assistant is told about, and nowhere else.
        let places: Vec<_> = DeskSetting::ALL.into_iter().map(location).collect();
        assert_eq!(
            places,
            [
                ("org.gnome.desktop.notifications", "show-banners"),
                ("org.gnome.desktop.interface", "color-scheme"),
                ("org.gnome.settings-daemon.plugins.color", "night-light-enabled"),
            ]
        );
    }

    #[test]
    fn a_label_begins_a_sentence() {
        assert_eq!(capitalised(DeskSetting::DoNotDisturb), "Do Not Disturb");
        assert_eq!(capitalised(DeskSetting::DarkMode), "Dark mode");
        assert_eq!(capitalised(DeskSetting::NightLight), "Night Light");
    }
}
