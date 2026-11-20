//! Tests that keep four places that name the shell's commands in step.
//!
//! Which commands a window may invoke is decided by Tauri's access list, and
//! that list is spread over four places that nothing at compile time ties
//! together:
//!
//! * `invoke_handler` in `src/lib.rs`, which registers the commands;
//! * `APP_COMMANDS` in `build.rs`, which declares them to Tauri so its access
//!   list applies to them at all;
//! * the capability files in `capabilities/`, which grant each declared
//!   command to the windows that need it;
//! * and `guard::NARROW_WINDOWS`, which says which *service* commands a narrow
//!   window may run through the one command, `call`, that reaches them.
//!
//! A command registered but not declared is refused for every window, and one
//! declared but not granted is refused for the window that needs it. Both show
//! up as a line in a webview's console and as a button that does nothing,
//! which is a poor way to learn that somebody forgot a line of JSON. A command
//! granted to a window whose page never calls it is the opposite mistake and a
//! quiet one: the narrow windows exist to be narrow.
//!
//! There is no code here, only tests, and the module is compiled for tests
//! alone. The pages are read as text rather than parsed: a command named in
//! quotes in a page is a command that page calls, and the test says which
//! window and which command when that stops being true in either direction.

use std::collections::BTreeSet;

use karmayoga_service::command;
use serde_json::Value;

use crate::guard::narrow_window_calls;

const BUILD_RS: &str = include_str!("../build.rs");
const LIB_RS: &str = include_str!("lib.rs");

const DEFAULT_CAPABILITY: &str = include_str!("../capabilities/default.json");
const WRITING_CAPABILITY: &str = include_str!("../capabilities/writing.json");
const VOICE_TYPING_CAPABILITY: &str = include_str!("../capabilities/voice-typing.json");
const RECORDING_CAPABILITY: &str = include_str!("../capabilities/recording.json");

const WRITING_PAGE: &str = include_str!("../../../ui/src/writing/WritingPopup.svelte");
const VOICE_TYPING_PAGE: &str = include_str!("../../../ui/src/voicetyping/VoiceTypingPill.svelte");
const RECORDING_PAGE: &str = include_str!("../../../ui/src/recording/RecordingCard.svelte");

/// The narrow windows: label, capability file, and the page that runs in it.
fn narrow_windows() -> [(&'static str, &'static str, &'static str); 3] {
    [
        (crate::writing::LABEL, WRITING_CAPABILITY, WRITING_PAGE),
        (crate::voicetyping::pill::LABEL, VOICE_TYPING_CAPABILITY, VOICE_TYPING_PAGE),
        (crate::recnotice::LABEL, RECORDING_CAPABILITY, RECORDING_PAGE),
    ]
}

/// The permission Tauri generates for a command.
fn allow(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// The quoted names in the list that starts at `after` in `source` and ends at
/// the first `end` following it, one per line.
fn quoted_lines(source: &str, after: &str, end: &str) -> Vec<String> {
    let start =
        source.find(after).unwrap_or_else(|| panic!("{after:?} is not there")) + after.len();
    let list = &source[start..];
    let list = &list[..list.find(end).unwrap_or_else(|| panic!("{end:?} does not end it"))];
    list.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_string())
        })
        .collect()
}

/// What `build.rs` declares.
fn declared() -> Vec<String> {
    quoted_lines(BUILD_RS, "const APP_COMMANDS: &[&str] = &[", "];")
}

/// What `invoke_handler` registers: the last path segment of each line of the
/// `generate_handler!` list that is not a comment.
fn registered() -> Vec<String> {
    let after = "tauri::generate_handler![";
    let start = LIB_RS.find(after).expect("lib.rs has no generate_handler!") + after.len();
    let list = &LIB_RS[start..];
    let list = &list[..list.find("])").expect("generate_handler! is not closed")];
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .map(|line| line.trim_end_matches(',').rsplit("::").next().unwrap().to_string())
        .collect()
}

/// A capability file's windows and the application commands it grants, as the
/// command names behind its `allow-*` permissions.
fn capability(json: &str) -> (Vec<String>, BTreeSet<String>) {
    let file: Value = serde_json::from_str(json).expect("a capability file is JSON");
    let windows = file["windows"]
        .as_array()
        .expect("a capability names its windows")
        .iter()
        .map(|w| w.as_str().expect("a window is a label").to_string())
        .collect();
    let granted = file["permissions"]
        .as_array()
        .expect("a capability lists permissions")
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| p.starts_with("allow-"))
        .map(str::to_string)
        .collect();
    (windows, granted)
}

