//! Reading RFC 822 bytes into the shape the rest of the crate works with.
//!
//! Everything here is built on [`mail-parser`](https://docs.rs/mail-parser),
//! which is fuzzed against exactly the malformed input a real mailbox
//! produces and documents that it never panics -- it hands back `None`
//! instead. We keep that guarantee: [`parse`] turns `None` into an
//! [`Error`], never a panic, and the proptest at the bottom of this file
//! throws random and truncated bytes at it to say so.
//!
//! A [`ParsedMessage`] is a snapshot, not a live view over the bytes --
//! everything in it is owned, because it is going to be sealed into a vault
//! row long after `raw` has been dropped. [`html`](ParsedMessage::html) and
//! [`text`](ParsedMessage::text) are chosen the way RFC 8621 (JMAP) chooses
//! them: the first `text/html` and first `text/plain` rendition of the
//! message, found by walking `multipart/alternative` and
//! `multipart/related` the way a mail client does, not just the first leaf
//! part in document order. `mail-parser` does that walk for us --
//! `body_html(0)` and `body_text(0)` are its own answer to "what should a
//! client show" -- so this module does not re-implement it.

use mail_parser::{
    Address as RawAddress, ContentType, Message as RawMessage, MessageParser, MessagePart,
    MimeHeaders, PartType,
};

/// Everything that can go wrong turning bytes into a [`ParsedMessage`].
///
/// There is no "malformed" variant with detail, because `mail-parser`
/// doesn't fail on malformed input -- it does its best and returns
/// *something*. The only way in here is bytes with no headers at all, or a
/// part id from a caller that does not match this message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no headers found; this is not an RFC 822 message")]
    Unparseable,
    #[error("message has no part {0}")]
    NoSuchPart(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A name and an address, both optional because a header can have either
/// alone (`<user@example.com>` with no display name) or, in a malformed
/// message, neither.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Address {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// Where a body part sits: shown as part of the message, or offered as a
/// file the person opens separately.
///
/// This mirrors `Content-Disposition`, defaulting to `Inline` when the
/// header is absent -- which is also what makes a `cid:`-referenced image
/// with no disposition header show up inside the body rather than as a
/// chip, matching every mail client's own behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Inline,
    Attachment,
}

/// One leaf part of the MIME tree: not a `multipart/*` container, which has
/// no bytes of its own and nothing a reader would want to address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Stable only for the lifetime of one `raw` buffer: the index
    /// `mail-parser` assigned this part while walking the tree. Round-trips
    /// through [`part_bytes`] and through `cid:` URLs, which is all it
    /// needs to do -- it is never compared across two different messages.
    pub part_id: String,
    /// `type/subtype`, lower-cased by the parser. Falls back to a sensible
    /// default (`text/plain`, `application/octet-stream`, ...) on the rare
    /// message that omits `Content-Type` entirely.
    pub content_type: String,
    pub filename: Option<String>,
    pub size: usize,
    /// The `Content-ID`, angle brackets stripped, so it compares equal to
    /// the `cid:` reference `sanitize.rs` rewrites without either side
    /// needing to remember which one still has the brackets.
    pub content_id: Option<String>,
    pub disposition: Disposition,
}

/// A message, parsed once, owned, and ready to be sealed into a vault row.
#[derive(Debug, Clone, Default)]
pub struct ParsedMessage {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub date: Option<jiff::Timestamp>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: Option<String>,
    /// The list this message was sent through, from `List-Id`. Not parsed
    /// further here -- categorising by it is a later phase's job, and
    /// keeping the raw header means that phase can change its mind about
    /// how to parse it without this module changing.
    pub list_id: Option<String>,
    pub list_unsubscribe: Option<String>,
    /// `Auto-Submitted`, raw (`auto-replied`, `auto-generated`, ...). Absent
    /// on almost every message a person sent by hand.
    pub auto_submitted: Option<String>,
    /// `Precedence`, raw (`bulk`, `junk`, `list`, ...). Older and less
    /// reliable than `Auto-Submitted`, but still worth keeping: some
    /// senders set only this one.
    pub precedence: Option<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub parts: Vec<Part>,
    /// The bytes of a `text/calendar` part, if the message carries an
    /// invitation. Only the first such part -- a message inviting to two
    /// events at once is not a shape any calendar client produces.
    pub calendar: Option<Vec<u8>>,
}

