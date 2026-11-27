//! Leaving a mailing list: what a message's `List-Unsubscribe` header offers,
//! and which of its ways out to take.
//!
//! RFC 2369 gives a list's mail a `List-Unsubscribe` header: one or more
//! links in angle brackets, comma separated, in the sender's order of
//! preference -- `<mailto:leave@list.example?subject=unsubscribe>,
//! <https://list.example/u/abc123>`. RFC 8058 adds `List-Unsubscribe-Post:
//! List-Unsubscribe=One-Click`, which says the https link takes a bare POST
//! and needs no page, no sign-in and no second click.
//!
//! Nothing here opens a socket or sends mail; `jatayu-service`'s
//! `mail::unsubscribe` does that. What lives here is the reading of the two
//! headers, kept on the message as a [`ListUnsubscribe`], and the one rule
//! about which way out comes first ([`ListUnsubscribe::methods`]):
//!
//! 1. **One click**, when the sender offered it and named an https link: a
//!    POST that nobody has to see.
//! 2. **An email** to the `mailto:` address, from the address the list writes
//!    to. Only its `subject` and `body` are taken from the link; any `to`,
//!    `cc` or `bcc` in it is ignored, so a stranger's header cannot add
//!    recipients to mail sent in this person's name.
//! 3. **A web page**, opened in the browser for the person to finish there.
//!    Never fetched in the background: a GET to an unsubscribe page can as
//!    easily confirm a subscription, or tell a tracker the message was read,
//!    and link scanners doing exactly that are why RFC 8058 exists.
//!
//! A message's sender is not checked here: RFC 8058 asks that both headers
//! be covered by a DKIM signature, and nothing in this application verifies
//! DKIM. What keeps a forged header from doing harm is that a POST only ever
//! goes to the open internet (the service's public-only client), only on a
//! person's own press, and carries nothing but `List-Unsubscribe=One-Click`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// The most links kept from one header. Real ones carry one or two.
const MAX_LINKS: usize = 8;

/// The longest link kept. Unsubscribe links carry a token per subscriber
/// and run long, but not this long; anything longer is not a link a sender
/// meant for this.
const MAX_LINK_CHARS: usize = 2048;

/// The longest subject taken from a `mailto:` link.
const MAX_SUBJECT_CHARS: usize = 200;

/// The longest body taken from a `mailto:` link.
const MAX_BODY_CHARS: usize = 2000;

/// What a message's `List-Unsubscribe` header offered, read once when the
/// message arrived, and when this person last used it.
///
/// Kept on [`super::Message::unsubscribe`]. One with no `links` there means
/// the message was looked at and offered no way out; `None` means it arrived
/// before headers were kept, and `jatayu-service` reads the message itself
/// to find out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ListUnsubscribe {
    /// The header's links in the sender's order, each unfolded and with its
    /// scheme lower-cased: `mailto:`, `https:` and `http:` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// Whether `List-Unsubscribe-Post: List-Unsubscribe=One-Click` came with
    /// it and an https link is there to POST to (RFC 8058).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub one_click: bool,
    /// When this person last asked to be taken off the list from this
    /// message -- a POST the sender accepted, or an email queued to them.
    /// Opening a web page does not count: what happened there is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<Timestamp>,
}

/// Which way out a sender offers first -- what the interface says before
/// asking ("asks them by email", "opens their page").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum UnsubscribeVia {
    /// An RFC 8058 POST.
    OneClick,
    /// An email to the list's `mailto:` address.
    Mail,
    /// A page to finish in the browser.
    Web,
}

/// One way out, ready to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// POST `List-Unsubscribe=One-Click` to this https address.
    OneClick { url: String },
    /// Send this email.
    Mail(Mailto),
    /// Open this page in the browser (https, or plain http).
    Web { url: String },
}

impl Method {
    pub fn via(&self) -> UnsubscribeVia {
        match self {
            Method::OneClick { .. } => UnsubscribeVia::OneClick,
            Method::Mail(_) => UnsubscribeVia::Mail,
            Method::Web { .. } => UnsubscribeVia::Web,
        }
    }

