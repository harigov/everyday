//! The assistant's tools for this computer's desktop.
//!
//! Declared here, in the service, as the browser's and the typing tools are:
//! they need the live [`Desk`], which only the app has. Offered by
//! `agent::build` to a turn at this computer alone
//! (`TurnMeta::desk_here`), and then only what [`Desk::abilities`] says this
//! desktop can do -- a model never told about the volume cannot promise to
//! turn it down. What asks first is decided in the agent's gate, from the
//! names here; see the module docs of [`super`].
//!
//! Every answer ends with a `note` saying how to report it, because most of
//! these are said aloud: "Paused." is the whole of a good reply to "pause
//! the music", and a model left to itself describes the tool it called.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp, Zoned};
use karmayoga_core::FileId;
use karmayoga_core::agent::desk::{self as core_desk, AppMatch, SearchSite};
use rig_agent::core::tool::{PortableDynamicTool, ToolExecutionError, ToolOutput};
use serde_json::{Value, json};

use super::{Abilities, Desk, DeskSetting, MediaAction, SECTIONS, Timer, VolumeChange};
use crate::agent::Typist;
use crate::service::Service;

/// Open a link, an application, one of Karma Yoga's own apps or a file.
pub const OPEN: &str = "open";
/// Search a website in their browser.
pub const SEARCH_SITE: &str = "search_site";
pub const CONTROL_MEDIA: &str = "control_media";
pub const SET_VOLUME: &str = "set_volume";
pub const DESKTOP_SETTING: &str = "desktop_setting";
pub const LOCK_SCREEN: &str = "lock_screen";
pub const COPY_TO_CLIPBOARD: &str = "copy_to_clipboard";
pub const SET_TIMER: &str = "set_timer";
pub const LIST_TIMERS: &str = "list_timers";
pub const CANCEL_TIMER: &str = "cancel_timer";

/// Every tool here, for a check that names them all.
pub const ALL: [&str; 10] = [
    OPEN,
    SEARCH_SITE,
    CONTROL_MEDIA,
    SET_VOLUME,
    DESKTOP_SETTING,
    LOCK_SCREEN,
    COPY_TO_CLIPBOARD,
    SET_TIMER,
    LIST_TIMERS,
    CANCEL_TIMER,
];

/// The most text copied at once: what `type_text` types at once, which is
/// also what the gate shows whole when it asks about either.
pub const MAX_COPIED_CHARS: usize = crate::agent::MAX_TYPED_CHARS;

/// What the tools are built from: the desk, and what the turn knows.
#[derive(Clone)]
pub struct Here {
    pub desk: Arc<dyn Desk>,
    pub service: Arc<Service>,
    /// The sites to search: somebody's own and the built-in ones, read as
    /// the turn was built -- see [`core_desk::all_sites`].
    pub sites: Vec<SearchSite>,
    /// What types where the person was typing -- only from the talk key or
    /// the wake word -- which is also what reads what they have selected,
    /// for `search_site`'s `selection`.
    pub typist: Option<Arc<dyn Typist>>,
    /// The person's time zone, by IANA name, for "an alarm at seven".
    pub zone: String,
}

/// The tools this desk can serve.
pub fn tools(here: Here) -> Vec<PortableDynamicTool> {
    let can = here.desk.abilities();
    let mut tools = vec![open_tool(here.clone(), &can), search_site_tool(here.clone())];
    if can.media {
        tools.push(control_media_tool(here.clone()));
    }
    if can.volume {
        tools.push(set_volume_tool(here.clone()));
    }
    if !can.settings.is_empty() {
        tools.push(desktop_setting_tool(here.clone(), &can.settings));
    }
    if can.lock {
        tools.push(lock_screen_tool(here.clone()));
    }
    if can.copy {
        tools.push(copy_tool(here.clone()));
    }
    tools.push(set_timer_tool(here.clone()));
    tools.push(list_timers_tool(here.clone()));
    tools.push(cancel_timer_tool(here));
    tools
}

