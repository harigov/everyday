//! What page script is not trusted with.
//!
//! Every window in this application is a webview, and a webview runs script.
//! Nothing known puts a stranger's script into one -- mail is shown on white
//! in a sandbox with no script, a feed article is sanitised, a calendar
//! feed's text is text -- but the interface is the one part of this program
//! that renders other people's words, and the day one of them gets through it
//! is the day whatever that script may ask of the shell is everything the
//! person who owns the vault may do. This module is the part of that which is
//! decided in Rust, where script cannot reach it. There are two halves.
//!
//! # Which window may ask for what
//!
//! Tauri's own access list (`build.rs`, `capabilities/`) says which of the
//! shell's *commands* a window may invoke. That leaves `call`, one command that
//! runs any of the service's hundreds by name, so a window that may `call` at
//! all could run any of them: [`window_may_call`] is the same list again for
//! `call`, for the windows that are not the main window or an app window. The
//! writing popup runs two service commands, the voice typing pill and the
//! meeting card none, and a window this table does not know runs none either.
//! A new narrow window has to be added here, which is the point: its calls are
//! decided when somebody writes it, not inherited.
//!
//! # What only the person may allow
//!
//! Some things are worth a question even though the interface asked for them
//! in good faith, because their effect outlasts the click and reaches past
//! this program: running another program, letting other computers connect, and
//! handing out a key to the vault. The interface already asks, in its own
//! dialogs -- which are page content, and so are as good as the page that drew
//! them. A script that wanted a program run would skip the question and send
//! the command. So the shell asks again, here, with the platform's own dialog
//! raised from Rust, which no script can click, and says what it is about to do
//! in terms the person can check: the program and its arguments exactly as the
//! service will start them, the names (never the values) of the variables it
//! is given, the address a server would answer on.
//!
//! The pure halves -- whether a call needs asking about, and what the question
//! says -- are separate from the dialog so they can be tested; [`ask`] is the
//! only part that touches the screen.
//!
//! The same question is asked before anything that lets another computer, or
//! another program, in: turning on web access, offering a pairing code,
//! letting a connected computer unlock the vault, offering destructive tools
//! over MCP, and trusting another of the owner's chat devices. Each of those is
//! a switch the interface has a button for, and each is exactly what a script
//! would press first.
//!
//! What is *not* asked about, deliberately: a connection that is already set up
//! being switched on or off, renamed or checked, since the person approved its
//! program when it was added; a server answering on this computer's loopback
//! address only, which nothing off this machine can reach; and anything the
//! assistant or a routine does, which does not come through here at all.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use karmayoga_core::chat::{ChatService, tidy_device_ids};
use karmayoga_core::connection::{Connection, Transport};
use karmayoga_core::{ChatLinkId, Error};
use karmayoga_service::Scope;
use karmayoga_service::chat::ChatDevice;
use karmayoga_service::command::parse;
use karmayoga_service::ctx::Ctx;
use karmayoga_service::domains::chat::SetChatTrustedDevices;
use karmayoga_service::domains::connections::{AddConnection, UpdateConnection};
use karmayoga_service::error::{CommandError, CommandResult, codes};
use karmayoga_service::service::blocking;
use serde_json::Value;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::state::AppState;

// ---- which window may call what ---------------------------------------------

/// The windows that are neither the main window nor an app window, and the
/// service commands each may run through `call`. Every other window, one that
/// is not in this table, may run none.
///
/// What each page does is what is listed, and nothing more:
///
/// * the writing popup (`ui/src/writing/WritingPopup.svelte`) runs the rewrite
///   itself and, when the person dismisses it while the model is still
///   thinking, settles it;
/// * the voice typing pill talks to the shell through commands of its own and
///   runs no service command;
/// * the meeting card is the same.
///
/// `acl::` tests read those pages and fail if this table and what they
/// name come apart.
const NARROW_WINDOWS: &[(&str, &[&str])] = &[
    (crate::writing::LABEL, &["rewrite_selection", "settle_writing_assist"]),
    (crate::voicetyping::pill::LABEL, &[]),
    (crate::recnotice::LABEL, &[]),
];

/// The service commands the narrow window called `label` may run through
/// `call`: the table above, or nothing for a window that is not in it.
pub fn narrow_window_calls(label: &str) -> &'static [&'static str] {
    match NARROW_WINDOWS.iter().find(|(l, _)| *l == label) {
        Some((_, names)) => names,
        None => &[],
    }
}

/// Whether the window called `label` may run the service command `name`
/// through `call`.
///
/// The main window and the windows opened on a single app run everything: they
/// are the interface. Any other window runs only what [`NARROW_WINDOWS`] lists.
pub fn window_may_call(label: &str, name: &str) -> bool {
    if label == crate::windows::MAIN || crate::windows::is_app_window(label) {
        return true;
    }
    narrow_window_calls(label).contains(&name)
}

/// [`window_may_call`] as the error `call` answers with.
pub fn check_window_call(label: &str, name: &str) -> CommandResult<()> {
    if window_may_call(label, name) {
        Ok(())
    } else {
        Err(CommandError::new(
            codes::FORBIDDEN,
            format!("the {label} window is not allowed to run {name}"),
        ))
    }
}