    /// Where it goes, for a person to read: the host a POST or a page is
    /// on, or the address an email goes to.
    pub fn destination(&self) -> String {
        match self {
            Method::OneClick { url } | Method::Web { url } => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default(),
            Method::Mail(mailto) => mailto.to.clone(),
        }
    }
}

/// An unsubscribe email, as a `mailto:` link describes it (RFC 6068).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailto {
    /// The one address it goes to: the first plausible one the link names.
    pub to: String,
    /// The link's `subject`, or "Unsubscribe" when it gave none.
    pub subject: String,
    /// The link's `body`, or "Unsubscribe" when it gave none.
    pub body: String,
}

impl ListUnsubscribe {
    /// Read a `List-Unsubscribe` header, and the `List-Unsubscribe-Post`
    /// header beside it if there was one. `None` when the header names no
    /// link this application could use.
    ///
    /// Both are taken as `mail-parser`'s `header_raw` hands them over: folded
    /// lines still folded, no encoded words decoded (a URI is ASCII). Text
    /// outside angle brackets -- comments, a bare URI -- is ignored, as RFC
    /// 2369 says it must be, and so is whitespace inside them.
    pub fn parse(header: &str, post: Option<&str>) -> Option<Self> {
        let links = parse_links(header);
        if links.is_empty() {
            return None;
        }
        let one_click =
            post.is_some_and(is_one_click) && links.iter().any(|l| web_link(l, true).is_some());
        Some(ListUnsubscribe { links, one_click, asked_at: None })
    }

    /// Every way out this header offers, best first: one click, then an
    /// email, then a page -- see the module docs. A one-click link is also
    /// offered again as a page last, for when the POST is refused.
    pub fn methods(&self) -> Vec<Method> {
        let mut methods = Vec::new();
        let first_https = self.links.iter().find_map(|l| web_link(l, true));
        if self.one_click
            && let Some(url) = &first_https
        {
            methods.push(Method::OneClick { url: url.clone() });
        }
        if let Some(mailto) = self.links.iter().find_map(|l| parse_mailto(l)) {
            methods.push(Method::Mail(mailto));
        }
        if let Some(url) =
            first_https.or_else(|| self.links.iter().find_map(|l| web_link(l, false)))
        {
            methods.push(Method::Web { url });
        }
        methods
    }

    /// The best way out, if there is one.
    pub fn method(&self) -> Option<Method> {
        self.methods().into_iter().next()
    }

    /// How [`Self::method`] would go, without building it.
    pub fn via(&self) -> Option<UnsubscribeVia> {
        self.method().map(|m| m.via())
    }
}

/// The bracketed links in `header`, unfolded, of a scheme kept, each once.
fn parse_links(header: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    let mut rest = header;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else { break };
        let link: String = after[..close].chars().filter(|c| !c.is_whitespace()).collect();
        rest = &after[close + 1..];
        let Some(link) = normalize_scheme(&link) else { continue };
        if link.chars().count() > MAX_LINK_CHARS || link.chars().any(char::is_control) {
            continue;
        }
        if !links.contains(&link) {
            links.push(link);
        }
        if links.len() == MAX_LINKS {
            break;
        }
    }
    links
}

/// `link` with its scheme lower-cased, if it is one of `mailto:`, `https:`
/// or `http:`.
fn normalize_scheme(link: &str) -> Option<String> {
    let (scheme, rest) = link.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    matches!(scheme.as_str(), "mailto" | "https" | "http").then(|| format!("{scheme}:{rest}"))
}

/// Whether a `List-Unsubscribe-Post` value is RFC 8058's
/// `List-Unsubscribe=One-Click`, compared without case or whitespace.
fn is_one_click(post: &str) -> bool {
    let value: String = post.chars().filter(|c| !c.is_whitespace()).collect();
    value.eq_ignore_ascii_case("List-Unsubscribe=One-Click")
}

/// `link` as a web address to POST to or open: https (or, when `https_only`
/// is false, plain http too), with a host, and with no user name or password
/// in it -- a link that signs in somewhere is not an unsubscribe link.
pub fn web_link(link: &str, https_only: bool) -> Option<String> {
    let url = url::Url::parse(link).ok()?;
    let scheme_ok = url.scheme() == "https" || (!https_only && url.scheme() == "http");
    if !scheme_ok || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.to_string())
}