/// What the preamble says about these tools, for a desk that can do what
/// `can` says -- naming exactly the tools [`tools`] offers it, so a model is
/// never told to pause the music on a computer that offered it no way to.
pub fn guidance(can: &Abilities) -> String {
    let mut opens = vec!["a link in their browser"];
    if can.apps {
        opens.push("an application installed on the computer");
    }
    if can.sections {
        opens.push("one of Karma Yoga's own apps");
    }
    if can.files {
        opens.push("a file from Files");
    }
    let settings: Vec<&str> = can.settings.iter().map(|s| s.label()).collect();
    let settings = format!("desktop_setting for {}", listed(&settings, "and"));
    let mut uses: Vec<&str> = Vec::new();
    if can.media {
        uses.push("control_media for music, podcasts and videos");
    }
    if can.volume {
        uses.push("set_volume for loudness");
    }
    if !can.settings.is_empty() {
        uses.push(&settings);
    }
    if can.lock {
        uses.push("lock_screen");
    }
    if can.copy {
        uses.push("copy_to_clipboard for \"copy that\"");
    }
    // One bullet of the assistant's "Your tools here" section -- see
    // `Guidance` in the agent -- so it begins with what it is about, and has no
    // line breaks of its own to fall out of the list.
    let mut said = format!(
        "This computer: you can act on the one they are at. Use search_site when they name a \
         website to search -- \"search Amazon for headphones\", \"look it up on Google\", \
         \"find it on YouTube\" -- which opens that site's results in their own browser; when \
         they only ask a question, with no site named, answer it yourself (with web_search, if \
         you have it). open opens {}.",
        listed(&opens, "or")
    );
    if !uses.is_empty() {
        said.push_str(&format!(" Use {}.", listed(&uses, "and")));
    }
    said.push_str(
        " set_timer, list_timers and cancel_timer are for timers and alarms -- they ring on this \
         computer for up to a day while the app is open; for anything further off or that must \
         not be missed, set a reminder on a task instead. Do these at once when asked, without \
         asking first, and say what you did in a few words: \"Paused.\", \"Ten-minute timer \
         set.\", \"Opened Amazon.\" Anything else on this computer you have no tool for: say so.",
    );
    said
}

/// "a, b and c", "a or b", "a".
fn listed(items: &[&str], last: &str) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., final_one] => format!("{} {last} {final_one}", rest.join(", ")),
    }
}

/// The note every answer carries: report it briefly.
const SAY_BRIEFLY: &str = "Done. Say so in a few words -- what happened, not how.";

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn text<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn said(error: String) -> ToolExecutionError {
    ToolExecutionError::other(error)
}

// ---- open ---------------------------------------------------------------

fn open_tool(here: Here, can: &Abilities) -> PortableDynamicTool {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "url".into(),
        json!({
            "type": "string",
            "description": "A web address to open in their browser, starting https:// or http://.",
        }),
    );
    if can.apps {
        properties.insert(
            "app".into(),
            json!({
                "type": "string",
                "description": "The name of an application installed on this computer, as they \
                                said it: \"Files\", \"Spotify\", \"Firefox\", \"Terminal\".",
            }),
        );
    }
    if can.sections {
        properties.insert(
            "section".into(),
            json!({
                "type": "string",
                "enum": SECTIONS,
                "description": "One of Karma Yoga's own apps -- the app you are part of -- for \
                                \"open my journal\", \"show my calendar\", \"go to mail\". \
                                `todo` is the task list.",
            }),
        );
    }
    if can.files {
        properties.insert(
            "file_id".into(),
            json!({
                "type": "string",
                "description": "A file from Files, by the id your file tools gave you, to open \
                                in the application the computer opens it with.",
            }),
        );
    }
    let what: Vec<&str> = properties.keys().map(String::as_str).collect();
    let description = format!(
        "Open something on the computer they are at: a web page in their own browser, an \
         application, one of Karma Yoga's own apps, or a file from Files. Give exactly one of: \
         {}. To search a website, use search_site; to read a page yourself, read_web_page.",
        what.join(", ")
    );
    let schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    PortableDynamicTool::new(OPEN, description, schema, move |arguments: Value| {
        let here = here.clone();
        Box::pin(async move { open(&here, &arguments).await })
    })
}

