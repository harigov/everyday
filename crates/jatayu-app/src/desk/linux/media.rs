//! Whatever is playing -- music, a video, a podcast -- through MPRIS.
//!
//! MPRIS is the one interface every Linux media player has agreed on: a
//! player puts `org.mpris.MediaPlayer2.<name>` on the session bus, with the
//! controls on `org.mpris.MediaPlayer2.Player` at `/org/mpris/MediaPlayer2`,
//! and GNOME's own media keys and its shell's player widget speak to it the
//! same way. Spotify, VLC, mpv with a script, Rhythmbox, and a browser with a
//! video or a tab of music in it all appear there. So "pause the music" is
//! not a guess at which application; it is asking the session who is playing.
//!
//! # Which player
//!
//! There may be several -- Spotify and a browser tab, or six browser tabs.
//! The one to act on is chosen by [`choose`]: whoever is playing, and if none
//! is, whoever was paused (so "play" resumes what was stopped), and only
//! then any player at all. Of the ones that tie, the first by bus name, so
//! that the same request is answered the same way twice. A `playerctld` --
//! the daemon that mirrors whichever player was last used, for the command
//! line -- is passed over for the players it mirrors.
//!
//! # What is said afterwards
//!
//! A player answers a control call before it has finished changing -- `Next`
//! returns as the next track is asked for, not as it begins -- so the state
//! is read back after [`SETTLE`], and what the model is told is what is
//! playing *now*, not what was asked for. A player that would not do what it
//! was asked (it cannot go back, there is no next track) is said to have
//! declined, by its own name.

use std::collections::HashMap;
use std::time::Duration;

use jatayu_service::desk::{MediaAction, NowPlaying};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// How long a player is given to start doing what it was asked before it is
/// asked what it is doing.
const SETTLE: Duration = Duration::from_millis(250);

/// How long the whole exchange gets. A player that does not answer is hung,
/// and a person waiting on a spoken command would rather be told so.
const PATIENCE: Duration = Duration::from_secs(6);

/// How long one player gets to say whether it is playing, so that one hung
/// player does not keep the others from being asked.
const GLANCE: Duration = Duration::from_secs(1);

/// What a player says it is doing: MPRIS's `PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::desk) enum Status {
    Playing,
    Paused,
    Stopped,
    /// It would not say.
    Unknown,
}

impl Status {
    fn parse(text: &str) -> Status {
        match text {
            "Playing" => Status::Playing,
            "Paused" => Status::Paused,
            "Stopped" => Status::Stopped,
            _ => Status::Unknown,
        }
    }
}

/// Do `action` to what is playing, and say what is playing afterwards --
/// `None` when no player is running.
pub(in crate::desk) async fn control(action: MediaAction) -> Result<Option<NowPlaying>, String> {
    match tokio::time::timeout(PATIENCE, act(action)).await {
        Ok(result) => result,
        Err(_) => Err("The player didn't answer.".to_string()),
    }
}

async fn act(action: MediaAction) -> Result<Option<NowPlaying>, String> {
    let connection = Connection::session().await.map_err(|e| {
        tracing::debug!(error = %e, "no session bus to find a media player on");
        "I can't reach the media players on this computer.".to_string()
    })?;
    let mut names = player_names(&connection).await?;
    names.sort();
    // Every player asked at once, each held to its own glance: one after
    // another, a handful of hung ones would spend the whole of `PATIENCE`
    // before the one that is playing was even asked.
    let glances = names.into_iter().map(|name| {
        let connection = &connection;
        async move {
            let status = match tokio::time::timeout(GLANCE, status_of(connection, &name)).await {
                Ok(Ok(status)) => status,
                _ => Status::Unknown,
            };
            (name, status)
        }
    });
    let mut seen: Vec<(String, Status)> = futures_util::future::join_all(glances).await;
    let Some(chosen) = choose(&seen) else { return Ok(None) };
    let name = seen.swap_remove(chosen).0;
    let identity = identity_of(&connection, &name).await;

    if let Some(method) = method_for(action) {
        let proxy = proxy(&connection, &name, PLAYER)
            .await
            .map_err(|e| unreachable_player(&identity, &e))?;
        proxy.call::<_, _, ()>(method, &()).await.map_err(|e| {
            tracing::debug!(error = %e, player = %name, method, "a player declined");
            format!("{identity} wouldn't do that.")
        })?;
        tokio::time::sleep(SETTLE).await;
    }

    let status =
        status_of(&connection, &name).await.map_err(|e| unreachable_player(&identity, &e))?;
    let (title, artist) = match metadata_of(&connection, &name).await {
        Ok(metadata) => text_of(&metadata),
        Err(e) => {
            tracing::debug!(error = %e, player = %name, "a player would not say what it plays");
            (None, None)
        }
    };
    Ok(Some(NowPlaying { player: identity, playing: status == Status::Playing, title, artist }))
}

