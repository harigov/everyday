//! Turning a compose-box's worth of fields into the bytes SMTP sends, and
//! the small amount of RFC 5322 arithmetic that a reply or a forward needs
//! before it gets there.
//!
//! # Why `mail-builder`, and what it is not asked to do
//!
//! [`mail-builder`](https://docs.rs/mail-builder) writes the wire form: it
//! knows RFC 2047 (non-ASCII names and subjects), RFC 2231 (long or
//! non-ASCII filenames), folding, boundaries and transfer encoding, so
//! nothing here re-implements any of that. What it does *not* know is this
//! crate's own choices about what a message should contain, which is what
//! [`build`] supplies: a `Message-ID` this crate mints rather than one
//! `mail-builder` would generate from the local hostname (see
//! [`build`]'s docs), a plain-text alternative derived from the HTML when
//! none was written by hand, and — the one rule with a test of its own,
//! [`build_omits_bcc_from_the_written_headers`] — a `Bcc` that reaches the
//! envelope and never the header block, because a header block is exactly
//! what every recipient's mail client shows back to them.
//!
//! # Why CSS is inlined here, and how remote loading is switched off
//!
//! The compose editor writes HTML with a `<style>` block (TipTap's own
//! classes); most mail clients delete `<style>` elements outright before
//! rendering, which is why every serious HTML-mail pipeline inlines CSS as
//! a `style="…"` attribute on each element instead. [`css_inline`] does
//! that rewrite. Its default [`css_inline::InlineOptions`] will fetch a
//! `<link rel="stylesheet">` over the network if the HTML names one — an
//! outbound HTTP request from a mail-composing library is not a thing this
//! crate signs up for on a person's behalf, quoted content included, so
//! [`inline_css`] always sets `load_remote_stylesheets: false` regardless
//! of which `css-inline` Cargo features happen to be enabled. (They are, in
//! fact, all disabled here too — `default-features = false` in
//! `Cargo.toml` — so the `reqwest` client behind the `http` feature is
//! never even compiled in; belt and braces.)

use std::fmt;

use css_inline::InlineOptions;
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MbAddress;
use rand::RngCore;

use crate::mime::ParsedMessage;
use crate::text::html_to_text;

/// Everything that can go wrong turning an [`Outgoing`] into bytes. Address
/// and header construction cannot fail — `mail-builder` accepts anything a
/// caller hands it and encodes around the awkward parts — so the only two
/// ways in here are CSS this crate could not parse well enough to inline,
/// and the write to the in-memory buffer failing, which
/// [`std::io::Write`]'s contract technically allows even though a `Vec<u8>`
/// never actually does it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not inline the message's CSS: {0}")]
    InlineCss(#[from] css_inline::InlineError),
    #[error("could not write the message: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A name and an address for a header this crate writes, as opposed to
/// [`crate::mime::Address`], which is what a header this crate *read* might
/// contain — there the address or the name can be missing because a sender
/// wrote a malformed message; here the email is always present because
/// nothing can be sent without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(email: impl Into<String>) -> Self {
        Self { name: None, email: email.into() }
    }

    pub fn named(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: Some(name.into()), email: email.into() }
    }
}

/// One attachment on its way out: either offered as a file (`inline_cid:
/// None`) or referenced from the HTML body by `cid:` (`Some`), in which
/// case [`build`] marks the MIME part `Content-Disposition: inline` instead
/// of `attachment` — see [`build`]'s docs on why the filename still travels
/// with it either way.
#[derive(Debug, Clone)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub inline_cid: Option<String>,
}

/// Everything a compose box, a reply or a forward needs to become one
/// outgoing message. Nothing here is optional in the sense of "the sync
/// engine fills it in later" — a [`Draft`](crate) becomes an `Outgoing` in
/// full before [`build`] ever sees it.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Never written to a header — see the module docs and
    /// [`build_omits_bcc_from_the_written_headers`]. Still present in
    /// [`Built::envelope_to`], because the point of Bcc is that the server
    /// delivers to it while nobody's `Bcc:` line shows it.
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    /// The compose editor's own HTML, before CSS inlining. [`build`] never
    /// mutates a caller's copy; it inlines a clone.
    pub html: String,
    /// A plain-text alternative written by hand (rare — almost nothing
    /// composes plain text directly today). `None` means [`build`]
    /// derives one from `html` with [`html_to_text`], the same safe
    /// conversion `text::model_text` is built on, so a `display:none`
    /// decoration in the compose HTML cannot leak into the alternative
    /// part either.
    pub text: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
    /// Set by [`reply_headers`] for a reply; left `None` for a fresh
    /// message or a forward, which RFC 5322 §3.6.4 does not thread.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// The domain half of the `Message-ID` [`build`] mints — see that
    /// function's docs for why this crate generates its own rather than
    /// letting `mail-builder` fall back to the machine's hostname.
    pub message_id_domain: String,
}

