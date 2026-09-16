//! The application's one HTTP client, and the two things every caller of it
//! needs to get right.
//!
//! Five features in Every Day send an ordinary web request through this
//! client: refreshing a subscribed calendar ([`crate::feeds`]); syncing a
//! calendar an account already has, over Google's or Microsoft's own API
//! rather than CalDAV ([`crate::accountcal::google`],
//! [`crate::accountcal::graph`]); looking up what a book is called
//! ([`crate::websearch`]); fetching a remote image a message asked to load
//! ([`crate::mailview::remote_image`]); and, once a call ends, sending a
//! chunk of speech to whichever remote transcriber the person configured
//! ([`crate::meeting::transcribe::openai`],
//! [`crate::meeting::transcribe::gemini`]). Sharing the client is not about
//! connection pooling; it is about the settings below being decided once. A
//! second `Client::builder()` elsewhere in the tree would be a second
//! timeout, a second redirect policy and a second chance to forget the size
//! cap, and the only sign of the mistake would be a wedged request some
//! months later.
//!
//! Two other things in this application open a socket on purpose rather than
//! coming through here. [`crate::meeting::models`] downloads a model archive
//! that can run to hundreds of megabytes over its own client, on its own
//! long timeout, because this one's thirty-second budget would abort a real
//! download before it finished -- see that module's doc for why its caution
//! belongs there and not here. [`crate::llm::client`] talks to whichever
//! model the assistant or the quick tier is configured against, over `rig`'s
//! own client rather than this one, for the streaming and tool-calling shape
//! chat needs; [`crate::accountcal::caldav`] likewise keeps its own
//! WebDAV-verb client. Mail's IMAP and SMTP connections are not HTTP at all
//! and never come near this module.
//!
//! What is *not* shared, among the five that do use this client, is the
//! prose, or the extra caution one caller needs that the others do not. A
//! 404 means "that subscription link has been revoked" to a calendar and
//! "nothing was found" to a lookup, so each caller maps status codes itself.
//! A remote image is the one address among them that a *stranger* chose
//! rather than the person using this application, so [`crate::mailview`]
//! layers its own SSRF check and a generic `User-Agent` on top of this
//! client's shared settings rather than trusting the far end the way a
//! calendar subscription, an account sync or a transcription request is. See
//! [`crate::feeds::fetch`], [`crate::websearch::get`] and
//! [`crate::mailview::remote_image`].
//!
//! # What this is allowed to talk to
//!
//! The address the user pasted into a calendar; the calendar API of an
//! account they signed into; the search endpoint behind a button they
//! pressed; an image address a message named -- fetched only once its
//! sender is trusted or the person asks, and only after
//! [`crate::mailview`]'s own checks -- and, if they chose a remote
//! transcriber over a local one, that transcriber's endpoint: OpenAI's,
//! Google's, or a base URL the person typed in for a compatible server of
//! their own. What travels there is speech only -- VAD cuts silence out
//! locally before anything is sent, per [`crate::meeting::transcribe`]'s own
//! doc -- and never a voiceprint, which this application does not transmit
//! to anybody. There is no telemetry, no update check, no crash reporter and
//! no analytics anywhere in this application. The webview's own network
//! permissions are unchanged and remain none at all — its content security
//! policy still allows `connect-src 'self' ipc:` — so nothing it renders can
//! cause a request of its own; every request this client makes is one a
//! person, not a webview, chose.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::error::{CommandError, CommandResult, codes};

/// Longest a single fetch may take, connection included.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many redirects to follow.
///
/// Publishers do redirect: `webcal://` links hand out short URLs, Outlook
/// moves feeds between regional hosts, and every image CDN in the world
/// bounces at least once.
const MAX_REDIRECTS: usize = 5;

/// The shared client.
///
/// Built once, on first use. Default features give rustls, HTTP/2 and the
/// platform's own proxy settings, which is the right set: a work calendar
/// behind a corporate proxy is the common case rather than the exotic one.
pub fn client() -> CommandResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                // Saying who we are is the polite thing to appear in
                // somebody's logs, and several publishers vary the document
                // by user agent more often than you would hope.
                .user_agent(concat!(
                    "EveryDay/",
                    env!("CARGO_PKG_VERSION"),
                    " (+https://github.com/everyday-app)"
                ))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| CommandError::new(codes::NETWORK, format!("could not start the fetcher: {e}")))
}

