//! The decisions the Matrix connector makes that need no network: who may be
//! heard -- the owner's id, from one of the devices the owner picked, over
//! encryption -- what the bot says about itself, and how this crate's
//! identifiers and messages cross into Matrix's.
//!
//! Kept apart, as plain functions over plain strings, because these are the
//! rules that decide whose words reach the assistant and where its answers
//! can be read. Each one is tested here without a homeserver, so a change to
//! any of them has to get past a test that says what it is for.

use crate::connector::{ChatError, ChatId, MessageRef};
use jiff::Timestamp;
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::ruma::{EventId, MilliSecondsSinceUnixEpoch, OwnedRoomId, RoomId};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

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

/// The ids of the owner's devices the bot trusts: the list picked by hand in
/// Settings → Chat, and nothing else.
///
/// Being cross-signed, or verified by comparing emoji, does not put a device
/// here. The owner's id is not enough to be believed -- anybody who signs in
/// to that account on a new device, or runs the homeserver, can write under
/// it -- so what is believed is what a device the owner chose said, and what
/// is given the bot's keys is those devices. Empty trusts no device, which is
/// what a bot has until the owner has picked one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TrustedDevices(BTreeSet<String>);

impl TrustedDevices {
    /// The list `ids` make: each trimmed, the empty ones left out.
    pub(crate) fn new<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self(
            ids.into_iter()
                .map(|id| id.as_ref().trim().to_owned())
                .filter(|id| !id.is_empty())
                .collect(),
        )
    }

    /// Whether `device` is on the list. Exact: a device id is not a name to
    /// be matched loosely.
    pub(crate) fn contains(&self, device: &str) -> bool {
        self.0.contains(device)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How an event from the owner's account reached the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival<'a> {
    /// In plain text.
    Plain,
    /// Encrypted, and decrypted. `device` is the device that sent it, as far
    /// as the encryption can say: `None` when it cannot name one.
    Encrypted { device: Option<&'a str> },
}

/// How an event arrived, from what the SDK says of how it was decrypted: no
/// [`EncryptionInfo`] means it was not.
pub(crate) fn arrival_of(encryption: Option<&EncryptionInfo>) -> Arrival<'_> {
    let Some(info) = encryption else {
        return Arrival::Plain;
    };
    Arrival::Encrypted { device: info.sender_device.as_deref().map(|d| d.as_str()) }
}

/// What to make of an event that claims to be the owner's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Welcome {
    /// Believe it: encrypted, from a device on the list.
    Accepted,
    /// It arrived in plain text in a room that is encrypted. See
    /// [`arrived_acceptably`]: somebody is writing the owner's name on it.
    Forged,
    /// It arrived in plain text in a room that is not encrypted. The bot
    /// answers nowhere that is not encrypted: there is no telling which of
    /// the owner's devices wrote it, and the reply would be readable to the
    /// server and to anybody it chose.
    Unencrypted,
    /// It arrived encrypted, from a device that is not on the list -- or
    /// that the encryption cannot name, which is no better. Carries the
    /// device when there is one, for Settings to say which it was.
    Untrusted(Option<String>),
}

/// Decide whether an event that carries the owner's id is the owner's words.
///
/// The owner's id alone is not enough: see [`TrustedDevices`]. Typed words,
/// pictures, files and voice notes count only when they arrived encrypted and
/// the device that sent them is on `trusted`. Reactions are not decided here:
/// they are not encrypted and carry no device, and answer a question the bot
/// asked in a room only the two of them are in, which is how they have always
/// been taken (see `hearing`).
///
/// An event that arrived encrypted is in an encrypted room whatever the room's
/// cached state says, so it is `room_is_encrypted` that is overruled, never
/// the other way round.
pub(crate) fn judge_arrival(
    room_is_encrypted: bool,
    arrival: Arrival<'_>,
    trusted: &TrustedDevices,
) -> Welcome {
    let Arrival::Encrypted { device } = arrival else {
        return if arrived_acceptably(room_is_encrypted, false) {
            Welcome::Unencrypted
        } else {
            Welcome::Forged
        };
    };
    match device {
        Some(device) if trusted.contains(device) => Welcome::Accepted,
        Some(device) => Welcome::Untrusted(Some(device.to_owned())),
        None => Welcome::Untrusted(None),
    }
}