// ---- what the question says -------------------------------------------------

/// A question for the person: a title and what is about to happen, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub title: &'static str,
    pub body: String,
}

/// The longest a single word of a command line, a name or a path is shown.
/// Long enough for any real path; short enough that a word padded to push the
/// rest of the dialog off the screen is cut, and says that it was.
const WORD_LIMIT: usize = 300;

/// How many arguments are listed before "and so many more".
const ARGS_SHOWN: usize = 16;

/// How many variable names are listed before "and so many more".
const NAMES_SHOWN: usize = 24;

/// Characters that draw nothing, or draw something other than what is there,
/// and are not control characters: a zero-width space, a right-to-left
/// override. Shown as their code, so a name cannot hide a word or run
/// backwards.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
    )
}

/// `text` as one line of plain text: new lines, tabs and anything unseen
/// written out as what they are, and cut at `limit` characters.
///
/// What is shown in the question comes from the page, and a question that a
/// page can reshape -- a new line and a fake "Program: /usr/bin/true" after
/// it -- is not one the person can check.
fn plain(text: &str, limit: usize) -> String {
    let mut shown = String::new();
    for (count, c) in text.chars().enumerate() {
        if count == limit {
            shown.push_str(&format!("... ({} characters in all)", text.chars().count()));
            break;
        }
        match c {
            '\n' => shown.push_str("\\n"),
            '\r' => shown.push_str("\\r"),
            '\t' => shown.push_str("\\t"),
            c if c.is_control() || is_invisible(c) => {
                shown.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => shown.push(c),
        }
    }
    shown
}

/// One word of a command line as a shell would need it written, so that an
/// argument with a space in it, or with nothing in it, reads as one argument.
fn shell_word(word: &str) -> String {
    let safe = !word.is_empty()
        && word.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe { word.to_string() } else { format!("'{}'", word.replace('\'', "'\\''")) }
}

/// The program and its arguments, written the way they would be typed.
fn program_line(command: &str, args: &[String]) -> String {
    let mut words = vec![plain(&shell_word(command), WORD_LIMIT)];
    words.extend(args.iter().take(ARGS_SHOWN).map(|a| plain(&shell_word(a), WORD_LIMIT)));
    let mut line = words.join(" ");
    if args.len() > ARGS_SHOWN {
        line.push_str(&format!(" ... and {} more arguments", args.len() - ARGS_SHOWN));
    }
    line
}

/// Variable names, comma separated, with a count when there are many.
fn names_line(names: &[String]) -> String {
    let mut line =
        names.iter().take(NAMES_SHOWN).map(|n| plain(n, WORD_LIMIT)).collect::<Vec<_>>().join(", ");
    if names.len() > NAMES_SHOWN {
        line.push_str(&format!(", and {} more", names.len() - NAMES_SHOWN));
    }
    line
}

/// What a program is started with, as lines of the question: the program, the
/// folder it starts in if one was chosen, and which variables it is given.
/// `None` for a transport that is not a program.
fn program_details(transport: &Transport) -> Option<String> {
    let Transport::Stdio { command, args, env, cwd } = transport else { return None };
    let mut lines = vec![format!("Program: {}", program_line(command, args))];
    if let Some(cwd) = cwd.as_deref().filter(|c| !c.is_empty()) {
        lines.push(format!("Starts in the folder: {}", plain(cwd, WORD_LIMIT)));
    }
    lines.push(if env.is_empty() {
        "Environment variables it is given: none".to_string()
    } else {
        format!(
            "Environment variables it is given: {} (their values are not shown)",
            names_line(env)
        )
    });
    Some(lines.join("\n"))
}

/// What is said last in any question about a program, so that it is read.
const PROGRAM_WARNING: &str = "It would run with all the access you have to this computer, and \
    the assistant could use whatever it offers. Only allow it if you asked for this yourself.";

/// The question to ask before this connection is added, if it runs a program.
///
/// Read from the arguments exactly as the service will read them -- the same
/// type, then the same tidying of spaces -- so what is asked about is what
/// would be started. `None` for a connection that is a web address, which
/// runs nothing here.
pub fn added_connection_prompt(added: &AddConnection) -> Option<Prompt> {
    let connection =
        Connection::new(added.name.clone(), added.transport.clone(), added.auth.clone())
            .normalized();
    let details = program_details(&connection.transport)?;
    Some(Prompt {
        title: "Allow a program to run?",
        body: format!(
            "Allow Karma Yoga to run this program on your computer?\n\n\
             Connection: {}\n{details}\n\n{PROGRAM_WARNING}",
            plain(&connection.name, WORD_LIMIT),
        ),
    })
}

/// The question to ask before `after` replaces `before`, if that changes what
/// is run.
///
/// Only a change of the program counts -- its command, arguments, variable
/// names or folder, compared as the service compares them, after the same
/// tidying -- or a connection that was a web address becoming one. Renaming,
/// switching on or off, and the person's choices about tools are not, and
/// neither is a program becoming a web address.
pub fn edited_connection_prompt(before: &Connection, after: Connection) -> Option<Prompt> {
    let after = after.normalized();
    let details = program_details(&after.transport)?;
    if before.transport == after.transport {
        return None;
    }
    let was = match program_details(&before.transport) {
        Some(was) => format!("It runs now:\n{was}\n\nIt would run instead:\n{details}"),
        None => format!(
            "It does not run a program now; it is a web address. It would run instead:\n{details}"
        ),
    };
    Some(Prompt {
        title: "Allow a different program to run?",
        body: format!(
            "Allow Karma Yoga to change which program this connection runs?\n\n\
             Connection: {}\n{was}\n\n{PROGRAM_WARNING}",
            plain(&after.name, WORD_LIMIT),
        ),
    })
}

/// A server this application can start on the person's say-so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// This vault, to computers the person pairs with (`share_start`).
    /// `allow_remote_unlock` is whether a connected computer may also unlock
    /// it, which `share_start` can turn on in the same call.
    Sharing { allow_remote_unlock: bool },
    /// This vault's tools, to a program holding a token (`mcp_start`).
    Mcp,
}