/// The client for addresses a stranger chose: a message's remote images.
///
/// A check before the request is not enough on its own, for two reasons this
/// client exists to close. A redirect is a second address the far end picks
/// after the check has passed, and the shared client follows up to
/// [`MAX_REDIRECTS`] of them anywhere. And a name resolved once for the check
/// is resolved again by the connection, so a server answering a public
/// address the first time and `127.0.0.1` the second (DNS rebinding) walks
/// straight past it. So here the filter is *inside* the fetch: every name is
/// resolved by [`PublicOnly`], which drops private answers before a socket
/// can use them, and every redirect hop whose host is a literal address is
/// refused by the redirect policy -- the one case a resolver never sees. No
/// proxy, because a proxy resolves names itself and would be a way around
/// both.
pub fn public_client() -> CommandResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .no_proxy()
                .dns_resolver(Arc::new(PublicOnly))
                .redirect(public_client_redirect_policy())
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| CommandError::new(codes::NETWORK, format!("could not start the fetcher: {e}")))
}

/// Follow at most [`MAX_REDIRECTS`] hops, only over http(s), and never to a
/// literal private address -- the hop a resolver never sees.
fn public_client_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let url = attempt.url();
        if url.scheme() != "http" && url.scheme() != "https" {
            return attempt.error("a redirect left http");
        }
        let literal = url
            .host_str()
            .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|h| h.parse::<IpAddr>().ok());
        if literal.is_some_and(|ip| is_forbidden_address(ip, false)) {
            return attempt.error("a redirect pointed at a private network");
        }
        attempt.follow()
    })
}