/// The MPRIS method for `action`, or `None` for one that changes nothing.
fn method_for(action: MediaAction) -> Option<&'static str> {
    match action {
        MediaAction::Play => Some("Play"),
        MediaAction::Pause => Some("Pause"),
        MediaAction::Toggle => Some("PlayPause"),
        MediaAction::Next => Some("Next"),
        MediaAction::Previous => Some("Previous"),
        MediaAction::Status => None,
    }
}

fn unreachable_player(identity: &str, error: &zbus::Error) -> String {
    tracing::debug!(error = %error, player = identity, "a player stopped answering");
    format!("{identity} isn't answering.")
}

/// A proxy to `interface` of the player at `name`, which asks the player
/// each time instead of keeping what it was told last: this is read once,
/// after a change, and a cached answer is the one from before it.
async fn proxy(
    connection: &Connection,
    name: &str,
    interface: &'static str,
) -> zbus::Result<zbus::Proxy<'static>> {
    zbus::proxy::Builder::<zbus::Proxy>::new(connection)
        .destination(name.to_string())?
        .path(PATH)?
        .interface(interface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// Every MPRIS player's bus name.
async fn player_names(connection: &Connection) -> Result<Vec<String>, String> {
    let bus = zbus::fdo::DBusProxy::new(connection).await.map_err(|e| {
        tracing::debug!(error = %e, "could not talk to the session bus");
        "I can't reach the media players on this computer.".to_string()
    })?;
    let names = bus.list_names().await.map_err(|e| {
        tracing::debug!(error = %e, "could not list the session bus's names");
        "I can't reach the media players on this computer.".to_string()
    })?;
    Ok(players_among(names.iter().map(|name| name.as_str())))
}

/// The MPRIS players among `names` -- every name on a bus, well known or not.
fn players_among<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| name.strip_prefix(PREFIX).is_some_and(|rest| !rest.is_empty()))
        .map(str::to_string)
        .collect()
}

async fn status_of(connection: &Connection, name: &str) -> zbus::Result<Status> {
    let text: String =
        proxy(connection, name, PLAYER).await?.get_property("PlaybackStatus").await?;
    Ok(Status::parse(&text))
}

async fn metadata_of(
    connection: &Connection,
    name: &str,
) -> zbus::Result<HashMap<String, OwnedValue>> {
    proxy(connection, name, PLAYER).await?.get_property("Metadata").await
}

/// What the player calls itself -- "Spotify", "Mozilla Firefox" -- or, for
/// one that will not say, what its bus name makes of it.
async fn identity_of(connection: &Connection, name: &str) -> String {
    let asked =
        async { proxy(connection, name, ROOT).await?.get_property::<String>("Identity").await };
    match tokio::time::timeout(GLANCE, asked).await {
        Ok(Ok(identity)) if !identity.trim().is_empty() => identity.trim().to_string(),
        _ => friendly(name),
    }
}

/// A player's bus name as a name for it: `org.mpris.MediaPlayer2.spotify`
/// is "Spotify", and a second instance of one --
/// `org.mpris.MediaPlayer2.vlc.instance4242` -- is "Vlc" again.
fn friendly(bus_name: &str) -> String {
    let rest = bus_name.strip_prefix(PREFIX).unwrap_or(bus_name);
    let name = rest.split(".instance").next().unwrap_or(rest);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "The player".to_string(),
    }
}

/// Which of `players` to act on, by position: the first that is playing,
/// else the first that is paused, else the first. See the module doc.
pub(in crate::desk) fn choose(players: &[(String, Status)]) -> Option<usize> {
    let proxies = |name: &str| name.strip_prefix(PREFIX) == Some("playerctld");
    // A mirror of the others is only the answer when it is all there is.
    let real = players.iter().enumerate().filter(|(_, (name, _))| !proxies(name));
    let candidates: Vec<usize> = if real.clone().next().is_some() {
        real.map(|(i, _)| i).collect()
    } else {
        (0..players.len()).collect()
    };
    [Status::Playing, Status::Paused]
        .into_iter()
        .find_map(|wanted| candidates.iter().copied().find(|&i| players[i].1 == wanted))
        .or_else(|| candidates.first().copied())
}

/// The title and the first artist out of a player's `Metadata`, which is a
/// dictionary of whatever the player cared to say. `xesam:title` is a
/// string; `xesam:artist` is an array of them, though a player that sends
/// a single string is read too.
fn text_of(metadata: &HashMap<String, OwnedValue>) -> (Option<String>, Option<String>) {
    let first = |key: &str| {
        let mut found = Vec::new();
        if let Some(value) = metadata.get(key) {
            strings(value, &mut found);
        }
        found.into_iter().map(|s| s.trim().to_string()).find(|s| !s.is_empty())
    };
    (first("xesam:title"), first("xesam:artist"))
}

