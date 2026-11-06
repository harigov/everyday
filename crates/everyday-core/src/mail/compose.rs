//! The pure text a reply's or a forward's subject and quoted body need --
//! nothing about SMTP, MIME or CSS, which is what keeps this in the core
//! beside the records rather than in `everyday-mail`.
//!
//! `everyday_service::domains::mail::new_draft` builds the very same shapes
//! for the person's own compose box, on top of `everyday_mail::compose`,
//! which cannot live here: it depends on `mail-builder` and `css-inline` to
//! turn a finished [`crate::mail::Draft`] into the bytes SMTP actually sends,
//! and this crate stays offline and dependency-light on principle. What
//! `agent::tools::mail`'s `draft_reply` needs is only ever "Re: " and a
//! quoting `<blockquote>" -- four lines of logic that do not justify pulling
//! a MIME writer into the core, so they are copied here in miniature rather
//! than shared. Both copies are pinned by tests; a drift between them is a
//! cosmetic difference in a quote header, not a correctness bug, because
//! neither one is the one true rendering -- the sanitised HTML they quote
//! from already is.

use super::Address;
use jiff::Timestamp;

/// `"Re: "` in front of `subject`, once -- a second reply to the same thread
/// must not read "Re: Re: Re: dinner".
pub fn reply_subject(subject: &str) -> String {
    prefixed_subject(subject, "Re:")
}

/// As [`reply_subject`], for forwarding.
pub fn forward_subject(subject: &str) -> String {
    prefixed_subject(subject, "Fwd:")
}

/// Who a reply to `parent` goes to, as `(to, cc)`. `own` is every address of
/// the account replying, lower-cased.
///
/// A reply goes to `Reply-To` when the sender set one (RFC 5322 §3.6.2: a
/// mailing list, a ticketing system, a `no-reply@` naming a real mailbox),
/// otherwise to `From`. A reply to your own message carries on with whoever
/// it went to, as every mail client does: its `To` stay `To` and its `Cc`
/// stay `Cc`, where answering `From` would have addressed the reply to
/// yourself. Reply all adds the rest of the parent's recipients to `Cc`.
///
/// Your own addresses are left out wherever they were, each address appears
/// once, and only a message you sent to nobody but yourself is answered to
/// yourself.
pub fn reply_recipients(
    parent: &super::Message,
    own: &std::collections::HashSet<String>,
    all: bool,
) -> (Vec<Address>, Vec<Address>) {
    let mine = |a: &Address| own.contains(&a.email.to_lowercase());
    let from_me = mine(&parent.from);
    let (to, cc): (Vec<&Address>, Vec<&Address>) = if from_me {
        (parent.to.iter().collect(), if all { parent.cc.iter().collect() } else { Vec::new() })
    } else {
        let to = if parent.reply_to.is_empty() {
            vec![&parent.from]
        } else {
            parent.reply_to.iter().collect()
        };
        let cc = if all { parent.to.iter().chain(parent.cc.iter()).collect() } else { Vec::new() };
        (to, cc)
    };
    let mut seen = std::collections::HashSet::new();
    let mut keep = |list: Vec<&Address>| -> Vec<Address> {
        list.into_iter()
            .filter(|a| !mine(a) && seen.insert(a.email.to_lowercase()))
            .cloned()
            .collect()
    };
    let to = keep(to);
    let cc = keep(cc);
    match (to.is_empty(), cc.is_empty()) {
        (false, _) => (to, cc),
        (true, false) => (cc, Vec::new()),
        (true, true) => (vec![parent.from.clone()], Vec::new()),
    }
}

fn prefixed_subject(subject: &str, prefix: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        subject.to_string()
    } else {
        format!("{prefix} {subject}")
    }
}

/// `body_html` -- the assistant's own new text -- with the parent message's
/// sanitised body quoted underneath it, under an "On {date}, {name} wrote:"
/// line built from `from` and `date`.
///
/// `quoted_html` is expected to already be sanitised -- exactly what
/// [`crate::mail::Body::html_sanitised`] holds, sanitising having happened
/// once, at sync, per the plan's own rule. This function does not sanitise
/// anything itself; it only wraps.
pub fn quote_reply(body_html: &str, from: &Address, date: Timestamp, quoted_html: &str) -> String {
    let who = escape_html(if from.name.is_empty() { &from.email } else { &from.name });
    let when = jiff::fmt::strtime::format("%a, %-d %b %Y at %H:%M", date)
        .unwrap_or_else(|_| date.to_string());
    format!(
        r#"{body_html}<p>On {}, {who} wrote:</p><blockquote type="cite">{quoted_html}</blockquote>"#,
        escape_html(&when)
    )
}