/// What [`build`] hands back: the bytes to send, the id it minted for them,
/// and the envelope — which is not the same list as the headers, precisely
/// because of Bcc.
pub struct Built {
    pub raw: Vec<u8>,
    pub message_id: String,
    pub envelope_from: String,
    pub envelope_to: Vec<String>,
}

impl fmt::Debug for Built {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Built")
            .field("raw", &format!("<{} bytes>", self.raw.len()))
            .field("message_id", &self.message_id)
            .field("envelope_from", &self.envelope_from)
            .field("envelope_to", &self.envelope_to)
            .finish()
    }
}

/// Builds an RFC 5322 message from `outgoing`.
///
/// # The `Message-ID`
///
/// `mail-builder` will generate one for free if none is set, from the
/// local machine's hostname — fine for a script running on a server that
/// owns that hostname, wrong for this crate, which runs on a person's
/// laptop and is sending on behalf of `outgoing.from`'s domain. So `build`
/// always sets its own: sixteen random bytes (RFC 5322 §3.6.4 asks only for
/// unique-enough, not cryptographically unguessable, but sixteen bytes of
/// [`rand`]'s default CSPRNG costs nothing extra and closes the door on
/// two accounts ever minting the same id) rendered as hex, `@` the domain
/// this message claims to be from, so a receiving server's eye test of "does
/// the Message-ID's domain match the From's" passes.
///
/// # The Date header
///
/// Set to now, read from the system clock at the moment `build` runs —
/// not from anything on `outgoing`, because a draft can sit for minutes
/// between the person clicking send and the outbox actually dialling out,
/// and the header should say when the message was sent, not when it was
/// composed.
///
/// # Bcc
///
/// Never becomes a header. `outgoing.bcc` only ever appears in
/// [`Built::envelope_to`] — see [`build_omits_bcc_from_the_written_headers`].
pub fn build(outgoing: &Outgoing) -> Result<Built> {
    let message_id = generate_message_id(&outgoing.message_id_domain);
    let html = inline_css(&outgoing.html)?;
    let text = outgoing.text.clone().unwrap_or_else(|| html_to_text(&html));

    let mut builder = MessageBuilder::new()
        .message_id(message_id.clone())
        .from(mb_address(&outgoing.from))
        .subject(outgoing.subject.clone())
        .date(mail_builder::headers::date::Date::from(jiff::Timestamp::now().as_second()))
        .html_body(html)
        .text_body(text);

    if !outgoing.to.is_empty() {
        builder = builder.to(mb_address_list(&outgoing.to));
    }
    if !outgoing.cc.is_empty() {
        builder = builder.cc(mb_address_list(&outgoing.cc));
    }
    if !outgoing.reply_to.is_empty() {
        builder = builder.reply_to(mb_address_list(&outgoing.reply_to));
    }
    if let Some(in_reply_to) = &outgoing.in_reply_to {
        builder = builder.in_reply_to(in_reply_to.clone());
    }
    if !outgoing.references.is_empty() {
        builder = builder.references(outgoing.references.clone());
    }

    for attachment in &outgoing.attachments {
        builder = match &attachment.inline_cid {
            Some(cid) => builder.inline(
                attachment.content_type.clone(),
                cid.clone(),
                attachment.bytes.clone(),
            ),
            None => builder.attachment(
                attachment.content_type.clone(),
                attachment.filename.clone(),
                attachment.bytes.clone(),
            ),
        };
    }

    let raw = builder.write_to_vec()?;

    let mut envelope_to: Vec<String> =
        Vec::with_capacity(outgoing.to.len() + outgoing.cc.len() + outgoing.bcc.len());
    envelope_to.extend(outgoing.to.iter().map(|a| a.email.clone()));
    envelope_to.extend(outgoing.cc.iter().map(|a| a.email.clone()));
    envelope_to.extend(outgoing.bcc.iter().map(|a| a.email.clone()));

    Ok(Built { raw, message_id, envelope_from: outgoing.from.email.clone(), envelope_to })
}