async fn open(here: &Here, arguments: &Value) -> Result<ToolOutput, ToolExecutionError> {
    let given: Vec<&str> = ["url", "app", "section", "file_id"]
        .into_iter()
        .filter(|key| text(arguments, key).is_some())
        .collect();
    let [one] = given.as_slice() else {
        return Err(ToolExecutionError::invalid_args(
            "give exactly one of url, app, section or file_id",
        ));
    };
    let value = text(arguments, one).unwrap_or_default();
    match *one {
        "url" => {
            let url = web_address(value)?;
            here.desk.open_url(url.clone()).await.map_err(said)?;
            Ok(ToolOutput::json(json!({ "ok": true, "opened": url, "note": SAY_BRIEFLY })))
        }
        "app" => {
            let apps = here.desk.apps().await.map_err(said)?;
            match core_desk::match_app(&apps, value) {
                AppMatch::One(app) => {
                    let name = app.name.clone();
                    here.desk.launch(app.id.clone()).await.map_err(said)?;
                    Ok(ToolOutput::json(json!({ "ok": true, "opened": name, "note": SAY_BRIEFLY })))
                }
                AppMatch::Several(apps) => {
                    let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
                    Err(ToolExecutionError::other(format!(
                        "Several applications match \"{value}\": {}. Ask which they meant, then \
                         call open again with its full name.",
                        names.join(", ")
                    )))
                }
                AppMatch::None => Err(ToolExecutionError::other(format!(
                    "No application called \"{value}\" is installed on this computer."
                ))),
            }
        }
        "section" => {
            let section = section_named(value).ok_or_else(|| {
                ToolExecutionError::invalid_args(format!(
                    "{value:?} is not one of Karma Yoga's apps: {}",
                    SECTIONS.join(", ")
                ))
            })?;
            here.desk.show_section(section.to_string()).await.map_err(said)?;
            Ok(ToolOutput::json(json!({ "ok": true, "opened": section, "note": SAY_BRIEFLY })))
        }
        _ => {
            let id: FileId = value.parse().map_err(|_| {
                ToolExecutionError::invalid_args(format!("{value:?} is not a file id"))
            })?;
            here.desk.open_file(id).await.map_err(said)?;
            Ok(ToolOutput::json(json!({ "ok": true, "opened": "the file", "note": SAY_BRIEFLY })))
        }
    }
}

/// `value` as an address a browser opens, by the very rule the gate judged
/// it by ([`crate::webpage::parse`]): starting `http://` or `https://`
/// literally, with a site, and no user name or password in it. Anything
/// else -- `file:`, `javascript:`, a bare word, `https:evil.example` or
/// `https://a@evil.example` -- is refused, because an address the gate
/// could not read as one is an address it never asked about.
fn web_address(value: &str) -> Result<String, ToolExecutionError> {
    let parsed = crate::webpage::parse(value).map_err(|why| {
        ToolExecutionError::invalid_args(format!(
            "{why}; give the whole web address, starting https://"
        ))
    })?;
    Ok(parsed.to_string())
}

/// One of [`SECTIONS`] by its key, or by what a person calls it.
fn section_named(value: &str) -> Option<&'static str> {
    let asked: String = value.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let alias = match asked.as_str() {
        "tasks" | "todos" | "todolist" | "tasklist" => "todo",
        "home" | "dashboard" => "overview",
        "email" | "inbox" => "mail",
        "news" | "podcasts" | "newsletters" => "feed",
        "chat" | "conversations" => "assistant",
        "diary" => "journal",
        "books" | "shelves" => "library",
        other => other,
    };
    SECTIONS.iter().copied().find(|s| *s == alias)
}

// ---- search_site --------------------------------------------------------