/// What a draft's signature is wrapped in. The attribute is how the compose
/// sheet tells it from the person's words -- a rewrite must leave it be, and a
/// draft holding nothing else is still blank -- and how it stays one block in
/// the editor. `MailCompose` reads and writes the same mark (`signature.ts`,
/// `splitSignature`); a stranger's mail cannot bring one along, because
/// sanitising drops every `data-` attribute from what gets quoted.
const SIGNATURE_OPEN: &str = r#"<div data-signature="">"#;
const SIGNATURE_MARK: &str = "<div data-signature";

/// `signature_html` as a draft holds it, or nothing for a signature with no
/// words in it.
pub fn signature_block(signature_html: &str) -> String {
    let has_words = signature_html.split('<').enumerate().any(|(i, part)| {
        !(if i == 0 { part } else { part.split_once('>').map_or("", |(_, t)| t) }).trim().is_empty()
    });
    if has_words {
        format!("{SIGNATURE_OPEN}{}</div>", signature_html.trim())
    } else {
        String::new()
    }
}

/// A model's or a tool's own words with the signature under them.
pub fn with_signature(body_html: &str, signature_html: &str) -> String {
    format!("{body_html}{}", signature_block(signature_html))
}

/// A brand new draft's own part: an empty line to write on, then the
/// signature. Empty with no signature, which is what a new draft has always
/// started as.
pub fn blank_with_signature(signature_html: &str) -> String {
    let block = signature_block(signature_html);
    if block.is_empty() { block } else { format!("<p></p>{block}") }
}

/// `new_body` for a draft that held `old_body`: what the assistant's
/// `update_draft` writes, since it replaces the whole body and was never asked
/// to take the signature with it. Left as it is when it has one of its own, or
/// there was none to keep; otherwise the old block goes back above any quote.
pub fn carry_signature(old_body: &str, new_body: &str) -> String {
    let Some(block) = find_signature(old_body) else { return new_body.to_string() };
    if new_body.contains(SIGNATURE_MARK) {
        return new_body.to_string();
    }
    let at = quote_start(new_body).unwrap_or(new_body.len());
    format!("{}{block}{}", &new_body[..at], &new_body[at..])
}

/// Where the quoted parent starts in a draft body: an "On ... wrote:" paragraph
/// with a `<blockquote>` straight after it, the shape [`quote_reply`] writes and
/// `splitQuoted` looks for. A blockquote alone is not it, since somebody can
/// quote a line back in their own words.
fn quote_start(body: &str) -> Option<usize> {
    const END: &str = "wrote:</p>";
    let mut from = 0;
    while let Some(found) = body[from..].find(END) {
        let at = from + found;
        from = at + END.len();
        if !body[from..].trim_start().starts_with("<blockquote") {
            continue;
        }
        match body[..at].rfind("<p>On ") {
            Some(start) if !body[start..at].contains("</p>") => return Some(start),
            _ => {}
        }
    }
    None
}

/// The whole signature block in a draft body, if there is one above the quote.
fn find_signature(body: &str) -> Option<&str> {
    let start = body.find(SIGNATURE_MARK)?;
    if quote_start(body).is_some_and(|quote| quote < start) {
        return None;
    }
    let mut depth = 0usize;
    let mut at = start;
    while let Some(next) =
        body[at..].find("<div").into_iter().chain(body[at..].find("</div>")).min()
    {
        at += next;
        if body[at..].starts_with("</div>") {
            depth -= 1;
            at += "</div>".len();
            if depth == 0 {
                return Some(&body[start..at]);
            }
        } else {
            depth += 1;
            at += "<div".len();
        }
    }
    None
}

