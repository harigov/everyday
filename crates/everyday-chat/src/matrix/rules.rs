//! The decisions the Matrix connector makes that need no network: who may be
//! heard, what the bot says about itself, and how this crate's identifiers
//! and messages cross into Matrix's.
//!
//! Kept apart, as plain functions over plain strings, because these are the
//! rules that decide whose words reach the assistant and where its answers
//! can be read. Each one is tested here without a homeserver, so a change to
//! any of them has to get past a test that says what it is for.

use crate::connector::{ChatError, ChatId, MessageRef};
use jiff::Timestamp;
use matrix_sdk::ruma::{EventId, MilliSecondsSinceUnixEpoch, OwnedRoomId, RoomId};
use std::collections::BTreeSet;
use std::time::Duration;

/// Who can read what the bot writes in a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomKind {
    /// The bot and the owner have joined, nobody else has, and nobody else
    /// has been invited. The only kind of room the bot answers in.
    Private,
    /// The bot is there alone and the owner has been invited but has not
    /// joined yet: the room the bot makes for itself and waits in.
    AwaitingOwner,
    /// Anything else. Somebody besides the owner can read, or could once they
    /// accepted.
    Shared,
}

/// Decide what a room is from who is in it.
///
/// `joined` and `invited` are Matrix user ids. An outstanding invitation
/// counts against privacy as well as a joined member: a room's history is
/// usually visible to whoever joins it later, so a third person invited but
/// not yet arrived is already a reader in waiting.
///
/// A link whose bot and owner are the same account (the settings pane refuses
/// to save one) is never private, rather than trivially so.
pub(crate) fn classify_room(bot: &str, owner: &str, joined: &[&str], invited: &[&str]) -> RoomKind {
    if bot == owner {
        return RoomKind::Shared;
    }
    let joined: BTreeSet<&str> = joined.iter().copied().collect();
    let invited: BTreeSet<&str> = invited.iter().copied().collect();
    if joined == BTreeSet::from([bot, owner]) && invited.is_empty() {
        RoomKind::Private
    } else if joined == BTreeSet::from([bot]) && invited == BTreeSet::from([owner]) {
        RoomKind::AwaitingOwner
    } else {
        RoomKind::Shared
    }
}

/// Whether a message may be believed given how it reached us.
///
/// In a room that is encrypted, a message that did not arrive encrypted is
/// not something the owner's client could have sent -- every client encrypts
/// there -- so it is somebody injecting an event with the owner's name on it
/// (Element shows the same thing as "not encrypted"). Rooms that are not
/// encrypted, or not known to be, take whatever arrives; encryption is
/// supported here, not required.
pub(crate) fn arrived_acceptably(room_is_encrypted: bool, arrived_encrypted: bool) -> bool {
    !room_is_encrypted || arrived_encrypted
}

/// What the bot says when it makes a room for itself and the owner.
///
/// `assistant_name` is what the person called their assistant; empty means it
/// has no name.
pub(crate) fn greeting(assistant_name: &str) -> String {
    let name = assistant_name.trim();
    let name = if name.is_empty() { "the assistant" } else { name };
    format!("Hi \u{2014} this is {name}. Ask me anything. `!help` lists commands.")
}

/// The sentence Settings shows while the connector is up.
///
/// `waiting` is true while the room made for the owner has not been joined.
/// `note` is whatever needs attention about the bot's encryption (see
/// `identity`), appended so the person sees it where they will look.
pub(crate) fn online_detail(bot: &str, owner: &str, waiting: bool, note: Option<&str>) -> String {
    let mut detail = if waiting {
        format!("Signed in as {bot}. Waiting for {owner} to accept the invitation.")
    } else {
        format!("Signed in as {bot}. Talking to {owner}.")
    };
    if let Some(note) = note {
        detail.push(' ');
        detail.push_str(note);
    }
    detail
}

/// The sentence the bot sends, once per room, when the owner writes to it
/// somewhere other people can read.
pub(crate) fn not_private_notice(owner: &str) -> String {
    format!("I only answer in a private room with just {owner} and me.")
}

/// A Matrix room, as this crate names a chat.
pub(crate) fn chat_id(room: &RoomId) -> ChatId {
    ChatId(room.to_string())
}

/// A Matrix event, as this crate names a message.
pub(crate) fn message_ref(event: &EventId) -> MessageRef {
    MessageRef(event.to_string())
}

/// The room a chat names. A chat the relay hands back is always one this
/// connector gave it, so failing here means a bug on the caller's side; it is
/// reported as an error rather than a panic so one bad id cannot take the bot
/// down.
pub(crate) fn room_of(chat: &ChatId) -> Result<OwnedRoomId, ChatError> {
    RoomId::parse(&chat.0)
        .map_err(|e| ChatError::Transient(format!("\"{}\" is not a Matrix room id: {e}", chat.0)))
}