/// A `mailto:` link as the email it describes, or `None` when it names no
/// plausible address. Only `subject` and `body` are read from it; see the
/// module docs. Percent-escapes are decoded and `+` is kept as a plus, as
/// RFC 6068 has it (unlike a web form, where it is a space).
pub fn parse_mailto(link: &str) -> Option<Mailto> {
    let (scheme, rest) = link.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("mailto") {
        return None;
    }
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let to = path
        .split(',')
        .map(percent_decode)
        .map(|a| a.trim().to_string())
        .find(|a| plausible_email(a))?;
    let mut subject = None;
    let mut body = None;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name.to_ascii_lowercase().as_str() {
            "subject" if subject.is_none() => {
                subject = Some(clean(&percent_decode(value), MAX_SUBJECT_CHARS, false))
            }
            "body" if body.is_none() => {
                body = Some(clean(&percent_decode(value), MAX_BODY_CHARS, true))
            }
            _ => {}
        }
    }
    let or_default = |v: Option<String>| {
        v.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Unsubscribe".to_string())
    };
    Some(Mailto { to, subject: or_default(subject), body: or_default(body) })
}

/// `value` cut to `max` characters, with control characters dropped -- line
/// breaks kept only where `lines` allows them (a body, not a subject).
fn clean(value: &str, max: usize, lines: bool) -> String {
    value
        .chars()
        .filter(|&c| !c.is_control() || (lines && c == '\n'))
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Percent-decode `s`, keeping `+` as itself; a `%` not followed by two hex
/// digits is kept as written, and bytes that are not UTF-8 come out as
/// replacement characters.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) =
                (bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b)))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether `address` could be an email address: one `@`, something either