fn search_site_tool(here: Here) -> PortableDynamicTool {
    let names: Vec<&str> = here.sites.iter().map(|s| s.name.as_str()).collect();
    let mut properties = serde_json::Map::new();
    properties.insert(
        "site".into(),
        json!({
            "type": "string",
            "description": format!("Which website, by name: {}.", names.join(", ")),
        }),
    );
    properties.insert(
        "query".into(),
        json!({ "type": "string", "description": "What to search for, in a few words." }),
    );
    let selection = here.typist.is_some();
    if selection {
        properties.insert(
            "selection".into(),
            json!({
                "type": "boolean",
                "description": "Search for what they have selected where they were typing, \
                                instead of a query -- \"search this on Amazon\", \"look this up \
                                on Google\" with something selected.",
            }),
        );
    }
    let description = format!(
        "Open a website's search results for something in their own browser: \"search Amazon \
         for noise-cancelling headphones\", \"look it up on Google\", \"find it on YouTube\". The \
         sites: {}. Use it when they name a site; a question with no site named, answer \
         yourself.",
        names.join(", ")
    );
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": ["site"],
        "additionalProperties": false,
    });
    PortableDynamicTool::new(SEARCH_SITE, description, schema, move |arguments: Value| {
        let here = here.clone();
        Box::pin(async move {
            let asked = text(&arguments, "site").unwrap_or_default();
            let site = core_desk::find_site(&here.sites, asked).ok_or_else(|| {
                let names: Vec<&str> = here.sites.iter().map(|s| s.name.as_str()).collect();
                ToolExecutionError::invalid_args(format!(
                    "there is no search site called {asked:?}; the sites are {}. They can add one \
                     of their own in the assistant's settings.",
                    names.join(", ")
                ))
            })?;
            let query = match (text(&arguments, "query"), selected(&arguments)) {
                (_, true) => {
                    let Some(typist) = &here.typist else {
                        return Err(ToolExecutionError::invalid_args(
                            "what is selected can only be read from the talk key",
                        ));
                    };
                    let picked = typist.selected_text().await.map_err(said)?;
                    if picked.trim().is_empty() {
                        return Err(ToolExecutionError::other(
                            "Nothing is selected. Ask them what to search for.",
                        ));
                    }
                    picked
                }
                (Some(query), false) => query.to_string(),
                (None, false) => {
                    return Err(ToolExecutionError::invalid_args(
                        "give the query -- what to search for",
                    ));
                }
            };
            let url = search_address(site, &query)?;
            here.desk.open_url(url.clone()).await.map_err(said)?;
            Ok(ToolOutput::json(json!({
                "ok": true,
                "searched": site.name,
                "for": query.chars().take(120).collect::<String>(),
                "note": "Their browser is showing the results. Say so in a few words; you \
                         have not seen them.",
            })))
        })
    })
}

/// Whether a `search_site` call searches the selection rather than a query.
pub fn selected(arguments: &Value) -> bool {
    arguments.get("selection").and_then(Value::as_bool).unwrap_or(false)
}

/// The address a `search_site` call opens, or `None` when it has no query
/// of its own (it searches the selection) or names no site there is. Read by
/// the gate, which shows it in the question it asks.
pub fn search_address_of(sites: &[SearchSite], arguments: &Value) -> Option<String> {
    if selected(arguments) {
        return None;
    }
    let site = core_desk::find_site(sites, text(arguments, "site")?)?;
    core_desk::search_url(site, text(arguments, "query")?).ok()
}

fn search_address(site: &SearchSite, query: &str) -> Result<String, ToolExecutionError> {
    core_desk::search_url(site, query).map_err(|e| ToolExecutionError::invalid_args(e.to_string()))
}

// ---- media, volume, settings, lock, copy -------------------------------

fn control_media_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        CONTROL_MEDIA,
        "Control the music, podcast or video playing on this computer: play, pause, \
         toggle (play or pause), next, previous, or status to only say what is playing.",
        object(
            json!({
                "action": {
                    "type": "string",
                    "enum": ["play", "pause", "toggle", "next", "previous", "status"],
                }
            }),
            &["action"],
        ),
        move |arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let action: MediaAction = arguments
                    .get("action")
                    .cloned()
                    .and_then(|a| serde_json::from_value(a).ok())
                    .ok_or_else(|| ToolExecutionError::invalid_args("action is one of the six"))?;
                match here.desk.media(action).await.map_err(said)? {
                    None => Err(ToolExecutionError::other(
                        "Nothing that plays music or video is running.",
                    )),
                    Some(now) => Ok(ToolOutput::json(json!({
                        "ok": true,
                        "player": now.player,
                        "playing": now.playing,
                        "title": now.title,
                        "artist": now.artist,
                        "note": SAY_BRIEFLY,
                    }))),
                }
            })
        },
    )
}

fn set_volume_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        SET_VOLUME,
        "Change how loud this computer plays: up or down (by `percent` points, or a step when \
         not given), set to `percent`, mute, unmute, or status to only say where it is.",
        object(
            json!({
                "action": {
                    "type": "string",
                    "enum": ["up", "down", "set", "mute", "unmute", "status"],
                },
                "percent": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "description": "For set, the level; for up and down, how many points.",
                }
            }),
            &["action"],
        ),
        move |arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let change = volume_change(&arguments)?;
                let now = here.desk.volume(change).await.map_err(said)?;
                Ok(ToolOutput::json(json!({
                    "ok": true,
                    "percent": now.percent,
                    "muted": now.muted,
                    "note": SAY_BRIEFLY,
                })))
            })
        },
    )
}