/// How often the same untrusted device is reported to Settings. It may write
/// a message a second -- or be somebody trying -- and a pane redrawn and a
/// notification considered at that rate would be noise.
pub(crate) const REPORT_EVERY: Duration = Duration::from_secs(30);

/// How many untrusted devices to remember having reported, at most.
const REPORTED_REMEMBERED: usize = 32;

/// Which untrusted devices were reported to Settings lately, so the same one
/// is not reported again straight away. Plain data, so the rule is tested.
#[derive(Debug, Default)]
pub(crate) struct Reported {
    /// Device (or the empty string for one that could not be named) and when
    /// it was last reported.
    last: Vec<(String, Instant)>,
}

impl Reported {
    /// Whether `device` should be reported now, noting that it is if so.
    pub(crate) fn should_report(&mut self, device: Option<&str>, now: Instant) -> bool {
        let key = device.unwrap_or_default();
        if let Some((_, at)) = self.last.iter_mut().find(|(d, _)| d == key) {
            if now.saturating_duration_since(*at) < REPORT_EVERY {
                return false;
            }
            *at = now;
            return true;
        }
        if self.last.len() >= REPORTED_REMEMBERED {
            self.last.remove(0);
        }
        self.last.push((key.to_owned(), now));
        true
    }
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

/// The sentence the bot sends, once per room, when the owner writes to it in
/// a room that is not encrypted. About the room only: nothing private in it.
pub(crate) fn not_encrypted_notice() -> &'static str {
    "I only answer in encrypted rooms. Start a new encrypted chat with me, or turn encryption on \
     for this room."
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

    fn trusted(ids: &[&str]) -> TrustedDevices {
        TrustedDevices::new(ids)
    }

    #[test]
    fn an_encrypted_message_from_a_trusted_device_is_the_owners() {
        let list = trusted(&["PHONE", "LAPTOP"]);
        for device in ["PHONE", "LAPTOP"] {
            assert_eq!(
                judge_arrival(true, Arrival::Encrypted { device: Some(device) }, &list),
                Welcome::Accepted
            );
        }
    }

    #[test]
    fn a_device_not_on_the_list_is_turned_away_however_it_is_signed() {
        let list = trusted(&["PHONE"]);
        // A device the owner's account has signed is not trusted for that.
        assert_eq!(
            judge_arrival(true, Arrival::Encrypted { device: Some("NEWLAPTOP") }, &list),
            Welcome::Untrusted(Some("NEWLAPTOP".into()))
        );
        // Ids are compared exactly.
        assert_eq!(
            judge_arrival(true, Arrival::Encrypted { device: Some("phone") }, &list),
            Welcome::Untrusted(Some("phone".into()))
        );
        // One the encryption cannot name cannot be on a list.
        assert_eq!(
            judge_arrival(true, Arrival::Encrypted { device: None }, &list),
            Welcome::Untrusted(None)
        );
    }

    #[test]
    fn an_empty_list_trusts_nobody() {
        let none = TrustedDevices::default();
        assert!(none.is_empty());
        for device in [Some("PHONE"), Some(""), None] {
            let welcome = judge_arrival(true, Arrival::Encrypted { device }, &none);
            assert!(matches!(welcome, Welcome::Untrusted(_)), "{device:?}: {welcome:?}");
        }
        // Blank entries are not a device that matches a blank id.
        assert!(trusted(&["", "  "]).is_empty());
        assert!(!trusted(&["", "  "]).contains(""));
    }