/// Escapes the five characters that matter inside an HTML text node or a
/// double-quoted attribute -- enough for the header line [`quote_reply`]
/// builds from a sender's own name, which is exactly the kind of string
/// that arrives with an `&` or a `<` in it uninvited.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_subject_adds_the_prefix_once() {
        assert_eq!(reply_subject("dinner"), "Re: dinner");
        assert_eq!(reply_subject("Re: dinner"), "Re: dinner");
        assert_eq!(reply_subject("re: dinner"), "re: dinner", "an existing prefix is left alone");
    }

    fn parent(from: &str, to: &[&str], cc: &[&str], reply_to: &[&str]) -> crate::mail::Message {
        let list = |l: &[&str]| l.iter().map(|a| Address::bare(*a)).collect::<Vec<_>>();
        crate::mail::Message {
            id: crate::id::MailMessageId::new(),
            account_id: crate::id::AccountId::new(),
            thread_id: crate::id::ThreadId::new(),
            message_id_header: "<a@example.com>".into(),
            date: Timestamp::now(),
            from: Address::bare(from),
            to: list(to),
            cc: list(cc),
            bcc: Vec::new(),
            reply_to: list(reply_to),
            subject: "hi".into(),
            snippet: String::new(),
            flags: crate::mail::MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: crate::mail::CategorySource::Rules,
            pack: crate::packstore::PackRef {
                account: "acc".into(),
                pack: crate::id::PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
        }
    }

    fn emails(list: &[Address]) -> Vec<&str> {
        list.iter().map(|a| a.email.as_str()).collect()
    }

    fn me() -> std::collections::HashSet<String> {
        ["me@example.com".to_string(), "alias@example.com".to_string()].into()
    }

    #[test]
    fn reply_all_never_addresses_yourself() {
        let p = parent(
            "ann@x.com",
            &["Me@Example.com", "bob@x.com"],
            &["alias@example.com", "cat@x.com"],
            &[],
        );
        let (to, cc) = reply_recipients(&p, &me(), true);
        assert_eq!(emails(&to), ["ann@x.com"]);
        assert_eq!(emails(&cc), ["bob@x.com", "cat@x.com"]);
        let (to, cc) = reply_recipients(&p, &me(), false);
        assert_eq!(emails(&to), ["ann@x.com"]);
        assert!(cc.is_empty());
    }

    #[test]
    fn replying_to_your_own_message_goes_to_whoever_it_went_to() {
        let p = parent(
            "me@example.com",
            &["ann@x.com", "bob@x.com"],
            &["cat@x.com", "me@example.com"],
            &[],
        );
        let (to, cc) = reply_recipients(&p, &me(), true);
        assert_eq!(emails(&to), ["ann@x.com", "bob@x.com"]);
        assert_eq!(emails(&cc), ["cat@x.com"]);
        let (to, cc) = reply_recipients(&p, &me(), false);
        assert_eq!(emails(&to), ["ann@x.com", "bob@x.com"]);
        assert!(cc.is_empty());
    }

    #[test]
    fn reply_to_wins_and_each_address_appears_once() {
        let p = parent("ann@x.com", &["list@x.com", "bob@x.com"], &["BOB@x.com"], &["list@x.com"]);
        let (to, cc) = reply_recipients(&p, &me(), true);
        assert_eq!(emails(&to), ["list@x.com"]);
        assert_eq!(emails(&cc), ["bob@x.com"]);
    }

    #[test]
    fn a_note_to_yourself_is_answered_to_yourself() {
        let p = parent("me@example.com", &["me@example.com"], &[], &[]);
        let (to, cc) = reply_recipients(&p, &me(), true);
        assert_eq!(emails(&to), ["me@example.com"]);
        assert!(cc.is_empty());
        // Only Cc left once you are taken out: they become who it is to.
        let p = parent("me@example.com", &["alias@example.com"], &["ann@x.com"], &[]);
        let (to, cc) = reply_recipients(&p, &me(), true);
        assert_eq!(emails(&to), ["ann@x.com"]);
        assert!(cc.is_empty());
    }

    #[test]
    fn forward_subject_adds_the_prefix_once() {
        assert_eq!(forward_subject("dinner"), "Fwd: dinner");
        assert_eq!(forward_subject("Fwd: dinner"), "Fwd: dinner");
    }

    #[test]
    fn a_signature_with_no_words_in_it_is_no_signature() {
        assert_eq!(signature_block(""), "");
        assert_eq!(signature_block("   "), "");
        assert_eq!(signature_block("<p></p><p> </p>"), "");
        assert_eq!(blank_with_signature("<p><br></p>"), "");
        assert_eq!(signature_block("<p>Hari</p>"), r#"<div data-signature=""><p>Hari</p></div>"#);
        assert_eq!(signature_block("Hari"), r#"<div data-signature="">Hari</div>"#);
    }

    #[test]
    fn a_new_draft_starts_with_a_line_to_write_on_above_the_signature() {
        assert_eq!(
            blank_with_signature("<p>Hari</p>"),
            r#"<p></p><div data-signature=""><p>Hari</p></div>"#
        );
        assert_eq!(
            with_signature("<p>Yes.</p>", "<p>Hari</p>"),
            r#"<p>Yes.</p><div data-signature=""><p>Hari</p></div>"#
        );
        assert_eq!(with_signature("<p>Yes.</p>", ""), "<p>Yes.</p>");
    }

    #[test]
    fn a_replaced_body_keeps_the_signature_above_any_quote() {
        let old = r#"<p>Old.</p><div data-signature=""><p>Hari</p></div><p>On Mon, Sam wrote:</p><blockquote type="cite"><p>Hi</p></blockquote>"#;
        assert_eq!(
            carry_signature(old, "<p>New.</p>"),
            r#"<p>New.</p><div data-signature=""><p>Hari</p></div>"#
        );
        assert_eq!(
            carry_signature(
                old,
                "<p>New.</p><p>On Mon, Sam wrote:</p><blockquote><p>Hi</p></blockquote>"
            ),
            r#"<p>New.</p><div data-signature=""><p>Hari</p></div><p>On Mon, Sam wrote:</p><blockquote><p>Hi</p></blockquote>"#
        );
    }

    #[test]
    fn a_replaced_body_is_left_alone_with_nothing_to_carry() {
        // Deleted by the person: not put back by the next rewrite.
        assert_eq!(carry_signature("<p>Old.</p>", "<p>New.</p>"), "<p>New.</p>");
        // Already carrying one.
        let new = r#"<p>New.</p><div data-signature=""><p>Other</p></div>"#;
        assert_eq!(carry_signature(r#"<div data-signature=""><p>Hari</p></div>"#, new), new);
        // Only a quote has one: somebody else's, not this draft's.
        let quoted = r#"<p>Old.</p><p>On Mon, Sam wrote:</p><blockquote><div data-signature=""><p>Sam</p></div></blockquote>"#;
        assert_eq!(carry_signature(quoted, "<p>New.</p>"), "<p>New.</p>");
    }

    #[test]
    fn a_line_quoted_back_above_the_signature_is_not_the_quote() {
        let old = r#"<blockquote>your point</blockquote><p>Agreed.</p><div data-signature=""><p>Hari</p></div>"#;
        assert_eq!(
            carry_signature(old, "<p>New.</p>"),
            r#"<p>New.</p><div data-signature=""><p>Hari</p></div>"#
        );
        // Under a blockquote with nobody's name on it, and still found again by
        // the next rewrite.
        let once = carry_signature(old, "<blockquote>your point</blockquote><p>New.</p>");
        assert_eq!(
            once,
            r#"<blockquote>your point</blockquote><p>New.</p><div data-signature=""><p>Hari</p></div>"#
        );
        assert_eq!(
            carry_signature(&once, "<p>Newer.</p>"),
            r#"<p>Newer.</p><div data-signature=""><p>Hari</p></div>"#
        );
    }

    #[test]
    fn a_signature_block_ends_at_its_own_closing_tag() {
        let old = r#"<div data-signature=""><div>Hari</div><div>CEO</div></div><p>after</p>"#;
        assert_eq!(
            carry_signature(old, "<p>New.</p>"),
            r#"<p>New.</p><div data-signature=""><div>Hari</div><div>CEO</div></div>"#
        );
    }

    #[test]
    fn quote_reply_names_the_sender_and_wraps_their_body() {
        let from = Address::new("Alice <script>", "alice@example.com");
        let date = "2026-09-14T09:00:00Z".parse::<Timestamp>().unwrap();
        let out = quote_reply("<p>Sounds good.</p>", &from, date, "<p>Original text</p>");
        assert!(out.starts_with("<p>Sounds good.</p>"));
        assert!(out.contains("wrote:"));
        assert!(out.contains("Original text"));
        // The sender's own name is escaped, not interpreted.
        assert!(!out.contains("<script>"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
    }

    #[test]
    fn quote_reply_falls_back_to_the_email_with_no_name() {
        let from = Address::bare("bob@example.com");
        let date = Timestamp::now();
        let out = quote_reply("", &from, date, "");
        assert!(out.contains("bob@example.com"));
    }
}