fn volume_change(arguments: &Value) -> Result<VolumeChange, ToolExecutionError> {
    let percent = match arguments.get("percent") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            whole_number(value)
                .filter(|p| *p <= 100)
                .map(|p| p as u8)
                .ok_or_else(|| ToolExecutionError::invalid_args("percent is 0 to 100"))?,
        ),
    };
    Ok(match text(arguments, "action").unwrap_or_default() {
        "up" => VolumeChange::Up(percent),
        "down" => VolumeChange::Down(percent),
        "set" => VolumeChange::Set(
            percent.ok_or_else(|| ToolExecutionError::invalid_args("set needs a percent"))?,
        ),
        "mute" => VolumeChange::Mute,
        "unmute" => VolumeChange::Unmute,
        "status" => VolumeChange::Status,
        _ => return Err(ToolExecutionError::invalid_args("action is one of the six")),
    })
}

fn desktop_setting_tool(here: Here, settings: &[DeskSetting]) -> PortableDynamicTool {
    let keys: Vec<&str> = settings.iter().map(|s| s.key()).collect();
    let labels: Vec<&str> = settings.iter().map(|s| s.label()).collect();
    let offered = settings.to_vec();
    PortableDynamicTool::new(
        DESKTOP_SETTING,
        format!(
            "Switch one of the desktop's own settings on or off -- {} -- or leave out `on` to \
             only say whether it is on.",
            labels.join(", ")
        ),
        object(
            json!({
                "setting": { "type": "string", "enum": keys },
                "on": { "type": "boolean" }
            }),
            &["setting"],
        ),
        move |arguments: Value| {
            let here = here.clone();
            let offered = offered.clone();
            Box::pin(async move {
                let setting = offered
                    .iter()
                    .copied()
                    .find(|s| text(&arguments, "setting") == Some(s.key()))
                    .ok_or_else(|| {
                        ToolExecutionError::invalid_args("not a setting offered here")
                    })?;
                let on = arguments.get("on").and_then(Value::as_bool);
                let now = here.desk.setting(setting, on).await.map_err(said)?;
                Ok(ToolOutput::json(json!({
                    "ok": true,
                    "setting": setting.label(),
                    "on": now,
                    "note": SAY_BRIEFLY,
                })))
            })
        },
    )
}

fn lock_screen_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        LOCK_SCREEN,
        "Lock this computer's screen, at once.",
        object(json!({}), &[]),
        move |_arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                here.desk.lock_screen().await.map_err(said)?;
                Ok(ToolOutput::json(json!({ "ok": true, "note": SAY_BRIEFLY })))
            })
        },
    )
}

fn copy_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        COPY_TO_CLIPBOARD,
        "Put text on the clipboard, for them to paste wherever they like: \"copy that\", \"copy \
         the address\". Give exactly the text to copy.",
        object(json!({ "text": { "type": "string" } }), &["text"]),
        move |arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let Some(text) =
                    arguments.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty())
                else {
                    return Err(ToolExecutionError::invalid_args("there is nothing to copy"));
                };
                let length = text.chars().count();
                if length > MAX_COPIED_CHARS {
                    return Err(ToolExecutionError::invalid_args(format!(
                        "that is {length} characters; at most {MAX_COPIED_CHARS} are copied at once"
                    )));
                }
                here.desk.copy(text.to_string()).await.map_err(said)?;
                Ok(ToolOutput::json(json!({ "ok": true, "note": SAY_BRIEFLY })))
            })
        },
    )
}

// ---- timers -------------------------------------------------------------