/// Every string in `value`, in order, however deeply it is nested in arrays
/// and variants.
fn strings(value: &Value<'_>, out: &mut Vec<String>) {
    match value {
        Value::Str(text) => out.push(text.as_str().to_string()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Value::Value(inner) => strings(inner, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(name: &str, status: Status) -> (String, Status) {
        (format!("{PREFIX}{name}"), status)
    }

    #[test]
    fn the_players_on_a_bus_are_the_mpris_names() {
        assert_eq!(
            players_among([
                ":1.42",
                "org.freedesktop.Notifications",
                "org.mpris.MediaPlayer2.spotify",
                "org.mpris.MediaPlayer2.firefox.instance_1_77",
                "org.mpris.MediaPlayer2.",
                "org.mpris.MediaPlayer2",
            ]),
            ["org.mpris.MediaPlayer2.spotify", "org.mpris.MediaPlayer2.firefox.instance_1_77"]
        );
    }

    #[test]
    fn whoever_is_playing_is_chosen_first() {
        let players = [
            player("amberol", Status::Paused),
            player("firefox", Status::Playing),
            player("spotify", Status::Playing),
        ];
        assert_eq!(choose(&players), Some(1));
    }

    #[test]
    fn with_nothing_playing_whoever_is_paused_is_chosen() {
        let players = [
            player("amberol", Status::Stopped),
            player("firefox", Status::Unknown),
            player("spotify", Status::Paused),
        ];
        assert_eq!(choose(&players), Some(2));
    }

    #[test]
    fn with_neither_the_first_is_chosen() {
        let players = [player("amberol", Status::Stopped), player("spotify", Status::Stopped)];
        assert_eq!(choose(&players), Some(0));
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn the_daemon_that_mirrors_players_is_not_one_of_them() {
        let players = [player("playerctld", Status::Playing), player("spotify", Status::Paused)];
        assert_eq!(choose(&players), Some(1));
        // Unless it is all there is.
        assert_eq!(choose(&[player("playerctld", Status::Paused)]), Some(0));
    }

    #[test]
    fn a_player_without_a_name_is_named_by_its_bus_name() {
        assert_eq!(friendly("org.mpris.MediaPlayer2.spotify"), "Spotify");
        assert_eq!(friendly("org.mpris.MediaPlayer2.vlc.instance4242"), "Vlc");
        assert_eq!(friendly("org.mpris.MediaPlayer2.firefox.instance_1_77"), "Firefox");
        assert_eq!(friendly("org.mpris.MediaPlayer2."), "The player");
    }

    #[test]
    fn a_status_the_player_does_not_know_is_unknown() {
        assert_eq!(Status::parse("Playing"), Status::Playing);
        assert_eq!(Status::parse("Paused"), Status::Paused);
        assert_eq!(Status::parse("Stopped"), Status::Stopped);
        assert_eq!(Status::parse("Buffering"), Status::Unknown);
    }

    #[test]
    fn only_status_changes_nothing() {
        assert_eq!(method_for(MediaAction::Toggle), Some("PlayPause"));
        assert_eq!(method_for(MediaAction::Previous), Some("Previous"));
        assert_eq!(method_for(MediaAction::Status), None);
    }

    fn owned(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).unwrap()
    }

    #[test]
    fn the_title_and_first_artist_come_out_of_the_metadata() {
        let metadata = HashMap::from([
            ("xesam:title".to_string(), owned(Value::from("Karma Police"))),
            ("xesam:artist".to_string(), owned(Value::from(vec!["Radiohead", "Thom Yorke"]))),
            ("mpris:length".to_string(), owned(Value::from(261_000_000_i64))),
        ]);
        assert_eq!(
            text_of(&metadata),
            (Some("Karma Police".to_string()), Some("Radiohead".to_string()))
        );
    }

    #[test]
    fn an_artist_sent_as_a_single_string_is_read_too() {
        let metadata = HashMap::from([
            ("xesam:title".to_string(), owned(Value::from("  Intro  "))),
            ("xesam:artist".to_string(), owned(Value::from("Boards of Canada"))),
        ]);
        assert_eq!(
            text_of(&metadata),
            (Some("Intro".to_string()), Some("Boards of Canada".to_string()))
        );
    }

    #[test]
    fn nothing_said_is_nothing_reported() {
        assert_eq!(text_of(&HashMap::new()), (None, None));
        let blank = HashMap::from([
            ("xesam:title".to_string(), owned(Value::from(""))),
            ("xesam:artist".to_string(), owned(Value::from(vec!["", " "]))),
        ]);
        assert_eq!(text_of(&blank), (None, None));
    }
}