/// side, a dot in the domain, and nothing that would end or split a header.
/// The SMTP layer checks it properly before anything is sent.
fn plausible_email(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && address.len() <= 254
        && !address
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\"(),;:\\[]".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_read_in_order_unfolded_and_only_from_brackets() {
        let header = "<mailto:leave@list.example?subject=unsubscribe>,\r\n <https://list.example/u/\r\n\tabc123> (the web way), http://bare.example/u";
        let parsed = ListUnsubscribe::parse(header, None).unwrap();
        assert_eq!(
            parsed.links,
            vec!["mailto:leave@list.example?subject=unsubscribe", "https://list.example/u/abc123"]
        );
        assert!(!parsed.one_click);
    }

    #[test]
    fn schemes_are_lower_cased_and_others_are_dropped() {
        let parsed = ListUnsubscribe::parse(
            "<MAILTO:a@b.example>, <ftp://x.example/u>, <HTTPS://x.example/u>",
            None,
        )
        .unwrap();
        assert_eq!(parsed.links, vec!["mailto:a@b.example", "https://x.example/u"]);
    }

    #[test]
    fn a_header_with_nothing_usable_is_none() {
        assert_eq!(ListUnsubscribe::parse("", None), None);
        assert_eq!(ListUnsubscribe::parse("https://no-brackets.example", None), None);
        assert_eq!(ListUnsubscribe::parse("<javascript:alert(1)>", None), None);
        assert_eq!(ListUnsubscribe::parse("<https://unclosed.example", None), None);
    }

    #[test]
    fn one_click_needs_the_post_header_and_an_https_link() {
        let post = Some(" List-Unsubscribe=One-Click\r\n");
        let both = ListUnsubscribe::parse("<mailto:a@b.example>, <https://x.example/u?t=1>", post)
            .unwrap();
        assert!(both.one_click);
        assert_eq!(both.methods()[0], Method::OneClick { url: "https://x.example/u?t=1".into() });
        assert_eq!(both.via(), Some(UnsubscribeVia::OneClick));

        let http_only = ListUnsubscribe::parse("<http://x.example/u>", post).unwrap();
        assert!(!http_only.one_click, "RFC 8058 is https only");

        let no_post = ListUnsubscribe::parse("<https://x.example/u>", None).unwrap();
        assert!(!no_post.one_click);
        assert_eq!(no_post.via(), Some(UnsubscribeVia::Web));

        let wrong_post =
            ListUnsubscribe::parse("<https://x.example/u>", Some("Something=Else")).unwrap();
        assert!(!wrong_post.one_click);
    }

    #[test]
    fn mail_comes_before_a_page_and_a_one_click_link_is_a_page_last() {
        let parsed = ListUnsubscribe::parse(
            "<https://x.example/u>, <mailto:leave@x.example>",
            Some("List-Unsubscribe=One-Click"),
        )
        .unwrap();
        let vias: Vec<_> = parsed.methods().iter().map(Method::via).collect();
        assert_eq!(vias, vec![UnsubscribeVia::OneClick, UnsubscribeVia::Mail, UnsubscribeVia::Web]);

        let mail_and_page =
            ListUnsubscribe::parse("<https://x.example/u>, <mailto:leave@x.example>", None)
                .unwrap();
        assert_eq!(mail_and_page.via(), Some(UnsubscribeVia::Mail));
    }

    #[test]
    fn mailto_takes_only_subject_and_body_and_keeps_plus() {
        let m = parse_mailto(
            "mailto:leave%2Bnews@list.example,other@x.example?Subject=Stop+it%21&body=remove%20me&cc=attacker@evil.example&to=more@evil.example",
        )
        .unwrap();
        assert_eq!(m.to, "leave+news@list.example");
        assert_eq!(m.subject, "Stop+it!");
        assert_eq!(m.body, "remove me");
    }

    #[test]
    fn mailto_defaults_and_refusals() {
        let m = parse_mailto("mailto:leave@list.example").unwrap();
        assert_eq!((m.subject.as_str(), m.body.as_str()), ("Unsubscribe", "Unsubscribe"));
        assert_eq!(parse_mailto("mailto:?subject=x"), None);
        assert_eq!(parse_mailto("mailto:not-an-address"), None);
        assert_eq!(
            parse_mailto("mailto:a@b.example%0D%0ABcc:x@y.example"),
            None,
            "a header break is no address"
        );
        assert_eq!(parse_mailto("https://x.example"), None);
        let subject = parse_mailto("mailto:a@b.example?subject=one%0D%0Atwo").unwrap().subject;
        assert_eq!(subject, "onetwo", "a subject is one line");
    }

    #[test]
    fn a_percent_without_two_hex_digits_is_kept_and_never_panics() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%Aé"), "%Aé");
        assert_eq!(percent_decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn web_links_refuse_credentials_and_odd_schemes() {
        assert_eq!(web_link("https://x.example/u", true).as_deref(), Some("https://x.example/u"));
        assert_eq!(web_link("http://x.example/u", true), None);
        assert_eq!(web_link("http://x.example/u", false).as_deref(), Some("http://x.example/u"));
        assert_eq!(web_link("https://user:pw@x.example/u", false), None);
        assert_eq!(web_link("mailto:a@b.example", false), None);
    }

    #[test]
    fn destinations_name_the_host_or_the_address() {
        let parsed = ListUnsubscribe::parse(
            "<mailto:leave@x.example>, <https://u.x.example/a?t=secret>",
            Some("List-Unsubscribe=One-Click"),
        )
        .unwrap();
        let dest: Vec<_> = parsed.methods().iter().map(Method::destination).collect();
        assert_eq!(dest, vec!["u.x.example", "leave@x.example", "u.x.example"]);
    }

    #[test]
    fn a_stored_header_round_trips_and_stays_small() {
        let parsed = ListUnsubscribe::parse("<https://x.example/u>", None).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, r#"{"links":["https://x.example/u"]}"#);
        assert_eq!(serde_json::from_str::<ListUnsubscribe>(&json).unwrap(), parsed);
        assert_eq!(
            serde_json::from_str::<ListUnsubscribe>("{}").unwrap(),
            ListUnsubscribe::default()
        );
    }

    #[test]
    fn many_links_are_capped() {
        let header: String = (0..20).map(|i| format!("<https://x.example/{i}>,")).collect();
        assert_eq!(ListUnsubscribe::parse(&header, None).unwrap().links.len(), MAX_LINKS);
    }
}