/// A resolver that answers only with addresses on the open internet.
///
/// A name whose every answer is private resolves to an error; a name with a
/// mix has its private answers removed, so the connection cannot fall back
/// to one of them.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let found: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| !is_forbidden_address(addr.ip(), false))
                .collect();
            if found.is_empty() {
                return Err("that address points at a private network, or at nothing".into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

/// Read a response body, refusing to grow past `max_bytes`.
///
/// Chunked rather than `.bytes()`, which would allocate whatever the server
/// decided to send. Both the advertised length and the actual arrival are
/// checked, because the first is a claim and the second is a fact.
///
/// `too_large` is called only when the cap is hit, so the caller's message
/// can name the thing that was too big.
pub async fn read_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
    too_large: impl Fn() -> String,
) -> CommandResult<Vec<u8>> {
    if response.content_length().is_some_and(|n| n as usize > max_bytes) {
        return Err(CommandError::new(codes::TOO_LARGE, too_large()));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport(&e))? {
        if body.len() + chunk.len() > max_bytes {
            return Err(CommandError::new(codes::TOO_LARGE, too_large()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A transport failure, with the URL taken out of it.
///
/// Callers with better words for their own situation should use those; this
/// is the floor, and its job is the redaction rather than the phrasing.
pub fn transport(e: &reqwest::Error) -> CommandError {
    CommandError::new(codes::NETWORK, format!("the request failed: {}", strip_url(&e.to_string())))
}

/// Remove anything that looks like a URL from an error message.
///
/// `reqwest` interpolates the request URL into most of its `Display` output,
/// and a calendar subscription URL is a bearer token — anyone holding one
/// can read that calendar until it is revoked. It must not end up in
/// `last_error`, which the interface shows and which is stored in the vault
/// beside (but separately from) the sealed copy of the URL itself.
///
/// It applies to search requests too, for a smaller reason that is still a
/// reason: a query is something the person typed, and their notes about what
/// they are reading do not belong in a log file.
pub fn strip_url(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                "(the address)"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Loopback, link-local, unspecified, broadcast, RFC 1918 private, and
/// carrier-grade NAT (RFC 6598) addresses -- everything a residential or
/// office network hands out to something that is not meant to be reached
/// from the open internet, plus the addresses that mean "this machine"
/// rather than any of those. Written against raw octets and segments rather
/// than the standard library's own `is_private`/`is_unique_local` family:
/// several of those stabilised well after this application's minimum
/// supported Rust version (`docs/`'s own note that local `rustc` lags CI),
/// and the ranges themselves are fixed by the RFCs regardless of which
/// release of the standard library happens to name them.
///
/// Also refuses the address ranges that exist specifically to disguise a
/// private or reserved address as something this filter would not
/// recognise on sight: `0.0.0.0/8` ("this network"), `192.0.0.0/24` and
/// `198.18.0.0/15` (both reserved by IANA for protocol testing and
/// benchmarking, never a real host), the `224.0.0.0/4` multicast range up
/// through `240.0.0.0/4` ("reserved for future use" -- neither is a unicast
/// address a single server could answer from, so nothing legitimate is ever
/// lost by refusing both), and three IPv6 shapes that carry an IPv4 address
/// inside an IPv6 one: the deprecated "IPv4-compatible" form (`::a.b.c.d`,
/// distinct from [`ipv4_mapped`]'s `::ffff:a.b.c.d` -- `::127.0.0.1` is
/// this, not that), NAT64's well-known prefix (`64:ff9b::/96`,
/// [RFC 6052](https://www.rfc-editor.org/rfc/rfc6052)), and 6to4
/// (`2002::/16`, [RFC 3056](https://www.rfc-editor.org/rfc/rfc3056)). Each of
/// the three is decoded to the `Ipv4Addr` it names and recursed into, the
/// same as `ipv4_mapped` already did, rather than blocked outright -- a
/// NAT64 gateway naming a public address must still be reachable, only the
/// address it actually names has to be checked, the same as any other one.
pub(crate) fn is_forbidden_address(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let loopback = o[0] == 127;
            if allow_loopback && loopback {
                return false;
            }
            loopback
                || o[0] == 0
                || o == [255, 255, 255, 255]
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (18..=19).contains(&o[1]))
                || o[0] >= 224
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let loopback = segments == [0, 0, 0, 0, 0, 0, 0, 1];
            if allow_loopback && loopback {
                return false;
            }
            if loopback || segments == [0; 8] {
                return true;
            }
            if let Some(mapped) = ipv4_mapped(&segments) {
                return is_forbidden_address(IpAddr::V4(mapped), allow_loopback);
            }
            if let Some(embedded) = ipv4_compatible(&segments) {
                return is_forbidden_address(IpAddr::V4(embedded), allow_loopback);
            }
            if let Some(embedded) = nat64_embedded(&segments) {
                return is_forbidden_address(IpAddr::V4(embedded), allow_loopback);
            }
            if let Some(embedded) = six_to_four_embedded(&segments) {
                return is_forbidden_address(IpAddr::V4(embedded), allow_loopback);
            }
            // `fe80::/10`, link-local, and `fc00::/7`, unique local -- the
            // IPv6 counterparts of `169.254.0.0/16` and the RFC 1918 ranges
            // above.
            let seg0 = segments[0];
            (seg0 & 0xffc0 == 0xfe80) || (seg0 & 0xfe00 == 0xfc00)
        }
    }
}

/// `::ffff:a.b.c.d` unpacked to the `Ipv4Addr` it maps, without
/// `Ipv6Addr::to_ipv4_mapped` -- see [`is_forbidden_address`]'s docs on
/// avoiding methods this crate's minimum Rust version might not have yet.
fn ipv4_mapped(segments: &[u16; 8]) -> Option<std::net::Ipv4Addr> {
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let a = (segments[6] >> 8) as u8;
        let b = (segments[6] & 0xff) as u8;
        let c = (segments[7] >> 8) as u8;
        let d = (segments[7] & 0xff) as u8;
        Some(std::net::Ipv4Addr::new(a, b, c, d))
    } else {
        None
    }
}

/// `::a.b.c.d`, the deprecated "IPv4-compatible IPv6 address": the top 96
/// bits are zero and the low 32 carry the address outright, with no `0xffff`
/// marker segment the way [`ipv4_mapped`]'s shape has one. `::1` (IPv6's own
/// loopback) and `::` (unspecified) both also have a zero top, but
/// [`is_forbidden_address`] checks and returns for both before this ever
/// runs, so this needs no special case for either.
fn ipv4_compatible(segments: &[u16; 8]) -> Option<std::net::Ipv4Addr> {
    if segments[0..6] == [0, 0, 0, 0, 0, 0] {
        let a = (segments[6] >> 8) as u8;
        let b = (segments[6] & 0xff) as u8;
        let c = (segments[7] >> 8) as u8;
        let d = (segments[7] & 0xff) as u8;
        Some(std::net::Ipv4Addr::new(a, b, c, d))
    } else {
        None
    }
}

/// `64:ff9b::/96`, NAT64's well-known prefix: the last 32 bits are the IPv4
/// address a translating gateway is standing in for. See
/// [`is_forbidden_address`]'s docs on why this is decoded and recursed into
/// rather than the whole prefix simply being refused.
fn nat64_embedded(segments: &[u16; 8]) -> Option<std::net::Ipv4Addr> {
    if segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        let a = (segments[6] >> 8) as u8;
        let b = (segments[6] & 0xff) as u8;
        let c = (segments[7] >> 8) as u8;
        let d = (segments[7] & 0xff) as u8;
        Some(std::net::Ipv4Addr::new(a, b, c, d))
    } else {
        None
    }
}

/// `2002::/16`, 6to4: the 32 bits right after the `2002` prefix are the IPv4
/// address being tunnelled. See [`is_forbidden_address`]'s docs on why this
/// is decoded and recursed into rather than the whole prefix simply being
/// refused.
fn six_to_four_embedded(segments: &[u16; 8]) -> Option<std::net::Ipv4Addr> {
    if segments[0] == 0x2002 {
        let a = (segments[1] >> 8) as u8;
        let b = (segments[1] & 0xff) as u8;
        let c = (segments[2] >> 8) as u8;
        let d = (segments[2] & 0xff) as u8;
        Some(std::net::Ipv4Addr::new(a, b, c, d))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server on this machine, answering anything with a redirect to
    /// `location`, or with a one-byte body when `location` is empty.
    async fn local_server(location: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let reply = if location.is_empty() {
                    "HTTP/1.1 200 OK\r\ncontent-length: 1\r\n\r\nx".to_string()
                } else {
                    format!(
                        "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n"
                    )
                };
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_this_machine_is_never_connected_to() {
        let port = local_server("").await;
        let client = public_client().unwrap();
        let err = client.get(format!("http://localhost:{port}/")).send().await;
        assert!(err.is_err(), "localhost must not resolve through the public client");
    }

    #[tokio::test]
    async fn a_redirect_to_a_private_address_is_not_followed() {
        // The first hop is a literal loopback address the caller's own check
        // would have refused; here the point is the second hop, which only
        // the redirect policy sees.
        let port = local_server("http://10.0.0.1/secret").await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(public_client_redirect_policy())
            .build()
            .unwrap();
        let err = client.get(format!("http://127.0.0.1:{port}/")).send().await.unwrap_err();
        assert!(err.is_redirect(), "expected the redirect to be refused, got {err}");
    }

    #[test]
    fn an_error_message_never_carries_the_address() {
        // The bug this exists to prevent: a subscription token, which is a
        // bearer credential, ending up in `last_error` -- a field the
        // interface displays and the logs record.
        let leaky = "error sending request for url \
             (https://calendar.example.com/private/secrettoken/basic.ics): timed out";
        let cleaned = strip_url(leaky);
        assert!(!cleaned.contains("secrettoken"), "got {cleaned:?}");
        assert!(!cleaned.contains("calendar.example.com"), "got {cleaned:?}");
        assert!(cleaned.contains("timed out"), "the useful half must survive: {cleaned:?}");
    }

    #[test]
    fn a_search_query_is_redacted_the_same_way() {
        let leaky = "error sending request for url \
             (https://html.duckduckgo.com/html/?q=my%20divorce%20lawyer): timed out";
        assert!(!strip_url(leaky).contains("divorce"));
    }

    // ---- finding 9: SSRF filter gaps ---------------------------------------

    #[test]
    fn the_newly_added_ipv4_ranges_are_forbidden() {
        let forbidden = [
            "0.0.0.0",
            "0.1.2.3",
            "192.0.0.1",
            "198.18.0.1",
            "198.19.255.255",
            "224.0.0.1", // multicast
            "240.0.0.1", // reserved
            "255.255.255.255",
        ];
        for addr in forbidden {
            let ip: IpAddr = addr.parse().unwrap();
            assert!(is_forbidden_address(ip, false), "{addr} should be forbidden");
        }
        // 198.18.0.0/15's neighbours must not be caught by an off-by-one.
        assert!(!is_forbidden_address("198.17.255.255".parse().unwrap(), false));
        assert!(!is_forbidden_address("198.20.0.0".parse().unwrap(), false));
    }

    #[test]
    fn an_ipv4_compatible_ipv6_loopback_is_forbidden() {
        // `::127.0.0.1` -- distinct from `::ffff:127.0.0.1` (`ipv4_mapped`'s
        // own shape) in that it carries no `0xffff` marker segment at all.
        let ip: IpAddr = "::127.0.0.1".parse().unwrap();
        assert!(is_forbidden_address(ip, false));
    }

    #[test]
    fn an_ipv4_compatible_ipv6_public_address_is_not_forbidden() {
        // The embedded address is decoded and checked on its own merits,
        // not blocked just for being an IPv4-compatible IPv6 address.
        let ip: IpAddr = "::93.184.216.34".parse().unwrap();
        assert!(!is_forbidden_address(ip, false));
    }

    #[test]
    fn a_nat64_embedded_private_address_is_forbidden() {
        // `64:ff9b::7f00:1` -- NAT64's well-known prefix wrapping
        // `127.0.0.1`.
        let ip: IpAddr = "64:ff9b::7f00:1".parse().unwrap();
        assert!(is_forbidden_address(ip, false));
    }

    #[test]
    fn a_nat64_embedded_public_address_is_not_forbidden() {
        // `93.184.216.34` == `5db8:d822`.
        let ip: IpAddr = "64:ff9b::5db8:d822".parse().unwrap();
        assert!(!is_forbidden_address(ip, false));
    }

    #[test]
    fn a_6to4_embedded_private_address_is_forbidden() {
        // `2002:0a00:0001::` embeds `10.0.0.1`.
        let ip: IpAddr = "2002:a00:1::".parse().unwrap();
        assert!(is_forbidden_address(ip, false));
    }

    #[test]
    fn a_6to4_embedded_public_address_is_not_forbidden() {
        // `2002:5db8:d822::` embeds `93.184.216.34`.
        let ip: IpAddr = "2002:5db8:d822::".parse().unwrap();
        assert!(!is_forbidden_address(ip, false));
    }
}