/// Parses RFC 822 bytes into a [`ParsedMessage`].
///
/// Never panics: `mail-parser` is fuzzed against malformed and truncated
/// messages and always returns its best effort or `None`, and everything
/// this function does with that result is infallible pattern matching. See
/// the proptest below.
pub fn parse(raw: &[u8]) -> Result<ParsedMessage> {
    let message = MessageParser::default().parse(raw).ok_or(Error::Unparseable)?;
    Ok(ParsedMessage::from_raw(&message))
}

/// Re-parses `raw` and returns the bytes of the part named `part_id`, as
/// produced by [`parse`]'s `Part::part_id`.
///
/// Re-parsing rather than keeping a borrow alive is deliberate: a
/// `ParsedMessage` is a value the sync engine holds long after `raw` has
/// been flushed to the pack store and re-read later, at which point this is
/// the only way to get at a part's bytes anyway.
pub fn part_bytes(raw: &[u8], part_id: &str) -> Result<Vec<u8>> {
    let message = MessageParser::default().parse(raw).ok_or(Error::Unparseable)?;
    let index: u32 = part_id.parse().map_err(|_| Error::NoSuchPart(part_id.to_string()))?;
    let part = message.part(index).ok_or_else(|| Error::NoSuchPart(part_id.to_string()))?;
    Ok(part.contents().to_vec())
}

impl ParsedMessage {
    fn from_raw(message: &RawMessage<'_>) -> Self {
        let calendar = message
            .parts
            .iter()
            .find(|part| is_calendar_part(part))
            .map(|part| part.contents().to_vec());

        Self {
            message_id: message.message_id().map(str::to_string),
            in_reply_to: message
                .in_reply_to()
                .as_text_list()
                .and_then(|list| list.first())
                .map(|s| normalize_message_id(s))
                .or_else(|| message.in_reply_to().as_text().map(normalize_message_id)),
            references: header_id_list(message.references()),
            date: message
                .date()
                .and_then(|date| jiff::Timestamp::from_second(date.to_timestamp()).ok()),
            from: addresses(message.from()),
            to: message.all_to().flat_map(|a| addresses(Some(a))).collect(),
            cc: message.all_cc().flat_map(|a| addresses(Some(a))).collect(),
            bcc: message.all_bcc().flat_map(|a| addresses(Some(a))).collect(),
            reply_to: addresses(message.reply_to()),
            subject: message.subject().map(str::to_string),
            // `List-Id` and `List-Unsubscribe` are grammatically addresses
            // (RFC 2919's `List-Id` is `display-name <list-id>`), so
            // `mail-parser` parses them that way -- `.as_text()` on the
            // structured accessor is always `None` for these two. The raw
            // header text is what every categorisation rule that reads
            // these actually wants anyway (a `List-Unsubscribe` often holds
            // two comma-separated URLs, which isn't a shape `Address` has
            // room for), so this reads the header, not the address parse.
            list_id: message.header_raw("List-Id").map(|s| s.trim().to_string()),
            list_unsubscribe: message.header_raw("List-Unsubscribe").map(|s| s.trim().to_string()),
            auto_submitted: message.header_raw("Auto-Submitted").map(|s| s.trim().to_string()),
            precedence: message.header_raw("Precedence").map(|s| s.trim().to_string()),
            html: message.body_html(0).map(|body| body.into_owned()),
            text: genuine_text_body(message),
            parts: message.parts.iter().enumerate().filter_map(part_of).collect(),
            calendar,
        }
    }
}

/// `Message::body_text(0)` is JMAP's "best plain-text rendition" rule: when
/// a message has no genuine `text/plain` part, it synthesises one from the
/// HTML by stripping tags. That synthesis is exactly the hole `text.rs`
/// exists to close -- a tag-strip has no notion of `display:none`, and
/// would hand a hidden instruction straight through. So this only ever
/// returns a body that was genuinely `text/plain` on the wire; when the
/// message is HTML-only, `text` stays `None` and `text::html_to_text` (which
/// does strip hidden content) is what produces text from `html` instead.
fn genuine_text_body(message: &RawMessage<'_>) -> Option<String> {
    let part = message.parts.get(*message.text_body.first()? as usize)?;
    match &part.body {
        PartType::Text(text) => Some(text.as_ref().to_string()),
        _ => None,
    }
}