/// The question to ask before a server starts answering on `address`, unless
/// that is this computer's own loopback address, which nothing off the machine
/// can reach.
///
/// An IPv4 address written as IPv6 (`::ffff:127.0.0.1`) is the same machine
/// and is not asked about; `0.0.0.0` and `::` are every network at once and
/// are.
pub fn listening_prompt(what: Served, address: SocketAddr) -> Option<Prompt> {
    let ip = address.ip();
    if ip.to_canonical().is_loopback() {
        return None;
    }
    let reach = if ip.is_unspecified() {
        format!("every network this computer is on, on port {}", address.port())
    } else {
        address.to_string()
    };
    Some(match what {
        Served::Sharing { allow_remote_unlock } => Prompt {
            title: "Allow other computers to connect?",
            body: format!(
                "Allow other computers to reach Karma Yoga on this one?\n\n\
                 It would answer on {reach}. A computer still has to pair with a code you give \
                 it before it can use your vault, but it would be able to ask.{}\n\n\
                 Only allow this if you turned sharing on yourself.",
                if allow_remote_unlock {
                    " A computer that has paired could also unlock the vault from there."
                } else {
                    ""
                },
            ),
        },
        Served::Mcp => Prompt {
            title: "Allow other computers to connect?",
            body: format!(
                "Allow programs on other computers to reach this vault's tools?\n\n\
                 It would answer on {reach}. A program that holds a token for this vault could \
                 use it from there.\n\n\
                 Only allow this if you turned the MCP server on yourself."
            ),
        },
    })
}

/// The question to ask before an access token is made.
pub fn token_prompt(scopes: &[Scope]) -> Prompt {
    let reach = if scopes.contains(&Scope::All) {
        "everything in this vault".to_string()
    } else if scopes.is_empty() {
        "nothing (no part of the vault was chosen)".to_string()
    } else {
        let names: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
        format!("these parts of this vault: {}", names.join(", "))
    };
    Prompt {
        title: "Allow a new access token?",
        body: format!(
            "Make an access token for another program?\n\n\
             Whoever holds it can use {reach}, as you, from outside Karma Yoga. It is shown to \
             you once.\n\n\
             Only allow this if you asked to connect a program yourself."
        ),
    }
}

/// The question to ask before web access is switched on: the interface served
/// to browsers on the person's tailnet, signed in to with the vault password.
pub fn web_access_prompt(port: u16) -> Prompt {
    Prompt {
        title: "Allow web access?",
        body: format!(
            "Allow this vault to be opened in a web browser from other devices on your \
             Tailscale network?\n\n\
             Karma Yoga would serve its pages on port {port}, to this computer's Tailscale \
             addresses only. Anyone who can reach it and knows your vault password could sign \
             in and use everything in the vault.\n\n\
             Only allow this if you turned web access on yourself."
        ),
    }
}

/// The question to ask before a pairing code is made: a code that lets
/// another computer become a device of this vault.
pub fn pairing_prompt() -> Prompt {
    Prompt {
        title: "Allow a new computer to pair?",
        body: "Make a pairing code so another computer can connect to this vault?\n\n\
               The code works for five minutes. A computer that pairs with it gets the same access \
               to your vault as this one has, until you remove it from the list of devices.\n\n\
               Only allow this if you are adding a computer yourself."
            .to_string(),
    }
}

/// The question to ask before a connected computer is allowed to unlock the
/// vault.
pub fn remote_unlock_prompt() -> Prompt {
    Prompt {
        title: "Allow other computers to unlock your vault?",
        body: "Let a computer that has paired with this vault unlock it?\n\n\
               Today a locked vault stays locked until somebody unlocks it here. With this on, a \
               paired computer can ask for it to be unlocked.\n\n\
               Only allow this if you turned it on yourself."
            .to_string(),
    }
}

/// The question to ask before MCP starts offering tools that delete or
/// overwrite.
pub fn destructive_tools_prompt() -> Prompt {
    Prompt {
        title: "Allow programs to delete things?",
        body: "Let programs connected over MCP use tools that delete or overwrite things in your \
               vault?\n\n\
               They are left out of what MCP offers now. Once allowed, a program that holds a \
               token for this vault can use them, within what its token covers.\n\n\
               Only allow this if you turned it on yourself."
            .to_string(),
    }
}