fn set_timer_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        SET_TIMER,
        "Set a timer that rings on this computer: after a length (hours, minutes, seconds, \
         added together) or at a time today or tomorrow (`at`, 24-hour HH:MM, for \"an alarm at \
         seven\" or \"wake me at 6:30\"). Up to a day, and only while the app is open -- for \
         longer, or anything that must not be missed, set a reminder on a task instead.",
        object(
            json!({
                "hours": { "type": "number", "minimum": 0 },
                "minutes": { "type": "number", "minimum": 0 },
                "seconds": { "type": "number", "minimum": 0 },
                "at": { "type": "string", "description": "A time to ring at, 24-hour HH:MM." },
                "label": {
                    "type": "string",
                    "description": "What it is for, in their words, when they said: \"pasta\".",
                }
            }),
            &[],
        ),
        move |arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let now = zoned_now(&here);
                let length = timer_length(&arguments)?;
                let ends = core_desk::timer_ends(&now, length, text(&arguments, "at"))
                    .map_err(|e| ToolExecutionError::invalid_args(e.to_string()))?;
                let label = text(&arguments, "label").map(str::to_string);
                let timer = here
                    .service
                    .timers
                    .set(here.desk.clone(), here.service.clock(), label, ends, &here.zone)
                    .map_err(said)?;
                Ok(ToolOutput::json(json!({
                    "ok": true,
                    "timer": described(&timer, &now),
                    "note": "It is set. Say so in a few words -- \"Ten-minute timer set.\", \
                             \"Alarm set for 7.\"",
                })))
            })
        },
    )
}

/// The length a `set_timer` call gives, `None` when it gives none -- or
/// gives only zeros, which is a model filling in every field of the schema
/// for "an alarm at seven", not a timer for no time.
fn timer_length(arguments: &Value) -> Result<Option<SignedDuration>, ToolExecutionError> {
    let mut seconds = 0.0_f64;
    let mut any = false;
    for (key, unit) in [("hours", 3600.0), ("minutes", 60.0), ("seconds", 1.0)] {
        let Some(value) = arguments.get(key).filter(|v| !v.is_null()) else { continue };
        let amount = value.as_f64().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(|| {
            ToolExecutionError::invalid_args(format!("{key} is a number, 0 or more"))
        })?;
        seconds += amount * unit;
        any = true;
    }
    if !any || seconds == 0.0 {
        return Ok(None);
    }
    // Rounded to the second, and clamped so a model's 1e300 is refused as too
    // long rather than overflowing.
    let seconds = seconds.round().min(10.0 * 86_400.0) as i64;
    Ok(Some(SignedDuration::from_secs(seconds)))
}

fn list_timers_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        LIST_TIMERS,
        "The timers running, soonest first, with how long each has left.",
        object(json!({}), &[]),
        move |_arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let now = zoned_now(&here);
                let timers: Vec<Value> =
                    here.service.timers.list().iter().map(|t| described(t, &now)).collect();
                Ok(ToolOutput::json(json!({ "count": timers.len(), "timers": timers })))
            })
        },
    )
}

fn cancel_timer_tool(here: Here) -> PortableDynamicTool {
    PortableDynamicTool::new(
        CANCEL_TIMER,
        "Stop a timer before it rings, by its id -- or all of them. With neither, the only one \
         running, if there is just one.",
        object(
            json!({
                "id": { "type": "integer", "minimum": 1 },
                "all": { "type": "boolean" }
            }),
            &[],
        ),
        move |arguments: Value| {
            let here = here.clone();
            Box::pin(async move {
                let now = zoned_now(&here);
                let timers = &here.service.timers;
                let id = match arguments.get("id") {
                    None | Some(Value::Null) => None,
                    // An id it cannot read is not "no id": with one timer
                    // running, that would cancel it whichever was meant.
                    Some(value) => Some(whole_number(value).ok_or_else(|| {
                        ToolExecutionError::invalid_args("id is a timer's number, from list_timers")
                    })?),
                };
                let stopped: Vec<Timer> =
                    if arguments.get("all").and_then(Value::as_bool) == Some(true) {
                        timers.cancel_all()
                    } else if let Some(id) = id {
                        let id = u32::try_from(id).unwrap_or(u32::MAX);
                        vec![timers.cancel(id).ok_or_else(|| {
                            ToolExecutionError::other(format!(
                                "There is no timer {id} running; list_timers says which are."
                            ))
                        })?]
                    } else {
                        match timers.list().as_slice() {
                            [] => Vec::new(),
                            [only] => timers.cancel(only.id).into_iter().collect(),
                            several => {
                                let which: Vec<Value> =
                                    several.iter().map(|t| described(t, &now)).collect();
                                return Err(ToolExecutionError::other(format!(
                                    "{} timers are running: {}. Ask which, then cancel it by id.",
                                    several.len(),
                                    serde_json::to_string(&which).unwrap_or_default()
                                )));
                            }
                        }
                    };
                if stopped.is_empty() {
                    return Err(ToolExecutionError::other("No timer is running."));
                }
                let stopped: Vec<Value> = stopped.iter().map(|t| described(t, &now)).collect();
                Ok(ToolOutput::json(
                    json!({ "ok": true, "cancelled": stopped, "note": SAY_BRIEFLY }),
                ))
            })
        },
    )
}

