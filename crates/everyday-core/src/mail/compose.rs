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

    #[test]
    fn forward_subject_adds_the_prefix_once() {
        assert_eq!(forward_subject("dinner"), "Fwd: dinner");
        assert_eq!(forward_subject("Fwd: dinner"), "Fwd: dinner");
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