/// How many devices are named in a question before "and so many more".
const DEVICES_SHOWN: usize = 12;

/// The devices in `requested` that `current` does not already trust, tidied
/// the way the service tidies a list before it keeps it (trimmed, without
/// empty entries or repeats, in order).
///
/// Taking a device off the list, or saving the list as it is, is no reason to
/// ask: nothing new can talk to the assistant.
pub fn newly_trusted(current: &[String], requested: Vec<String>) -> Vec<String> {
    let current = tidy_device_ids(current);
    tidy_device_ids(requested).into_iter().filter(|device| !current.contains(device)).collect()
}

/// The question to ask before more of the owner's devices are trusted to talk
/// to the assistant. Each is its id and, where the chat service has told us, the
/// name its owner gave it.
pub fn trusted_devices_prompt(added: &[(String, Option<String>)]) -> Prompt {
    let mut lines: Vec<String> = added
        .iter()
        .take(DEVICES_SHOWN)
        .map(|(id, name)| match name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => format!("  {} ({})", plain(name, WORD_LIMIT), plain(id, WORD_LIMIT)),
            None => format!("  {}", plain(id, WORD_LIMIT)),
        })
        .collect();
    if added.len() > DEVICES_SHOWN {
        lines.push(format!("  ... and {} more", added.len() - DEVICES_SHOWN));
    }
    let (these, them) =
        if added.len() == 1 { ("this device", "it") } else { ("these devices", "them") };
    Prompt {
        title: "Allow another device to talk to the assistant?",
        body: format!(
            "Trust {these} to talk to the assistant through chat?\n\n{}\n\n\
             The assistant believes what a trusted device sends it, and its replies go there. \
             A device that is not on the list is ignored. Only allow this if you picked {them} \
             yourself.",
            lines.join("\n"),
        ),
    }
}

// ---- asking -----------------------------------------------------------------

/// Set while a question is on the screen.
///
/// One at a time: a page that asked for a hundred of them would bury the one
/// it meant, and each answer would be a click the person has no way to tell
/// from the last. The second is refused, not queued -- behind a question the
/// person has not answered there is nothing to wait for.
static ASKING: AtomicBool = AtomicBool::new(false);

/// The right to put a question on the screen, given back when this is dropped.
struct OneAtATime;

impl OneAtATime {
    fn take() -> CommandResult<Self> {
        if ASKING.swap(true, Ordering::SeqCst) {
            return Err(CommandError::new(
                codes::BUSY,
                "Karma Yoga is already waiting for an answer to another question; answer that \
                 one first",
            ));
        }
        Ok(Self)
    }
}

impl Drop for OneAtATime {
    fn drop(&mut self) {
        ASKING.store(false, Ordering::SeqCst);
    }
}