fn header_id_list(value: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    if let Some(list) = value.as_text_list() {
        list.iter().map(|s| normalize_message_id(s)).collect()
    } else if let Some(text) = value.as_text() {
        vec![normalize_message_id(text)]
    } else {
        Vec::new()
    }
}

/// Strips the `<...>` a `Message-ID`, `In-Reply-To` or `References` value
/// is wrapped in, so every comparison in `threading.rs` and every `cid:`
/// lookup works on the same bare form. A value that never had brackets
/// (a sender that got the header slightly wrong) is passed through as-is.
fn normalize_message_id(raw: &str) -> String {
    raw.trim().trim_start_matches('<').trim_end_matches('>').to_string()
}

fn addresses(address: Option<&RawAddress<'_>>) -> Vec<Address> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .map(|addr| Address {
            name: addr.name().map(str::to_string),
            email: addr.address().map(str::to_string),
        })
        .collect()
}

fn is_calendar_part(part: &MessagePart<'_>) -> bool {
    part.content_type().is_some_and(|ct| {
        ct.ctype().eq_ignore_ascii_case("text")
            && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("calendar"))
    })
}

fn part_of((index, part): (usize, &MessagePart<'_>)) -> Option<Part> {
    // Multipart containers hold no bytes of their own -- their children are
    // already in this same list -- so they are not something a reader could
    // ever want to address.
    if matches!(part.body, PartType::Multipart(_)) {
        return None;
    }

    let filename = part
        .content_disposition()
        .and_then(|cd| cd.attribute("filename"))
        .or_else(|| part.content_type().and_then(|ct| ct.attribute("name")))
        .map(str::to_string);

    let disposition = if part.content_disposition().is_some_and(ContentType::is_attachment) {
        Disposition::Attachment
    } else {
        Disposition::Inline
    };

    Some(Part {
        part_id: index.to_string(),
        content_type: content_type_string(part),
        filename,
        size: part.len(),
        content_id: part.content_id().map(normalize_message_id),
        disposition,
    })
}

fn content_type_string(part: &MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(subtype) => format!("{}/{subtype}", ct.ctype()),
            None => ct.ctype().to_string(),
        },
        None => match part.body {
            PartType::Text(_) => "text/plain".to_string(),
            PartType::Html(_) => "text/html".to_string(),
            PartType::Message(_) => "message/rfc822".to_string(),
            _ => "application/octet-stream".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SIMPLE: &[u8] = b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>\r\n\
Subject: Hello\r\n\
Message-ID: <abc123@example.com>\r\n\
Date: Mon, 1 Jan 2024 10:00:00 +0000\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hi Bob.\r\n";

    #[test]
    fn an_html_only_message_does_not_get_a_synthesised_text_body() {
        // mail-parser's own `body_text(0)` would synthesise plain text from
        // the HTML by stripping tags -- with no notion of `display:none` --
        // which is exactly the hole `text::html_to_text` exists to close.
        // `ParsedMessage::text` must stay `None` here so callers fall
        // through to the safe conversion instead of trusting this one.
        let raw = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Html only\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>Visible.</p><div style=\"display:none\">hidden</div>\r\n";
        let parsed = parse(raw).unwrap();
        assert!(parsed.html.is_some());
        assert!(parsed.text.is_none());
    }

    #[test]
    fn list_headers_are_read_even_though_they_are_grammatically_addresses() {
        let raw = b"From: news@example.com\r\n\
To: reader@example.com\r\n\
Subject: Digest\r\n\
List-Id: Example Newsletter <newsletter.example.com>\r\n\
List-Unsubscribe: <https://example.com/unsubscribe>\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello.\r\n";
        let parsed = parse(raw).unwrap();
        assert_eq!(parsed.list_id.as_deref(), Some("Example Newsletter <newsletter.example.com>"));
        assert_eq!(parsed.list_unsubscribe.as_deref(), Some("<https://example.com/unsubscribe>"));
    }

    #[test]
    fn parses_the_headers_a_client_needs() {
        let parsed = parse(SIMPLE).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Hello"));
        assert_eq!(parsed.message_id.as_deref(), Some("abc123@example.com"));
        assert_eq!(parsed.from[0].email.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.to[0].email.as_deref(), Some("bob@example.com"));
        assert_eq!(parsed.text.as_deref(), Some("Hi Bob.\r\n"));
        assert!(parsed.date.is_some());
    }

    #[test]
    fn empty_bytes_are_an_error_not_a_panic() {
        assert!(matches!(parse(b""), Err(Error::Unparseable)));
    }

    #[test]
    fn picks_html_the_jmap_way_when_both_renditions_exist() {
        let raw = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Alt\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
plain body\r\n\
--b\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>html body</p>\r\n\
--b--\r\n";
        let parsed = parse(raw).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("plain body"));
        assert!(parsed.html.as_deref().unwrap().contains("html body"));
    }

    #[test]
    fn an_inline_image_is_findable_by_content_id_and_its_bytes_round_trip() {
        let raw = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Inline\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/html\r\n\
\r\n\
<img src=\"cid:logo@example.com\">\r\n\
--b\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo@example.com>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
aGVsbG8=\r\n\
--b--\r\n";
        let parsed = parse(raw).unwrap();
        let image = parsed
            .parts
            .iter()
            .find(|p| p.content_id.as_deref() == Some("logo@example.com"))
            .expect("inline image part");
        assert_eq!(image.disposition, Disposition::Inline);
        let bytes = part_bytes(raw, &image.part_id).unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn an_attachment_is_marked_as_such_and_keeps_its_filename() {
        let raw = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Attach\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
see attached\r\n\
--b\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
aGVsbG8=\r\n\
--b--\r\n";
        let parsed = parse(raw).unwrap();
        let attachment = parsed
            .parts
            .iter()
            .find(|p| p.disposition == Disposition::Attachment)
            .expect("attachment part");
        assert_eq!(attachment.filename.as_deref(), Some("invoice.pdf"));
        assert_eq!(attachment.content_type, "application/pdf");
    }

    #[test]
    fn a_calendar_part_is_pulled_out_on_its_own() {
        let raw = b"From: a@example.com\r\n\
To: b@example.com\r\n\
Subject: Invite\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
join us\r\n\
--b\r\n\
Content-Type: text/calendar; method=REQUEST\r\n\
\r\n\
BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n\
--b--\r\n";
        let parsed = parse(raw).unwrap();
        let calendar = parsed.calendar.expect("calendar bytes");
        assert!(String::from_utf8_lossy(&calendar).contains("BEGIN:VCALENDAR"));
    }

    #[test]
    fn part_bytes_of_an_unknown_id_is_an_error_not_a_panic() {
        assert!(matches!(part_bytes(SIMPLE, "999"), Err(Error::NoSuchPart(_))));
        assert!(matches!(part_bytes(SIMPLE, "not-a-number"), Err(Error::NoSuchPart(_))));
    }

    proptest::proptest! {
        /// `mail-parser` promises never to panic on malformed input; this is
        /// the belt to its braces, and a test that would fail loudly if a
        /// future version of the crate, or a future change to how we call
        /// it, ever broke that promise. `catch_unwind` is the point of this
        /// test -- everywhere else in this crate a panic should fail the
        /// test the ordinary way.
        #[test]
        fn parsing_random_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let result = std::panic::catch_unwind(|| parse(&bytes));
            prop_assert!(result.is_ok());
        }

        /// The other way a real socket hands us bytes: a message cut off
        /// mid-header or mid-body by a dropped connection or a batch that
        /// stopped short. Same guarantee, same test shape.
        #[test]
        fn parsing_a_truncated_real_message_never_panics(cut in 0usize..SIMPLE.len()) {
            let result = std::panic::catch_unwind(|| parse(&SIMPLE[..cut]));
            prop_assert!(result.is_ok());
        }
    }
}