/// When an event was sent, by its server's clock. A timestamp too large to
/// represent (which only a broken server sends) reads as now, so the relay
/// treats the message as fresh rather than discarding it as ancient.
pub(crate) fn sent_at(when: MilliSecondsSinceUnixEpoch) -> Timestamp {
    Timestamp::from_millisecond(i64::from(when.get())).unwrap_or_else(|_| Timestamp::now())
}

/// The most Markdown the bot puts in one event.
///
/// Matrix servers refuse an event over 64 KiB, and Markdown is sent twice --
/// as the plain `body` and again as rendered HTML -- so a long answer has to
/// be split long before it nears that. 16 KiB leaves room for the HTML to
/// grow past the Markdown it came from.
pub(crate) const MAX_MARKDOWN_BYTES: usize = 16 * 1024;

/// Cut `markdown` into pieces of at most `limit` bytes to send one after
/// another, at the nicest place that fits: a blank line, else a line end, else
/// a space, else wherever the limit falls (never inside a character). Always
/// returns at least one piece, and nothing but the whitespace at the cuts is
/// lost.
///
/// It does not know about code fences, so a fenced block longer than the
/// limit is cut through its middle. Answers that long are not what the
/// assistant is asked to write for a phone.
pub(crate) fn split_markdown(markdown: &str, limit: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = markdown;
    while rest.len() > limit {
        let cut = cut_point(rest, limit);
        let piece = rest[..cut].trim_end();
        if !piece.is_empty() {
            pieces.push(piece);
        }
        // The cut falls on the separator it chose, which belongs to neither
        // side; whatever follows it keeps its own indentation.
        rest = match rest[cut..].chars().next() {
            Some('\n' | '\r') => rest[cut..].trim_start_matches(['\n', '\r']),
            Some(' ') => rest[cut..].trim_start_matches(' '),
            _ => &rest[cut..],
        };
    }
    if !rest.is_empty() || pieces.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// Where to cut `text`, which is longer than `limit`: a byte index on a
/// character boundary, at most `limit`, and at least one character in.
fn cut_point(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        // `limit` is smaller than the first character: take that one whole.
        return text.chars().next().map_or(text.len(), char::len_utf8);
    }
    let window = &text[..end];
    // Do not cut so early that the pieces come out tiny.
    let earliest = end / 2;
    for separator in ["\n\n", "\n", " "] {
        if let Some(at) = window.rfind(separator)
            && at >= earliest
            && at > 0
        {
            return at;
        }
    }
    end
}