/// Runs `html` through `css-inline`, turning `<style>` rules into `style="…"`
/// attributes so a client that deletes `<style>` blocks — most of them —
/// still shows the compose editor's formatting. See the module docs for why
/// `load_remote_stylesheets` is always `false` here, independent of which
/// `css-inline` Cargo features this crate has enabled.
pub fn inline_css(html: &str) -> Result<String> {
    let options = InlineOptions { load_remote_stylesheets: false, ..InlineOptions::default() };
    css_inline::CSSInliner::new(options).inline(html).map_err(Error::from)
}

/// `In-Reply-To` and `References` for a reply to `parent`, per RFC 5322
/// §3.6.4: `In-Reply-To` names the parent's own `Message-ID`; `References`
/// is the parent's own `References` (or, failing that, its `In-Reply-To`,
/// for the rare parent that only had one) with the parent's `Message-ID`
/// appended, so the chain grows by one link per reply instead of losing the
/// earlier ones. Either can come back empty: a parent with no `Message-ID`
/// (a malformed or very old message) cannot be threaded by id at all, and
/// [`crate::threading`] is what falls back to subject and timing for those.
pub fn reply_headers(parent: &ParsedMessage) -> (Option<String>, Vec<String>) {
    let in_reply_to = parent.message_id.clone();

    let mut references = if !parent.references.is_empty() {
        parent.references.clone()
    } else if let Some(irt) = &parent.in_reply_to {
        vec![irt.clone()]
    } else {
        Vec::new()
    };

    if let Some(id) = &parent.message_id {
        if !references.contains(id) {
            references.push(id.clone());
        }
    }

    (in_reply_to, references)
}

/// `subject` with one `Re: ` in front, unless it already has one — so
/// replying to "Re: Budget" gives "Re: Budget", not "Re: Re: Budget", no
/// matter how many times around the thread it goes.
pub fn reply_subject(subject: &str) -> String {
    prefixed_subject(subject, "Re:")
}

/// As [`reply_subject`], for forwarding: `Fwd: ` in front, once.
pub fn forward_subject(subject: &str) -> String {
    prefixed_subject(subject, "Fwd:")
}

fn prefixed_subject(subject: &str, prefix: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        subject.to_string()
    } else {
        format!("{prefix} {subject}")
    }
}

/// Wraps `sanitised_html` — the parent message's own sanitised body, exactly
/// as [`crate::sanitize`] left it at sync time; this function does not
/// sanitise anything itself, per the plan's "sanitising happens once, at
/// sync" rule — in a `<blockquote type="cite">` under an "On {date}, {name}
/// wrote:" line built from `parent`'s own headers. `type="cite"` is the
/// attribute mail clients (and browsers' default stylesheets) key on to
/// draw the quoting bar; nothing here relies on it being styled, since the
/// compose CSS can do that.
pub fn quote_html(parent: &ParsedMessage, sanitised_html: &str) -> String {
    let who = escape_html(&display_name(parent.from.first()));
    let header = match parent.date {
        Some(date) => format!("On {}, {who} wrote:", escape_html(&format_quote_date(date))),
        None => format!("On {who} wrote:"),
    };
    format!(r#"<p>{header}</p><blockquote type="cite">{sanitised_html}</blockquote>"#)
}

fn display_name(from: Option<&crate::mime::Address>) -> String {
    match from {
        Some(addr) => addr
            .name
            .clone()
            .or_else(|| addr.email.clone())
            .unwrap_or_else(|| "someone".to_string()),
        None => "someone".to_string(),
    }
}

/// A plain, UTC rendering of `date` for the quote header — "Mon, 1 Jan 2024
/// at 09:30". Deliberately not converted to any particular reader's time
/// zone: this text is generated once at compose time and baked into the
/// message body, so there is no single zone that would stay correct for
/// every later reader of the thread.
fn format_quote_date(date: jiff::Timestamp) -> String {
    jiff::fmt::strtime::format("%a, %-d %b %Y at %H:%M", date).unwrap_or_else(|_| date.to_string())
}

/// Escapes the five characters that matter inside an HTML text node or a
/// double-quoted attribute — enough for the header line [`quote_html`]
/// builds from a sender's name, which is untrusted text.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Sixteen random bytes, hex-encoded, `@` `domain` — see [`build`]'s docs.
fn generate_message_id(domain: &str) -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("{hex}@{domain}")
}

fn mb_address(addr: &Address) -> MbAddress<'static> {
    MbAddress::new_address(addr.name.clone(), addr.email.clone())
}

