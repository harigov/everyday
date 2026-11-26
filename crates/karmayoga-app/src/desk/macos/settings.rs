//! Dark mode, through System Events.
//!
//! The one switch a Mac has a public way to flip: System Events' appearance
//! preferences, which is what the Appearance setting itself writes. Do Not
//! Disturb (Focus) and Night Shift have no public interface -- the scripts
//! that exist press buttons in Control Centre or write private preferences,
//! and break with the next release -- so they are not offered: see
//! [`super::abilities`].
//!
//! # The first time asks
//!
//! Unlike the volume, this is telling *another application* to do something,
//! and macOS asks once, in a prompt of its own, whether Karma Yoga may control
//! System Events. If the person says no -- or has said it before -- `osascript`
//! fails with error -1743, and the answer is in words that say where to
//! change their mind (System Settings, Privacy & Security, Automation).

use karmayoga_service::desk::DeskSetting;

const READ: &str =
    r#"tell application "System Events" to tell appearance preferences to get dark mode"#;

/// Switch `setting` on or off (`Some`) or only read it (`None`), and say
/// whether it is on afterwards. Blocking.
pub(in crate::desk) fn apply(setting: DeskSetting, on: Option<bool>) -> Result<bool, String> {
    if setting != DeskSetting::DarkMode {
        return Err(format!("{} can't be changed on a Mac from here.", setting.label()));
    }
    if let Some(on) = on {
        run(&format!(
            r#"tell application "System Events" to tell appearance preferences to set dark mode to {on}"#
        ))?;
    }
    // Read back, so what is said is what is so.
    parse(&run(READ)?).ok_or_else(|| {
        tracing::warn!("dark mode was not answered with true or false");
        "I couldn't tell whether dark mode is on.".to_string()
    })
}

/// Run a line of AppleScript for System Events and give back what it printed.
fn run(line: &str) -> Result<String, String> {
    let ran = super::script(line).map_err(|e| {
        tracing::warn!(error = %e, "dark mode could not be reached");
        "The Mac's appearance settings didn't answer.".to_string()
    })?;
    if ran.success {
        return Ok(ran.stdout);
    }
    tracing::warn!(stderr = %ran.stderr.trim(), "dark mode could not be changed");
    if refused(&ran.stderr) {
        return Err("The Mac hasn't allowed Karma Yoga to change its appearance. Turn on \
                    System Events for Karma Yoga in System Settings, under Privacy & \
                    Security, Automation."
            .to_string());
    }
    Err("The Mac wouldn't change dark mode.".to_string())
}

/// Did the person (or an earlier answer of theirs) refuse the Automation
/// prompt? `osascript` says error -1743, "Not authorized to send Apple
/// events".
fn refused(stderr: &str) -> bool {
    stderr.contains("-1743") || stderr.contains("Not authorized to send Apple events")
}

/// `true` or `false`, as AppleScript prints a boolean.
fn parse(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_prompt_is_error_1743() {
        assert!(refused(
            "29:61: execution error: Not authorized to send Apple events to System Events. (-1743)"
        ));
        assert!(refused("execution error: (-1743)"));
        assert!(!refused("execution error: System Events got an error: Can't get it. (-1728)"));
        assert!(!refused(""));
    }

    #[test]
    fn dark_mode_is_a_printed_boolean() {
        assert_eq!(parse("true\n"), Some(true));
        assert_eq!(parse("false\n"), Some(false));
        assert_eq!(parse("missing value"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn what_a_mac_cannot_switch_is_said_so() {
        let said = apply(DeskSetting::NightLight, Some(true)).unwrap_err();
        assert!(said.contains("can't be changed"), "{said}");
    }

    /// The real System Events on this Mac, changing nothing anybody would
    /// see: dark mode is read, then set to what it already was. The first
    /// run may raise the Automation prompt this module's errors talk about.
    #[test]
    #[ignore = "asks this Mac's System Events; run by hand with --ignored"]
    fn live_dark_mode_is_read_and_set_where_it_was() {
        let before = apply(DeskSetting::DarkMode, None).expect("dark mode was read");
        let after = apply(DeskSetting::DarkMode, Some(before)).expect("dark mode was set");
        assert_eq!(after, before);
    }
}