    #[test]
    fn plain_text_is_forged_in_an_encrypted_room_and_unanswered_in_any_other() {
        // Even from what would be a trusted device's id: plain text names none.
        let list = trusted(&["PHONE"]);
        assert_eq!(judge_arrival(true, Arrival::Plain, &list), Welcome::Forged);
        assert_eq!(judge_arrival(false, Arrival::Plain, &list), Welcome::Unencrypted);
        let none = TrustedDevices::default();
        assert_eq!(judge_arrival(false, Arrival::Plain, &none), Welcome::Unencrypted);
    }

    #[test]
    fn an_encrypted_arrival_in_a_room_not_yet_known_to_be_encrypted_is_judged_by_its_device() {
        let list = trusted(&["PHONE"]);
        assert_eq!(
            judge_arrival(false, Arrival::Encrypted { device: Some("PHONE") }, &list),
            Welcome::Accepted
        );
        assert_eq!(
            judge_arrival(false, Arrival::Encrypted { device: Some("OTHER") }, &list),
            Welcome::Untrusted(Some("OTHER".into()))
        );
    }

    #[test]
    fn an_arrival_names_the_device_its_encryption_names() {
        use matrix_sdk::deserialized_responses::{AlgorithmInfo, VerificationState};
        use matrix_sdk::ruma::{device_id, user_id};
        let info = |device: Option<&matrix_sdk::ruma::DeviceId>| EncryptionInfo {
            sender: user_id!("@me:example.org").to_owned(),
            sender_device: device.map(ToOwned::to_owned),
            forwarder: None,
            algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
                curve25519_key: String::new(),
                sender_claimed_keys: Default::default(),
                session_id: None,
            },
            verification_state: VerificationState::Verified,
        };
        assert_eq!(arrival_of(None), Arrival::Plain);
        assert_eq!(
            arrival_of(Some(&info(Some(device_id!("PHONE"))))),
            Arrival::Encrypted { device: Some("PHONE") }
        );
        assert_eq!(arrival_of(Some(&info(None))), Arrival::Encrypted { device: None });

        // A device the encryption says is verified is still not a trusted one.
        let nobody = TrustedDevices::default();
        let verified = info(Some(device_id!("PHONE")));
        assert_eq!(
            judge_arrival(true, arrival_of(Some(&verified)), &nobody),
            Welcome::Untrusted(Some("PHONE".into()))
        );
    }

    #[test]
    fn the_list_is_trimmed_and_has_no_repeats() {
        let list = trusted(&[" B ", "A", "B"]);
        assert_eq!(list, trusted(&["B", "A"]));
        assert!(list.contains("A") && list.contains("B") && !list.contains(" B "));
    }

    #[test]
    fn the_same_untrusted_device_is_reported_once_in_a_while() {
        let start = Instant::now();
        let mut reported = Reported::default();
        assert!(reported.should_report(Some("NEW"), start));
        assert!(!reported.should_report(Some("NEW"), start + Duration::from_secs(5)));
        // Another device is its own report.
        assert!(reported.should_report(Some("OTHER"), start + Duration::from_secs(6)));
        assert!(reported.should_report(None, start + Duration::from_secs(7)));
        assert!(!reported.should_report(None, start + Duration::from_secs(8)));
        // And again, once it has been quiet long enough.
        let later = start + REPORT_EVERY;
        assert!(reported.should_report(Some("NEW"), later));
        assert!(!reported.should_report(Some("NEW"), later + Duration::from_secs(1)));
    }

    #[test]
    fn only_so_many_reported_devices_are_remembered() {
        let now = Instant::now();
        let mut reported = Reported::default();
        for n in 0..REPORTED_REMEMBERED * 2 {
            assert!(reported.should_report(Some(&format!("D{n}")), now));
        }
        assert_eq!(reported.last.len(), REPORTED_REMEMBERED);
    }

    #[test]
    fn what_the_bot_says_about_an_unencrypted_room_holds_nothing_private() {
        let said = not_encrypted_notice();
        assert!(said.contains("encrypted"), "{said}");
        assert!(!said.contains('@') && !said.contains('!'), "{said}");
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