/// Now, where the person is: their own zone, or the machine's when theirs
/// is not one jiff knows.
fn zoned_now(here: &Here) -> Zoned {
    let now = here.service.now();
    now.in_tz(&here.zone)
        .or_else(|_| now.in_tz(&karmayoga_core::model::system_tz()))
        .unwrap_or_else(|_| now.to_zoned(jiff::tz::TimeZone::UTC))
}

/// A timer as the model is told it: its id, what it is for, when it rings
/// in the person's own time (with the day, when that is not today) and how
/// long it has left.
fn described(timer: &Timer, now: &Zoned) -> Value {
    let rings = timer.ends.to_zoned(now.time_zone().clone());
    let at = if rings.date() == now.date() {
        rings.strftime("%H:%M").to_string()
    } else {
        rings.strftime("%H:%M tomorrow").to_string()
    };
    json!({
        "id": timer.id,
        "label": timer.label,
        "rings_at": at,
        "left": left(now.timestamp(), timer.ends),
    })
}

/// How long from `now` to `ends`, as a person says it -- "9 minutes 30
/// seconds", "1 hour 5 minutes" ([`super::spoken_length`]) -- or "under a
/// second".
fn left(now: Timestamp, ends: Timestamp) -> String {
    super::spoken_length(now.duration_until(ends).as_secs())
        .unwrap_or_else(|| "under a second".into())
}