fn mb_address_list(addrs: &[Address]) -> MbAddress<'static> {
    MbAddress::new_list(addrs.iter().map(mb_address).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime;

    fn simple_outgoing() -> Outgoing {
        Outgoing {
            from: Address::named("Alice", "alice@example.com"),
            to: vec![Address::named("Bob", "bob@example.com")],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Hello".to_string(),
            html: "<p>Hi Bob.</p>".to_string(),
            text: None,
            attachments: Vec::new(),
            in_reply_to: None,
            references: Vec::new(),
            message_id_domain: "example.com".to_string(),
        }
    }

    #[test]
    fn message_id_is_minted_from_the_senders_domain_not_the_hostname() {
        let built = build(&simple_outgoing()).unwrap();
        assert!(built.message_id.ends_with("@example.com"), "{}", built.message_id);
        // Two builds of the same Outgoing must not collide.
        let built2 = build(&simple_outgoing()).unwrap();
        assert_ne!(built.message_id, built2.message_id);
    }

    #[test]
    fn a_plain_text_alternative_is_derived_from_html_when_none_is_given() {
        let built = build(&simple_outgoing()).unwrap();
        let parsed = mime::parse(&built.raw).unwrap();
        assert!(parsed.text.unwrap().contains("Hi Bob."));
    }

    #[test]
    fn a_hand_written_text_body_is_kept_as_is() {
        let mut outgoing = simple_outgoing();
        outgoing.text = Some("Plain text, written by hand.".to_string());
        let built = build(&outgoing).unwrap();
        let parsed = mime::parse(&built.raw).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("Plain text, written by hand."));
    }

    #[test]
    fn build_omits_bcc_from_the_written_headers() {
        let mut outgoing = simple_outgoing();
        outgoing.cc = vec![Address::new("cc@example.com")];
        outgoing.bcc = vec![Address::new("carol@example.com")];
        let built = build(&outgoing).unwrap();

        // Not in the envelope's forward path -- er, not in the wire bytes at all.
        let raw_text = String::from_utf8_lossy(&built.raw);
        assert!(!raw_text.contains("carol@example.com"), "{raw_text}");
        assert!(!raw_text.to_ascii_lowercase().contains("bcc:"), "{raw_text}");

        // But still in the envelope, so the outbox's SMTP session RCPT TOs it.
        assert!(built.envelope_to.contains(&"carol@example.com".to_string()));
        assert_eq!(
            built.envelope_to,
            vec![
                "bob@example.com".to_string(),
                "cc@example.com".to_string(),
                "carol@example.com".to_string(),
            ]
        );
    }

    #[test]
    fn attachments_round_trip_including_the_inline_ones() {
        let mut outgoing = simple_outgoing();
        outgoing.html = r#"<p>See <img src="cid:logo"></p>"#.to_string();
        outgoing.attachments = vec![
            OutgoingAttachment {
                filename: "logo.png".to_string(),
                content_type: "image/png".to_string(),
                bytes: b"pretend-png-bytes".to_vec(),
                inline_cid: Some("logo".to_string()),
            },
            OutgoingAttachment {
                filename: "invoice.pdf".to_string(),
                content_type: "application/pdf".to_string(),
                bytes: b"pretend-pdf-bytes".to_vec(),
                inline_cid: None,
            },
        ];
        let built = build(&outgoing).unwrap();
        let parsed = mime::parse(&built.raw).unwrap();

        let inline = parsed
            .parts
            .iter()
            .find(|p| p.content_id.as_deref() == Some("logo"))
            .expect("the inline image round-trips by content id");
        assert_eq!(inline.disposition, mime::Disposition::Inline);
        assert_eq!(mime::part_bytes(&built.raw, &inline.part_id).unwrap(), b"pretend-png-bytes");

        let attachment = parsed
            .parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("invoice.pdf"))
            .expect("the ordinary attachment round-trips by filename");
        assert_eq!(attachment.disposition, mime::Disposition::Attachment);
        assert_eq!(
            mime::part_bytes(&built.raw, &attachment.part_id).unwrap(),
            b"pretend-pdf-bytes"
        );
    }

    #[test]
    fn non_ascii_subject_and_names_round_trip_through_rfc_2047() {
        let mut outgoing = simple_outgoing();
        outgoing.from = Address::named("Zoé Müller", "zoe@example.com");
        outgoing.to = vec![Address::named("北京", "beijing@example.com")];
        outgoing.subject = "Café ☕ meeting".to_string();
        let built = build(&outgoing).unwrap();

        // The raw bytes are the RFC 2047 encoded form, not raw UTF-8, in the
        // headers -- that is the whole point of the standard this asserts
        // against.
        let raw_text = String::from_utf8_lossy(&built.raw);
        assert!(!raw_text.contains("Café"), "the header must be encoded, not literal UTF-8");

        let parsed = mime::parse(&built.raw).unwrap();
        assert_eq!(parsed.subject.as_deref(), Some("Café ☕ meeting"));
        assert_eq!(parsed.from[0].name.as_deref(), Some("Zoé Müller"));
        assert_eq!(parsed.to[0].name.as_deref(), Some("北京"));
    }

    #[test]
    fn css_inline_moves_style_rules_onto_the_elements_they_target() {
        let mut outgoing = simple_outgoing();
        outgoing.html =
            "<html><head><style>p { color: red; }</style></head><body><p>Hi</p></body></html>"
                .to_string();
        let built = build(&outgoing).unwrap();
        let parsed = mime::parse(&built.raw).unwrap();
        let html = parsed.html.unwrap();
        assert!(html.contains("style=\"color: red;\"") || html.contains("style=\"color:red;\""));
    }

    #[test]
    fn inline_css_never_touches_the_network_for_a_remote_stylesheet() {
        // A URL that cannot possibly resolve in a test sandbox; if this
        // function ever tried to fetch it, the test would hang or error
        // instead of returning immediately.
        let html = r#"<html><head><link rel="stylesheet" href="https://192.0.2.1/nonexistent.css"></head><body><p>Hi</p></body></html>"#;
        let inlined = inline_css(html)
            .expect("inlining does not fail just because a stylesheet was not fetched");
        assert!(inlined.contains("<p>Hi</p>"));
    }

    #[test]
    fn reply_subject_does_not_stack() {
        assert_eq!(reply_subject("Budget"), "Re: Budget");
        assert_eq!(reply_subject("Re: Budget"), "Re: Budget");
        assert_eq!(reply_subject("re: Budget"), "re: Budget");
    }

    #[test]
    fn forward_subject_does_not_stack() {
        assert_eq!(forward_subject("Budget"), "Fwd: Budget");
        assert_eq!(forward_subject("Fwd: Budget"), "Fwd: Budget");
    }

    fn parent_with_id(message_id: &str, references: Vec<String>) -> ParsedMessage {
        ParsedMessage {
            message_id: Some(message_id.to_string()),
            references,
            date: jiff::Timestamp::from_second(1_700_000_000).ok(),
            from: vec![mime::Address {
                name: Some("Alice".to_string()),
                email: Some("alice@example.com".to_string()),
            }],
            ..ParsedMessage::default()
        }
    }

    #[test]
    fn reply_headers_names_the_parent_and_extends_its_references() {
        let parent = parent_with_id("child@example.com", vec!["root@example.com".to_string()]);
        let (in_reply_to, references) = reply_headers(&parent);
        assert_eq!(in_reply_to.as_deref(), Some("child@example.com"));
        assert_eq!(
            references,
            vec!["root@example.com".to_string(), "child@example.com".to_string()]
        );
    }

    #[test]
    fn reply_headers_falls_back_to_the_parents_in_reply_to_when_it_has_no_references() {
        let mut parent = parent_with_id("child@example.com", Vec::new());
        parent.in_reply_to = Some("root@example.com".to_string());
        let (_, references) = reply_headers(&parent);
        assert_eq!(
            references,
            vec!["root@example.com".to_string(), "child@example.com".to_string()]
        );
    }

    #[test]
    fn reply_headers_on_a_parent_with_no_message_id_cannot_thread_by_id() {
        let parent = ParsedMessage::default();
        let (in_reply_to, references) = reply_headers(&parent);
        assert_eq!(in_reply_to, None);
        assert!(references.is_empty());
    }

    #[test]
    fn quote_html_wraps_the_parent_under_an_on_wrote_line() {
        let parent = parent_with_id("child@example.com", Vec::new());
        let quoted = quote_html(&parent, "<p>Original text.</p>");
        assert!(quoted.contains("On "));
        assert!(quoted.contains("Alice wrote:"));
        assert!(quoted.contains(r#"<blockquote type="cite">"#));
        assert!(quoted.contains("<p>Original text.</p>"));
    }

    #[test]
    fn quote_html_escapes_an_untrusted_display_name() {
        let mut parent = parent_with_id("child@example.com", Vec::new());
        parent.from = vec![mime::Address {
            name: Some("<script>alert(1)</script>".to_string()),
            email: Some("evil@example.com".to_string()),
        }];
        let quoted = quote_html(&parent, "<p>Body.</p>");
        assert!(!quoted.contains("<script>"));
        assert!(quoted.contains("&lt;script&gt;"));
    }
}