/// Put `prompt` in the platform's own dialog, and wait for the answer.
///
/// `Ok` is "Allow"; anything else -- Cancel, the dialog closed, the platform
/// unable to show it -- is `cancelled`, which the interface shows as the
/// message below and the sign-in flow already treats as the person's own
/// decision. Never a default: a question that could not be asked was not
/// answered yes.
///
/// The dialog is the plugin's callback form with a channel behind it, not its
/// blocking one: this runs on Tauri's async runtime, and a worker held for as
/// long as somebody takes to read a question is a window that has stopped
/// answering.
pub async fn ask(app: &tauri::AppHandle, prompt: Prompt) -> CommandResult<()> {
    let _only_one = OneAtATime::take()?;
    let (answer, heard) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(prompt.body)
        .title(prompt.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Allow".into(), "Cancel".into()))
        .show(move |allowed| {
            let _ = answer.send(allowed);
        });
    match heard.await {
        Ok(true) => Ok(()),
        _ => Err(CommandError::new(codes::CANCELLED, "You did not allow that, so it was not done")),
    }
}

/// The owner's devices as the running bot lists them, for naming the ones about
/// to be trusted. Empty if the bot is not running, does not answer in a few
/// seconds, or answers with something unreadable: the question then names the
/// devices by id alone, which is all the list holds anyway.
async fn device_names(state: &AppState, id: ChatLinkId) -> Vec<ChatDevice> {
    let listed = tokio::time::timeout(
        Duration::from_secs(5),
        state.service().call(Ctx::local(), "chat_devices", serde_json::json!({ "id": id })),
    )
    .await;
    match listed {
        Ok(Ok(value)) => serde_json::from_value(value).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Ask about a call the interface is making, if it is one worth asking about.
///
/// Called by `call` before the service sees anything, for a vault in this
/// process. For one on another computer the request is forwarded, that
/// computer's own rules refuse a program to anybody but the shell on that
/// machine, and a question about this computer would be about the wrong one.
///
/// A connection's or a device list's arguments that do not parse are refused
/// here with the error the service would give, rather than let through for it
/// to refuse: nothing is forwarded that this has not read.
///
/// For `set_chat_trusted_devices` the question is asked only when the new list
/// holds a device the link does not trust now -- the saved list is read the way
/// the service reads it, and a list that only shrinks or stays the same is not
/// asked about. The devices are named from the running bot's own list when it
/// will give one, and by id when it will not.
pub async fn confirm_call(
    app: &tauri::AppHandle,
    state: &AppState,
    name: &str,
    args: &Value,
) -> CommandResult<()> {
    let prompt = match name {
        "add_connection" => {
            let added: AddConnection = parse(name, args.clone())?;
            added_connection_prompt(&added)
        }
        "update_connection" => {
            let edited: UpdateConnection = parse(name, args.clone())?;
            let id = edited.connection.id;
            // What is stored now, which is what the service compares with.
            let vault = state.service().require_unlocked()?;
            let before = blocking(move || Ok(vault.connection(id)?)).await?;
            edited_connection_prompt(&before, edited.connection)
        }
        "set_chat_trusted_devices" => {
            let wanted: SetChatTrustedDevices = parse(name, args.clone())?;
            let id = wanted.id;
            // The list the link trusts now, read the way the service reads it
            // before it replaces it: from the saved settings, by link id. A
            // link that is not there is the service's `not_found`.
            let vault = state.service().require()?;
            let current = blocking(move || {
                let settings = vault.chat_settings()?;
                let link = settings
                    .link(id)
                    .ok_or_else(|| CommandError::from(Error::not_found("chat link", id)))?;
                let ChatService::Matrix(matrix) = &link.service;
                Ok(matrix.trusted_devices.clone())
            })
            .await?;
            let added = newly_trusted(&current, wanted.devices);
            if added.is_empty() {
                None
            } else {
                let known = device_names(state, id).await;
                let named: Vec<(String, Option<String>)> = added
                    .into_iter()
                    .map(|device| {
                        let name = known
                            .iter()
                            .find(|k| k.device_id == device)
                            .and_then(|k| k.display_name.clone());
                        (device, name)
                    })
                    .collect();
                Some(trusted_devices_prompt(&named))
            }
        }
        _ => None,
    };
    match prompt {
        Some(prompt) => ask(app, prompt).await,
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::connection::AuthMethod;
    use serde_json::json;

    fn program(command: &str, args: &[&str], env: &[&str], cwd: Option<&str>) -> Transport {
        Transport::Stdio {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: env.iter().map(|e| e.to_string()).collect(),
            cwd: cwd.map(str::to_string),
        }
    }

    fn address() -> Transport {
        Transport::Http { url: "https://mcp.example.com/".into(), headers: Vec::new() }
    }

    fn connection(transport: Transport) -> Connection {
        Connection::new("Files", transport, AuthMethod::None)
    }

    /// An `add_connection` call as the interface sends it, for a program.
    fn add_call(secrets: Value) -> AddConnection {
        serde_json::from_value(json!({
            "name": "Shell",
            "transport": {
                "type": "stdio",
                "command": "/bin/sh",
                "args": ["-c", "echo hi"],
                "env": ["API_TOKEN"],
            },
            "auth": { "type": "none" },
            "secrets": secrets,
        }))
        .unwrap()
    }

    // ---- which window may call what

    #[test]
    fn the_main_window_and_app_windows_may_call_anything() {
        for label in ["main", "app-todo", "app-mail"] {
            assert!(window_may_call(label, "add_connection"), "{label}");
            assert!(window_may_call(label, "anything_at_all"), "{label}");
        }
    }

    #[test]
    fn the_writing_popup_may_call_the_two_commands_its_page_uses_and_no_others() {
        assert!(window_may_call("writing", "rewrite_selection"));
        assert!(window_may_call("writing", "settle_writing_assist"));
        assert!(!window_may_call("writing", "add_connection"));
        assert!(!window_may_call("writing", "entries"));
        assert!(!window_may_call("writing", "rewrite_selection_please"));
    }

    #[test]
    fn the_pill_and_the_card_may_call_nothing() {
        for label in ["voice-typing", "recording-notice"] {
            assert!(!window_may_call(label, "rewrite_selection"), "{label}");
            assert!(!window_may_call(label, "add_connection"), "{label}");
            assert!(narrow_window_calls(label).is_empty(), "{label}");
        }
    }

    #[test]
    fn a_window_nobody_listed_may_call_nothing() {
        for label in ["", "settings", "main ", "Main", "application", "app", "writing-2"] {
            assert!(!window_may_call(label, "rewrite_selection"), "{label:?}");
        }
    }

    #[test]
    fn a_refused_call_says_which_window_and_which_command() {
        let refused = check_window_call("voice-typing", "mcp_start").unwrap_err();
        assert_eq!(refused.code, codes::FORBIDDEN);
        assert!(refused.message.contains("voice-typing"), "{}", refused.message);
        assert!(refused.message.contains("mcp_start"), "{}", refused.message);
        assert!(check_window_call("main", "mcp_start").is_ok());
    }

    // ---- adding a program

    #[test]
    fn adding_a_program_asks_with_the_program_its_arguments_and_the_variable_names() {
        let prompt = added_connection_prompt(&add_call(json!({}))).unwrap();
        assert!(prompt.body.contains("run this program on your computer"), "{}", prompt.body);
        assert!(prompt.body.contains("Program: /bin/sh -c 'echo hi'"), "{}", prompt.body);
        assert!(prompt.body.contains("API_TOKEN"), "{}", prompt.body);
        assert!(prompt.body.contains("Connection: Shell"), "{}", prompt.body);
    }

    #[test]
    fn the_values_of_the_variables_are_never_in_the_question() {
        let prompt =
            added_connection_prompt(&add_call(json!({ "env": { "API_TOKEN": "hunter2" } })))
                .unwrap();
        assert!(!prompt.body.contains("hunter2"), "{}", prompt.body);
    }

    #[test]
    fn adding_a_web_address_asks_nothing() {
        let added: AddConnection = serde_json::from_value(json!({
            "name": "Linear",
            "transport": { "type": "http", "url": "https://mcp.linear.app/mcp", "headers": [] },
            "auth": { "type": "none" },
        }))
        .unwrap();
        assert_eq!(added_connection_prompt(&added), None);
    }

    #[test]
    fn the_program_is_shown_as_the_service_will_start_it_with_its_spaces_trimmed() {
        let added: AddConnection = serde_json::from_value(json!({
            "name": "  Padded  ",
            "transport": { "type": "stdio", "command": "  /bin/sh  ", "args": ["", "a b"] },
            "auth": { "type": "none" },
        }))
        .unwrap();
        let prompt = added_connection_prompt(&added).unwrap();
        assert!(prompt.body.contains("Program: /bin/sh 'a b'\n"), "{}", prompt.body);
        assert!(prompt.body.contains("Connection: Padded\n"), "{}", prompt.body);
        assert!(prompt.body.contains("variables it is given: none"), "{}", prompt.body);
    }

    #[test]
    fn a_folder_is_named_when_one_was_chosen() {
        let with = connection(program("node", &["server.js"], &[], Some("/home/me/tools")));
        let prompt = edited_connection_prompt(&connection(address()), with).unwrap();
        assert!(prompt.body.contains("Starts in the folder: /home/me/tools"), "{}", prompt.body);
    }

    // ---- changing a connection

    #[test]
    fn an_edit_that_leaves_the_program_alone_asks_nothing() {
        let before = connection(program("npx", &["-y", "thing"], &["KEY"], None));
        let mut after = before.clone();
        after.name = "Renamed".into();
        after.enabled = false;
        after.unattended = true;
        assert_eq!(edited_connection_prompt(&before, after), None);
    }

    #[test]
    fn an_edit_is_compared_after_the_same_tidying_the_service_does() {
        let before = connection(program("npx", &["thing"], &[], None));
        let after = connection(program("  npx  ", &["thing", ""], &[], Some("  ")));
        assert_eq!(edited_connection_prompt(&before, after), None);
    }

    #[test]
    fn changing_the_command_the_arguments_the_variables_or_the_folder_each_asks() {
        let before = connection(program("npx", &["thing"], &["KEY"], None));
        let changes = [
            program("/bin/sh", &["thing"], &["KEY"], None),
            program("npx", &["thing", "--then-this"], &["KEY"], None),
            program("npx", &["thing"], &["KEY", "LD_PRELOAD"], None),
            program("npx", &["thing"], &["KEY"], Some("/tmp")),
        ];
        for changed in changes {
            let mut after = before.clone();
            after.transport = changed.clone();
            let prompt = edited_connection_prompt(&before, after)
                .unwrap_or_else(|| panic!("{changed:?} was not asked about"));
            assert!(prompt.body.contains("It runs now:"), "{}", prompt.body);
            assert!(prompt.body.contains("It would run instead:"), "{}", prompt.body);
            assert!(prompt.body.contains("Program: npx thing"), "{}", prompt.body);
        }
    }

    #[test]
    fn a_web_address_becoming_a_program_asks_and_says_what_it_was() {
        let before = connection(address());
        let after = connection(program("/bin/sh", &["-c", "id"], &[], None));
        let prompt = edited_connection_prompt(&before, after).unwrap();
        assert!(prompt.body.contains("does not run a program now"), "{}", prompt.body);
        assert!(prompt.body.contains("Program: /bin/sh -c id"), "{}", prompt.body);
    }

    #[test]
    fn a_program_becoming_a_web_address_asks_nothing() {
        let before = connection(program("npx", &["thing"], &[], None));
        let after = connection(address());
        assert_eq!(edited_connection_prompt(&before, after), None);
    }

    // ---- reading what the page sent

    #[test]
    fn a_new_line_in_an_argument_cannot_start_a_line_of_the_question() {
        let added: AddConnection = serde_json::from_value(json!({
            "name": "Harmless\nProgram: /usr/bin/true",
            "transport": {
                "type": "stdio",
                "command": "/bin/sh",
                "args": ["-c", "id\nEnvironment variables it is given: none"],
            },
            "auth": { "type": "none" },
        }))
        .unwrap();
        let prompt = added_connection_prompt(&added).unwrap();
        // Three real lines of detail plus the heading and the warning: the
        // page's own line breaks are written out, not obeyed.
        assert!(!prompt.body.contains("Harmless\nProgram"), "{}", prompt.body);
        assert!(prompt.body.contains("Harmless\\nProgram"), "{}", prompt.body);
        assert_eq!(prompt.body.matches("\nProgram: ").count(), 1, "{}", prompt.body);
        assert_eq!(prompt.body.matches("Environment variables it is given").count(), 2);
    }

    #[test]
    fn characters_that_draw_nothing_are_written_out() {
        assert_eq!(plain("a\u{202e}b\u{200b}c\u{7}", 50), "a\\u{202e}b\\u{200b}c\\u{0007}");
        assert_eq!(plain("tab\there", 50), "tab\\there");
        assert_eq!(plain("tidy path/é.txt", 50), "tidy path/é.txt");
    }

    #[test]
    fn a_word_padded_to_push_the_rest_off_the_screen_is_cut_and_says_so() {
        let long = "a".repeat(5_000);
        let shown = plain(&long, WORD_LIMIT);
        assert!(shown.starts_with(&"a".repeat(WORD_LIMIT)));
        assert!(shown.ends_with("(5000 characters in all)"), "{shown}");
        assert!(shown.chars().count() < WORD_LIMIT + 40);
    }

    #[test]
    fn a_crowd_of_arguments_is_counted_rather_than_listed() {
        let many: Vec<String> = (0..40).map(|n| format!("arg{n}")).collect();
        let line = program_line("tool", &many);
        assert!(line.contains("arg15"), "{line}");
        assert!(!line.contains("arg16"), "{line}");
        assert!(line.ends_with("... and 24 more arguments"), "{line}");
        let names: Vec<String> = (0..30).map(|n| format!("VAR_{n}")).collect();
        assert!(names_line(&names).ends_with(", and 6 more"));
    }

    #[test]
    fn words_are_quoted_so_that_each_reads_as_one_argument() {
        assert_eq!(shell_word("/usr/bin/node"), "/usr/bin/node");
        assert_eq!(shell_word("--flag=value"), "--flag=value");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("echo hi"), "'echo hi'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word("a;b"), "'a;b'");
        assert_eq!(
            program_line("/bin/sh", &["-c".into(), "echo hi".into()]),
            "/bin/sh -c 'echo hi'"
        );
    }

    // ---- listening

    fn socket(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    /// Sharing as the interface starts it, with no unlocking from afar.
    const SHARING: Served = Served::Sharing { allow_remote_unlock: false };

    #[test]
    fn a_server_on_this_computers_own_address_asks_nothing() {
        for what in [SHARING, Served::Sharing { allow_remote_unlock: true }, Served::Mcp] {
            for text in
                ["127.0.0.1:7398", "127.0.0.9:7398", "[::1]:7398", "[::ffff:127.0.0.1]:7398"]
            {
                assert_eq!(listening_prompt(what, socket(text)), None, "{what:?} {text}");
            }
        }
    }

    #[test]
    fn a_server_on_every_interface_asks_and_says_so() {
        for what in [SHARING, Served::Mcp] {
            for text in ["0.0.0.0:7397", "[::]:7397"] {
                let prompt = listening_prompt(what, socket(text))
                    .unwrap_or_else(|| panic!("{what:?} {text} was not asked about"));
                assert!(prompt.body.contains("every network this computer is on, on port 7397"));
            }
        }
    }

    #[test]
    fn a_server_on_one_network_address_names_it() {
        let prompt = listening_prompt(Served::Mcp, socket("192.168.1.20:7398")).unwrap();
        assert!(prompt.body.contains("192.168.1.20:7398"), "{}", prompt.body);
        assert!(prompt.body.contains("MCP"), "{}", prompt.body);
        let prompt = listening_prompt(SHARING, socket("100.64.0.3:7397")).unwrap();
        assert!(prompt.body.contains("100.64.0.3:7397"), "{}", prompt.body);
        assert!(prompt.body.contains("pair"), "{}", prompt.body);
    }

    #[test]
    fn sharing_that_also_lets_a_computer_unlock_the_vault_says_so() {
        let unlocking = Served::Sharing { allow_remote_unlock: true };
        let prompt = listening_prompt(unlocking, socket("0.0.0.0:7397")).unwrap();
        assert!(prompt.body.contains("could also unlock the vault"), "{}", prompt.body);
        let prompt = listening_prompt(SHARING, socket("0.0.0.0:7397")).unwrap();
        assert!(!prompt.body.contains("unlock"), "{}", prompt.body);
    }

    // ---- the other ways in

    #[test]
    fn web_access_names_the_port_and_that_the_vault_password_is_the_way_in() {
        let prompt = web_access_prompt(7400);
        assert!(prompt.body.contains("port 7400"), "{}", prompt.body);
        assert!(prompt.body.contains("Tailscale"), "{}", prompt.body);
        assert!(prompt.body.contains("vault password"), "{}", prompt.body);
    }

    #[test]
    fn a_pairing_code_says_how_long_it_lasts_and_what_a_paired_computer_gets() {
        let prompt = pairing_prompt();
        assert!(prompt.body.contains("five minutes"), "{}", prompt.body);
        assert!(prompt.body.contains("same access"), "{}", prompt.body);
    }

    #[test]
    fn unlocking_and_destructive_tools_each_have_a_question_that_says_which() {
        assert!(remote_unlock_prompt().body.contains("unlock it"));
        let destructive = destructive_tools_prompt();
        assert!(destructive.body.contains("delete or overwrite"), "{}", destructive.body);
        assert!(destructive.body.contains("MCP"), "{}", destructive.body);
    }

    // ---- trusting devices

    fn ids(devices: &[&str]) -> Vec<String> {
        devices.iter().map(|d| d.to_string()).collect()
    }

    #[test]
    fn only_a_device_not_trusted_now_is_new() {
        let current = ids(&["PHONE", "LAPTOP"]);
        assert_eq!(newly_trusted(&current, ids(&["PHONE", "LAPTOP", "TABLET"])), ["TABLET"]);
        assert_eq!(newly_trusted(&current, ids(&["TABLET", "ATTACKER"])), ["ATTACKER", "TABLET"]);
    }

    #[test]
    fn a_list_that_shrinks_or_stays_the_same_adds_nothing() {
        let current = ids(&["PHONE", "LAPTOP"]);
        assert!(newly_trusted(&current, ids(&["PHONE"])).is_empty());
        assert!(newly_trusted(&current, ids(&["LAPTOP", "PHONE"])).is_empty());
        assert!(newly_trusted(&current, Vec::new()).is_empty());
        assert!(newly_trusted(&[], Vec::new()).is_empty());
    }

    #[test]
    fn devices_are_compared_after_the_same_tidying_the_service_does() {
        // Trimmed, without empty entries or repeats: the same device spelt
        // with a space is not a new one, and a blank is not a device.
        let current = ids(&["PHONE"]);
        assert!(newly_trusted(&current, ids(&[" PHONE ", "", "  ", "PHONE"])).is_empty());
        assert_eq!(newly_trusted(&current, ids(&[" NEW ", "NEW", ""])), ["NEW"]);
        // And a saved list that was kept with stray spaces still matches.
        assert!(newly_trusted(&ids(&[" PHONE"]), ids(&["PHONE"])).is_empty());
    }

    #[test]
    fn a_device_is_named_by_its_name_and_id_or_by_id_alone() {
        let prompt = trusted_devices_prompt(&[
            ("ABCDEFGHIJ".to_string(), Some("Element on iPhone".to_string())),
            ("KLMNOPQRST".to_string(), None),
            ("UVWXYZ0123".to_string(), Some("   ".to_string())),
        ]);
        assert!(prompt.body.contains("  Element on iPhone (ABCDEFGHIJ)\n"), "{}", prompt.body);
        assert!(prompt.body.contains("  KLMNOPQRST\n"), "{}", prompt.body);
        assert!(prompt.body.contains("  UVWXYZ0123\n"), "{}", prompt.body);
        assert!(prompt.body.contains("these devices"), "{}", prompt.body);
    }

    #[test]
    fn one_device_is_asked_about_in_the_singular() {
        let prompt = trusted_devices_prompt(&[("ABCDEFGHIJ".to_string(), None)]);
        assert!(prompt.body.contains("Trust this device to talk"), "{}", prompt.body);
        assert!(prompt.body.contains("picked it yourself"), "{}", prompt.body);
    }

    #[test]
    fn a_device_name_cannot_start_a_line_of_the_question() {
        let prompt = trusted_devices_prompt(&[(
            "ID".to_string(),
            Some("Phone\nTrust this device to talk to nobody".to_string()),
        )]);
        assert!(!prompt.body.contains("Phone\nTrust"), "{}", prompt.body);
        assert!(prompt.body.contains("Phone\\nTrust"), "{}", prompt.body);
    }

    #[test]
    fn a_crowd_of_devices_is_counted_rather_than_listed() {
        let many: Vec<(String, Option<String>)> =
            (0..20).map(|n| (format!("DEVICE{n:02}"), None)).collect();
        let prompt = trusted_devices_prompt(&many);
        assert!(prompt.body.contains("DEVICE11"), "{}", prompt.body);
        assert!(!prompt.body.contains("DEVICE12"), "{}", prompt.body);
        assert!(prompt.body.contains("... and 8 more"), "{}", prompt.body);
    }

    // ---- tokens

    #[test]
    fn a_token_for_everything_says_everything() {
        let prompt = token_prompt(&[Scope::All]);
        assert!(prompt.body.contains("everything in this vault"), "{}", prompt.body);
    }

    #[test]
    fn a_narrower_token_names_what_it_reaches() {
        let prompt = token_prompt(&[Scope::Journals, Scope::Mail]);
        assert!(prompt.body.contains("journals, mail"), "{}", prompt.body);
        assert!(!prompt.body.contains("everything"), "{}", prompt.body);
    }

    // ---- being asked

    #[test]
    fn only_one_question_is_on_the_screen_at_a_time() {
        let first = OneAtATime::take().expect("nothing was being asked");
        let second = OneAtATime::take().err().expect("a second question was let through");
        assert_eq!(second.code, codes::BUSY);
        drop(first);
        drop(OneAtATime::take().expect("the screen was not given back"));
    }
}