/// A whole number, 0 or more, however a provider wrote it: `2`, `2.0` or
/// `"2"`. `None` for anything else -- a fraction, a negative, a word.
fn whole_number(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    if let Some(f) = value.as_f64() {
        return (f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= f64::from(u32::MAX))
            .then_some(f as u64);
    }
    value.as_str().and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_web_address_is_opened_in_the_browser() {
        assert_eq!(web_address("https://example.com/a?b=c").unwrap(), "https://example.com/a?b=c");
        assert!(web_address("http://example.com").is_ok());
        assert!(web_address("file:///etc/passwd").is_err());
        assert!(web_address("javascript:alert(1)").is_err());
        assert!(web_address("example.com").is_err());
        assert!(web_address("mailto:a@example.com").is_err());
        // What the gate cannot read as an address, it never asked about; so
        // the tool does not open it either.
        assert!(web_address("https://a@evil.example/?q=journal").is_err());
        assert!(web_address("https:evil.example/?q=journal").is_err());
        assert!(web_address("https:\\\\evil.example/?q=journal").is_err());
    }

    #[test]
    fn a_section_is_found_by_its_key_or_what_people_call_it() {
        assert_eq!(section_named("todo"), Some("todo"));
        assert_eq!(section_named("Tasks"), Some("todo"));
        assert_eq!(section_named("to-do list"), Some("todo"));
        assert_eq!(section_named("Screen time"), Some("screentime"));
        assert_eq!(section_named("inbox"), Some("mail"));
        assert_eq!(section_named("podcasts"), Some("feed"));
        assert_eq!(section_named("spreadsheets"), None);
    }

    #[test]
    fn a_timer_length_adds_its_parts_and_rounds_to_the_second() {
        let length = |v: Value| timer_length(&v).unwrap();
        assert_eq!(length(json!({})), None);
        // Every field filled with zeros, beside an `at`, is no length at all.
        assert_eq!(length(json!({ "hours": 0, "minutes": 0, "seconds": 0 })), None);
        assert_eq!(length(json!({ "minutes": 10 })), Some(SignedDuration::from_mins(10)));
        assert_eq!(
            length(json!({ "hours": 1, "minutes": 1.5, "seconds": 2 })),
            Some(SignedDuration::from_secs(3600 + 90 + 2))
        );
        assert!(timer_length(&json!({ "minutes": -1 })).is_err());
        assert!(timer_length(&json!({ "minutes": "ten" })).is_err());
        // Clamped well past a day, so the core refuses it as too long.
        assert!(length(json!({ "hours": 1e300 })).unwrap() > core_desk::LONGEST_TIMER);
    }

    #[test]
    fn the_volume_is_changed_as_asked() {
        let change = |v: Value| volume_change(&v).unwrap();
        assert_eq!(change(json!({ "action": "up" })), VolumeChange::Up(None));
        assert_eq!(
            change(json!({ "action": "down", "percent": 20 })),
            VolumeChange::Down(Some(20))
        );
        assert_eq!(change(json!({ "action": "set", "percent": 30 })), VolumeChange::Set(30));
        assert_eq!(change(json!({ "action": "set", "percent": 30.0 })), VolumeChange::Set(30));
        assert_eq!(change(json!({ "action": "set", "percent": "30" })), VolumeChange::Set(30));
        assert!(volume_change(&json!({ "action": "set", "percent": 30.5 })).is_err());
        assert!(volume_change(&json!({ "action": "set" })).is_err());
        assert!(volume_change(&json!({ "action": "set", "percent": 150 })).is_err());
        assert!(volume_change(&json!({ "action": "louder" })).is_err());
    }

    #[test]
    fn a_whole_number_is_read_however_it_was_written() {
        assert_eq!(whole_number(&json!(2)), Some(2));
        assert_eq!(whole_number(&json!(2.0)), Some(2));
        assert_eq!(whole_number(&json!(" 2 ")), Some(2));
        assert_eq!(whole_number(&json!(2.5)), None);
        assert_eq!(whole_number(&json!(-1)), None);
        assert_eq!(whole_number(&json!("two")), None);
        assert_eq!(whole_number(&json!(true)), None);
    }

    /// The guidance names a tool exactly when [`tools`] would offer it, for
    /// a desk that can do everything, nothing, or the macOS three.
    #[test]
    fn the_guidance_names_only_the_tools_this_desk_offers() {
        use crate::desk::DeskSetting;
        let everything = Abilities {
            apps: true,
            files: true,
            sections: true,
            media: true,
            volume: true,
            settings: DeskSetting::ALL.to_vec(),
            lock: true,
            copy: true,
        };
        let elsewhere =
            Abilities { files: true, sections: true, copy: true, ..Abilities::default() };
        for can in [everything, elsewhere, Abilities::default()] {
            let said = guidance(&can);
            let offered = [
                (CONTROL_MEDIA, can.media),
                (SET_VOLUME, can.volume),
                (DESKTOP_SETTING, !can.settings.is_empty()),
                (LOCK_SCREEN, can.lock),
                (COPY_TO_CLIPBOARD, can.copy),
                (SEARCH_SITE, true),
                (SET_TIMER, true),
                (LIST_TIMERS, true),
                (CANCEL_TIMER, true),
            ];
            for (tool, offered) in offered {
                assert_eq!(said.contains(tool), offered, "{tool} for {can:?}: {said}");
            }
            assert_eq!(said.contains("an application installed"), can.apps, "{said}");
            assert_eq!(
                said.contains("Night Light"),
                can.settings.contains(&DeskSetting::NightLight)
            );
        }
    }

    #[test]
    fn what_is_left_is_said_as_a_person_says_it() {
        let now = Timestamp::UNIX_EPOCH;
        let after = |s: i64| now.checked_add(SignedDuration::from_secs(s)).unwrap();
        assert_eq!(left(now, now), "under a second");
        assert_eq!(left(now, after(1)), "1 second");
        assert_eq!(left(now, after(600)), "10 minutes");
        assert_eq!(left(now, after(570)), "9 minutes 30 seconds");
        assert_eq!(left(now, after(3600 + 300 + 12)), "1 hour 5 minutes");
        assert_eq!(left(now, after(7200)), "2 hours");
    }

    #[test]
    fn the_gate_is_shown_the_address_a_search_opens() {
        let sites = core_desk::all_sites(&[]);
        assert_eq!(
            search_address_of(&sites, &json!({ "site": "Amazon", "query": "usb c cable" })),
            Some("https://www.amazon.com/s?k=usb%20c%20cable".into())
        );
        assert_eq!(
            search_address_of(&sites, &json!({ "site": "Amazon", "selection": true })),
            None
        );
        assert_eq!(search_address_of(&sites, &json!({ "site": "Nowhere", "query": "x" })), None);
    }
}