fn set(names: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    names.into_iter().collect()
}

#[test]
fn build_rs_declares_exactly_the_commands_lib_rs_registers() {
    let (declared, registered) = (declared(), registered());
    assert!(registered.len() > 50, "read only {} commands from lib.rs", registered.len());
    for (what, names) in [("build.rs", &declared), ("lib.rs", &registered)] {
        assert_eq!(set(names.clone()).len(), names.len(), "{what} names a command twice");
    }
    let (declared, registered) = (set(declared), set(registered));
    let missing: Vec<_> = registered.difference(&declared).collect();
    let extra: Vec<_> = declared.difference(&registered).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "registered in lib.rs but not declared in build.rs: {missing:?}; declared in build.rs but \
         not registered: {extra:?}"
    );
}

#[test]
fn the_main_window_and_the_app_windows_are_granted_every_command() {
    let (windows, granted) = capability(DEFAULT_CAPABILITY);
    assert_eq!(windows, [crate::windows::MAIN, "app-*"]);
    let every: BTreeSet<_> = registered().iter().map(|c| allow(c)).collect();
    let missing: Vec<_> = every.difference(&granted).collect();
    let unknown: Vec<_> = granted.difference(&every).collect();
    assert!(
        missing.is_empty() && unknown.is_empty(),
        "default.json is missing {missing:?}, and grants {unknown:?}, which no command has"
    );
}

#[test]
fn the_glob_for_app_windows_matches_the_label_they_are_given() {
    assert!(crate::windows::is_app_window("app-todo"));
    assert!(!crate::windows::is_app_window("writing"));
    assert!(DEFAULT_CAPABILITY.contains("\"app-*\""));
}

#[test]
fn every_window_is_in_exactly_one_capability_file() {
    let mut seen = Vec::new();
    for json in
        [DEFAULT_CAPABILITY, WRITING_CAPABILITY, VOICE_TYPING_CAPABILITY, RECORDING_CAPABILITY]
    {
        seen.extend(capability(json).0);
    }
    assert_eq!(
        set(seen.clone()).len(),
        seen.len(),
        "a window is in two capability files: {seen:?}"
    );
    for (label, json, _) in narrow_windows() {
        assert_eq!(capability(json).0, [label], "the capability for {label} names other windows");
    }
}

#[test]
fn each_narrow_window_is_granted_the_commands_its_page_names_and_no_others() {
    let registered = registered();
    for (label, json, page) in narrow_windows() {
        let named = set(registered
            .iter()
            .filter(|c| page.contains(&format!("'{c}'")) || page.contains(&format!("\"{c}\"")))
            .map(|c| allow(c)));
        let (_, granted) = capability(json);
        assert!(!named.is_empty(), "read no command from the {label} window's page");
        assert_eq!(
            granted, named,
            "the {label} window's capability and the commands its page names have come apart"
        );
    }
}

#[test]
fn a_narrow_window_is_granted_call_exactly_when_it_may_call_something() {
    for (label, json, _) in narrow_windows() {
        let (_, granted) = capability(json);
        assert_eq!(
            granted.contains("allow-call"),
            !narrow_window_calls(label).is_empty(),
            "{label}: the grant of `call` and the list of what it may call disagree"
        );
    }
}

#[test]
fn each_narrow_window_may_call_the_service_commands_its_page_calls_and_no_others() {
    for (label, _, page) in narrow_windows() {
        // `call` is invoked as `invoke('call', { name: 'rewrite_selection', ... })`.
        let mut called = BTreeSet::new();
        let mut rest = page;
        while let Some(at) = rest.find("name: '") {
            rest = &rest[at + "name: '".len()..];
            called.insert(rest[..rest.find('\'').unwrap()].to_string());
        }
        let allowed: BTreeSet<String> =
            narrow_window_calls(label).iter().map(|n| n.to_string()).collect();
        assert_eq!(allowed, called, "{label}: guard::NARROW_WINDOWS and its page disagree");
        for name in &allowed {
            assert!(
                command::find(name).is_some(),
                "{label} may call {name}, which is not a command"
            );
        }
    }
}