/// How long to wait after the `failures`th failure in a row to reach the
/// server, before trying again: one second, doubling, never more than half a
/// minute.
pub(crate) fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(5);
    Duration::from_secs((1u64 << doublings).min(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const BOT: &str = "@bot:example.org";
    const OWNER: &str = "@me:example.org";

    #[test]
    fn a_room_of_just_the_bot_and_the_owner_is_private() {
        assert_eq!(classify_room(BOT, OWNER, &[BOT, OWNER], &[]), RoomKind::Private);
        // Order and repeats do not matter.
        assert_eq!(classify_room(BOT, OWNER, &[OWNER, BOT, OWNER], &[]), RoomKind::Private);
    }

    #[test]
    fn a_third_person_makes_a_room_shared_whether_joined_or_only_invited() {
        let other = "@other:example.org";
        assert_eq!(classify_room(BOT, OWNER, &[BOT, OWNER, other], &[]), RoomKind::Shared);
        assert_eq!(classify_room(BOT, OWNER, &[BOT, OWNER], &[other]), RoomKind::Shared);
    }

    #[test]
    fn a_room_the_owner_has_not_joined_is_waiting_for_them_not_private() {
        assert_eq!(classify_room(BOT, OWNER, &[BOT], &[OWNER]), RoomKind::AwaitingOwner);
        // Waiting for somebody else is a shared room, not the owner's.
        assert_eq!(classify_room(BOT, OWNER, &[BOT], &["@other:example.org"]), RoomKind::Shared);
        assert_eq!(
            classify_room(BOT, OWNER, &[BOT], &[OWNER, "@other:example.org"]),
            RoomKind::Shared
        );
    }

    #[test]
    fn rooms_without_the_owner_or_without_the_bot_are_never_private() {
        assert_eq!(classify_room(BOT, OWNER, &[BOT], &[]), RoomKind::Shared);
        assert_eq!(classify_room(BOT, OWNER, &[OWNER], &[]), RoomKind::Shared);
        assert_eq!(classify_room(BOT, OWNER, &[], &[]), RoomKind::Shared);
        assert_eq!(classify_room(BOT, OWNER, &[OWNER], &[BOT]), RoomKind::Shared);
    }

    #[test]
    fn an_account_cannot_be_its_own_private_room() {
        assert_eq!(classify_room(BOT, BOT, &[BOT], &[]), RoomKind::Shared);
    }

    #[test]
    fn user_ids_are_compared_exactly() {
        assert_eq!(classify_room(BOT, OWNER, &[BOT, "@ME:example.org"], &[]), RoomKind::Shared);
    }

    #[test]
    fn plain_text_in_an_encrypted_room_is_not_taken_for_the_owners() {
        assert!(arrived_acceptably(true, true));
        assert!(!arrived_acceptably(true, false));
        assert!(arrived_acceptably(false, false));
        assert!(arrived_acceptably(false, true));
    }

    #[test]
    fn the_greeting_names_the_assistant_and_points_at_help() {
        assert_eq!(
            greeting("Juno"),
            "Hi \u{2014} this is Juno. Ask me anything. `!help` lists commands."
        );
        assert!(greeting("").contains("this is the assistant."));
        assert!(greeting("  \n").contains("this is the assistant."));
        assert!(greeting("Juno").contains("!help"));
    }

    #[test]
    fn the_online_sentence_says_who_it_is_talking_to_or_waiting_for() {
        assert_eq!(
            online_detail(BOT, OWNER, false, None),
            "Signed in as @bot:example.org. Talking to @me:example.org."
        );
        assert_eq!(
            online_detail(BOT, OWNER, true, None),
            "Signed in as @bot:example.org. Waiting for @me:example.org to accept the invitation."
        );
        assert_eq!(
            online_detail(BOT, OWNER, false, Some("Not verified.")),
            "Signed in as @bot:example.org. Talking to @me:example.org. Not verified."
        );
    }

    #[test]
    fn rooms_and_events_cross_between_the_crates_ids_and_matrixs() {
        let chat = ChatId("!room:example.org".into());
        let room = room_of(&chat).unwrap();
        assert_eq!(chat_id(&room), chat);

        let event = EventId::parse("$event:example.org").unwrap();
        assert_eq!(message_ref(&event), MessageRef("$event:example.org".into()));
    }

    #[test]
    fn something_that_is_not_an_id_is_an_error_not_a_panic() {
        assert!(matches!(room_of(&ChatId("nonsense".into())), Err(ChatError::Transient(_))));
    }

    #[test]
    fn a_send_time_becomes_the_same_instant() {
        let when = MilliSecondsSinceUnixEpoch::from_system_time(
            UNIX_EPOCH + Duration::from_millis(1_700_000_123_456),
        )
        .unwrap();
        assert_eq!(sent_at(when).as_millisecond(), 1_700_000_123_456);
    }

    #[test]
    fn short_text_is_one_piece_and_empty_text_is_too() {
        assert_eq!(split_markdown("hello", 100), ["hello"]);
        assert_eq!(split_markdown("", 100), [""]);
    }

    #[test]
    fn long_text_is_cut_at_paragraphs_first() {
        let text = "aaaa aaaa\n\nbbbb bbbb\n\ncccc cccc";
        let pieces = split_markdown(text, 12);
        assert_eq!(pieces, ["aaaa aaaa", "bbbb bbbb", "cccc cccc"]);
    }

    #[test]
    fn text_without_paragraphs_is_cut_at_lines_then_spaces_then_anywhere() {
        assert_eq!(split_markdown("aaaa\nbbbb\ncccc", 10), ["aaaa\nbbbb", "cccc"]);
        assert_eq!(split_markdown("aaaa bbbb cccc", 10), ["aaaa bbbb", "cccc"]);
        assert_eq!(split_markdown("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn splitting_never_cuts_a_character_and_loses_nothing_but_whitespace() {
        let text = "héllo wörld — ünïcode ✅ text without any newline at all, ".repeat(40);
        for limit in [1, 2, 3, 7, 50, 333] {
            let pieces = split_markdown(&text, limit);
            assert!(pieces.iter().all(|p| p.len() <= limit.max(4)), "limit {limit}");
            assert!(pieces.iter().all(|p| !p.is_empty()), "limit {limit}");
            let squash = |s: &str| s.split_whitespace().collect::<String>();
            assert_eq!(squash(&pieces.concat()), squash(&text), "limit {limit}");
        }
    }

    #[test]
    fn the_retry_delay_doubles_and_stops_at_half_a_minute() {
        let seconds: Vec<u64> = (1..=8).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_delay(0).as_secs(), 1);
    }
}
