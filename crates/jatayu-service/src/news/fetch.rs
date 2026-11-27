//! The network, for the Feed: a feed with a conditional request, an address
//! somebody pasted, and a site's icon.
//!
//! Which client asks depends on who chose the address, as `super`'s doc
//! says, and on where it points. A feed the person added themselves goes
//! over [`crate::http::client`] wherever it points, as a calendar
//! subscription does: a self-hosted feed behind a public name that answers
//! with a private address (split DNS) is theirs to follow, and so is one
//! that is only reachable through their proxy. The rest of this paragraph is
//! about addresses nobody typed -- one the assistant followed, one a page
//! named. A feed on the person's own network -- a
//! private or tailnet address, a `.local` or `.ts.net` name, a bare name like
//! `nas` -- goes over [`crate::http::client`], like a calendar subscription:
//! a self-hosted reader bridge is a real thing to follow. Everything else,
//! which is to say every address on the internet, goes over
//! [`crate::http::public_client`], which refuses private addresses and
//! redirects to them. That is the whole defence against a feed being used to
//! reach into the network: a source is fetched again every few minutes for
//! as long as it exists, the assistant can add one on the word of whatever it
//! last read, and a redirect from a public site to `http://169.254.169.254/`
//! is the oldest trick there is. The assistant's tool refuses the first kind
//! of address outright (see `jatayu_core::chat::is_private_network_host`,
//! which both sides share), so only a person typing one into the interface
//! ends up on the plain client. A site's icon is named by a page, and the
//! page by a stranger, so it too goes over the public client.
//!
//! A page somebody pasted to find a feed is the one place the two meet: it
//! may be on their network, and then it names feeds on their network, but a
//! page on the internet that names `http://192.168.0.1/feed` is not asking
//! for a feed, it is asking to be fetched from inside. Feeds a page names
//! therefore get no more trust than the page had.
//!
//! Every failure is a sentence fit for the line under a source's name, with
//! the address stripped out: a private feed's address is a bearer
//! credential, and `last_error` is shown and stored in the clear. The
//! wording follows [`crate::feeds`], which does the same for a calendar.

use super::{FoundFeed, discover, parse};
use crate::http::{self, BROWSER_USER_AGENT};
use crate::webpage::{self, Kind};
use futures::future::join_all;
use jatayu_core::Vault;
use jatayu_core::id::BlobId;
use jatayu_core::news::{FetchState, schedule::Hints};
use jiff::Timestamp;
use reqwest::header::{
    ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ETAG, HeaderMap, HeaderName, HeaderValue,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER, USER_AGENT,
};
use reqwest::{StatusCode, Url};
use std::time::Duration;

/// Largest feed accepted. Half what a calendar may be
/// (`crate::feeds::MAX_FEED_BYTES` is 32 MB), and more than it sounds like a
/// feed would need: a site generator that publishes every post ever written,
/// in full, in one file -- danluu.com's is 6.6 MB -- is common enough that
/// five megabytes refused real feeds. The point is that there is a number.
pub const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

/// Largest JSON answer accepted. Algolia's thirty stories are a few tens of
/// kilobytes.
pub const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;

/// Largest icon kept. A favicon is a few kilobytes, an apple-touch-icon a
/// few tens; past half a megabyte it is not an icon.
const MAX_ICON_BYTES: usize = 512 * 1024;

/// Largest page read to find its icon. Only the `<head>` matters, but the
/// body arrives in chunks regardless; past this the page is skipped and
/// `/favicon.ico` tried.
const MAX_ICON_PAGE_BYTES: usize = 1024 * 1024;

/// How long [`site_icon`] may take in all. The icon is a nicety shown beside
/// a source being added; nobody should wait on a slow site for it.
const ICON_BUDGET: Duration = Duration::from_secs(20);

/// How long one of [`discover`]'s guesses at a feed's path may take.
const GUESS_BUDGET: Duration = Duration::from_secs(10);

/// How many icon addresses [`site_icon`] tries before giving up.
const ICON_TRIES: usize = 4;

/// What a feed reader asks for. Several publishers negotiate on this, and a
/// good many serve the right thing as `text/html` or `application/octet-stream`
/// regardless, so everything is accepted.
const FEED_ACCEPT: &str = "application/atom+xml, application/rss+xml, application/feed+json, \
     application/json;q=0.9, application/xml;q=0.8, text/xml;q=0.8, */*;q=0.5";

const JSON_ACCEPT: &str = "application/json, */*;q=0.5";

/// A page somebody pasted may be the feed or the site that advertises one.
const PAGE_ACCEPT: &str = "text/html, application/xhtml+xml, application/atom+xml, \
     application/rss+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.9, \
     */*;q=0.5";

const ICON_ACCEPT: &str = "image/png, image/*;q=0.9, */*;q=0.5";

/// What a conditional request for a feed came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// `304 Not Modified`.
    NotModified { hints: Hints },
    /// A body, with the validators to send next time.
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
        /// Where the request ended up after redirects.
        final_url: String,
        hints: Hints,
    },
}

/// Why a fetch failed: a sentence fit to show a person, with any address
/// stripped, and the scheduling hints the failure carried (`Retry-After`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError {
    pub message: String,
    pub hints: Hints,
}

fn fail(message: impl Into<String>) -> FetchError {
    FetchError { message: message.into(), hints: Hints::default() }
}

/// Who chose an address, which decides how far a request for it may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The person typed it: anywhere, over the shared client.
    Typed,
    /// A page on the person's own network named it, or it is a fixed
    /// address of this application's: their network or the internet.
    Inside,
    /// The assistant chose it, or a page on the internet named it: the
    /// internet only.
    Outside,
}

/// Fetch a feed, sending `state`'s validators, capped at 16 MB. `reach` is
/// [`Reach::Typed`] for a source the person added and [`Reach::Outside`] for
/// one the assistant did.
pub async fn feed(url: &str, state: &FetchState, reach: Reach) -> Result<Fetched, FetchError> {
    let address = web_address(url).map_err(fail)?;
    let mut request = get(address, reach)?.header(ACCEPT, FEED_ACCEPT);
    // What the last answer said identifies it, so the server can say "same
    // as before" without sending it. Both are sent when both are known: a
    // server that understands the first ignores the second.
    if let Some(etag) = validator(state.etag.as_deref()) {
        request = request.header(IF_NONE_MATCH, etag);
    }
    if let Some(since) = validator(state.last_modified.as_deref()) {
        request = request.header(IF_MODIFIED_SINCE, since);
    }
    let response = send(request).await?;

    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified { hints: cache_hints(response.headers()) });
    }
    let response = succeeded(response)?;
    let headers = response.headers().clone();
    let final_url = response.url().to_string();
    let bytes = read(response, MAX_FEED_BYTES).await?;
    Ok(Fetched::Body {
        bytes,
        etag: header_text(&headers, &ETAG),
        last_modified: header_text(&headers, &LAST_MODIFIED),
        final_url,
        hints: cache_hints(&headers),
    })
}

/// A JSON API answer (Hacker News), routed like [`feed`].
pub async fn json(url: &str) -> Result<Vec<u8>, FetchError> {
    let address = web_address(url).map_err(fail)?;
    let response = send(get(address, Reach::Inside)?.header(ACCEPT, JSON_ACCEPT)).await?;
    read(succeeded(response)?, MAX_JSON_BYTES).await
}

/// Every feed at or advertised by an address somebody pasted: the address
/// itself if it is a feed, else what its page advertises, else what the
/// fallback paths answer with. Empty when there is none.
///
/// Routed like the feed it is looking for: the person typed the address, and
/// a self-hosted reader bridge on their own network is a common thing to
/// paste. What the page then names, and the guesses at the end, are fetched
/// with no more trust than the page itself had -- see [`get`] -- so a page on
/// the internet cannot send this to an address on the inside.
pub async fn discover(url: &str) -> Result<Vec<FoundFeed>, String> {
    let address = web_address(url)?;
    let page = get_page(address, Reach::Typed).await.map_err(|e| e.message)?;
    // Where the page actually was, after redirects: a typed address that
    // redirected to somebody else's server does not make that server's
    // advertisements the person's own.
    let inside = on_own_network(&page.url);

    if let Ok(feed) = parse::parse_feed(&page.bytes, &page.url) {
        return Ok(vec![FoundFeed { url: page.url, title: feed.title }]);
    }
    if page.is_html() {
        let found = discover::feed_links(&String::from_utf8_lossy(&page.bytes), &page.url);
        if !found.is_empty() {
            return Ok(confirm_feeds(found, inside).await);
        }
    }
    Ok(guess_feeds(&page.url, inside).await)
}

/// How many advertised feeds are read to learn their names.
const ADVERTISED_READ: usize = 4;

/// What a page advertises, each named by the feed itself where it can be.
///
/// A `<link rel="alternate">`'s `title` is as often the format as the
/// publication -- "Atom", "RSS", "Feed" -- and a source called "Atom" in the
/// sidebar is no use to anybody. So the first few are read, together and
/// briefly, and named by their own `<title>`; one that does not parse is
/// dropped, since it would only fail on its first refresh. Should none of
/// them answer in time, the page's word for them is better than nothing, and
/// they are offered as advertised.
///
/// `inside` is whether the page was on the person's own network, and so
/// whether a feed it names may be. One that may not is dropped before
/// anything else: not merely left unread, since "offered as advertised" is
/// what happens to the ones that do not answer, and an address that was never
/// allowed to be fetched must not be offered to be saved and fetched later.
async fn confirm_feeds(mut found: Vec<FoundFeed>, inside: bool) -> Vec<FoundFeed> {
    found.retain(|feed| inside || !on_own_network(&feed.url));
    let asks = found.iter().take(ADVERTISED_READ).map(|feed| async move {
        let address = web_address(&feed.url).ok()?;
        let page = tokio::time::timeout(GUESS_BUDGET, get_page(address, reach_of(inside)))
            .await
            .ok()?
            .ok()?;
        let parsed = parse::parse_feed(&page.bytes, &page.url).ok()?;
        let title = if parsed.title.trim().is_empty() { feed.title.clone() } else { parsed.title };
        Some(FoundFeed { url: feed.url.clone(), title })
    });
    let answered: Vec<Option<FoundFeed>> = join_all(asks).await;
    if answered.iter().all(Option::is_none) {
        return found;
    }
    answered.into_iter().flatten().collect()
}

/// The usual paths for a feed on a site whose page advertises none, in the
/// order [`discover::fallback_paths`] lists them, keeping those that parse.
/// Asked together, because most of them will be a 404, and each given a short
/// time of its own.
async fn guess_feeds(page_url: &str, inside: bool) -> Vec<FoundFeed> {
    let addresses: Vec<Url> = discover::fallback_paths(page_url)
        .iter()
        .filter_map(|path| web_address(path).ok())
        .collect();
    let asks = addresses.into_iter().map(|address| async move {
        tokio::time::timeout(GUESS_BUDGET, get_page(address, reach_of(inside))).await.ok()?.ok()
    });
    let mut found: Vec<FoundFeed> = Vec::new();
    for page in join_all(asks).await.into_iter().flatten() {
        if let Ok(feed) = parse::parse_feed(&page.bytes, &page.url)
            && !found.iter().any(|f| f.url == page.url)
        {
            found.push(FoundFeed { url: page.url, title: feed.title });
        }
    }
    found
}

/// A site's icon, downloaded into the blob store: the page's
/// `<link rel="icon">`, else `/favicon.ico`. `None` when there is none or it
/// is not a picture. Over [`crate::http::public_client`].
///
/// Best-effort in every direction. A site that is down, slow, refuses to say,
/// names something that is not a picture, or names a private address all
/// come to the same thing -- a source with initials where its icon would be
/// -- and none of them is worth an error. The write to the blob store is a
/// few kilobytes, so it is made where it stands rather than on the blocking
/// pool: a `&Vault` cannot be sent there.
pub async fn site_icon(vault: &Vault, site_url: &str) -> Option<BlobId> {
    let site = public_address(site_url)?;
    let bytes = tokio::time::timeout(ICON_BUDGET, icon_bytes(site)).await.ok()??;
    match vault.put_blob(&bytes) {
        Ok(blob) => Some(blob),
        Err(e) => {
            tracing::warn!(error = %e, "could not store a site's icon");
            None
        }
    }
}

/// The first icon a site has that is a picture within the cap.
async fn icon_bytes(site: Url) -> Option<Vec<u8>> {
    let client = http::public_client().ok()?;

    let mut candidates = match icon_page(client, &site).await {
        Some((html, landed)) => icon_links(&html, &landed),
        None => Vec::new(),
    };
    if let Ok(favicon) = site.join("/favicon.ico") {
        candidates.push(favicon);
    }
    for candidate in candidates.into_iter().filter(|u| !is_private_literal(u)).take(ICON_TRIES) {
        if let Some(bytes) = icon_file(client, candidate).await {
            return Some(bytes);
        }
    }
    None
}

/// The site's front page as text and where it ended up, if it is a page we
/// can read in a small space.
async fn icon_page(client: &reqwest::Client, site: &Url) -> Option<(String, Url)> {
    let response = client
        .get(site.clone())
        .header(ACCEPT, "text/html, application/xhtml+xml;q=0.9, */*;q=0.5")
        .header(USER_AGENT, BROWSER_USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let landed = response.url().clone();
    let declared = webpage::classify(content_type(response.headers()).as_deref());
    let bytes = http::read_capped(response, MAX_ICON_PAGE_BYTES, String::new).await.ok()?;
    let kind = match declared {
        Kind::Unknown => webpage::sniff(&bytes),
        known => known,
    };
    matches!(kind, Kind::Html).then(|| (String::from_utf8_lossy(&bytes).into_owned(), landed))
}

/// One candidate icon, if it answers and is a picture.
async fn icon_file(client: &reqwest::Client, address: Url) -> Option<Vec<u8>> {
    let response = client
        .get(address)
        .header(ACCEPT, ICON_ACCEPT)
        .header(USER_AGENT, BROWSER_USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = http::read_capped(response, MAX_ICON_BYTES, String::new).await.ok()?;
    is_raster_picture(&bytes).then_some(bytes)
}

/// The icons a page names, best first: an `apple-touch-icon`, which is always
/// a bitmap and a good size, then each `rel="icon"` in document order.
/// A vector icon is left out -- an SVG can carry script, and the webview
/// would be handed it as a picture -- and so is anything that is not an
/// `http(s)` address once resolved against the page.
fn icon_links(html: &str, page: &Url) -> Vec<Url> {
    let document = dom_query::Document::from(html);
    // A `<base href>` moves what the page's relative addresses are relative
    // to, as it does for a browser.
    let base = document.base_uri().and_then(|href| page.join(&href).ok());
    let page = base.as_ref().unwrap_or(page);
    let mut touch = Vec::new();
    let mut plain = Vec::new();
    for link in document.select("link[rel][href]").nodes() {
        let (Some(rel), Some(href)) = (link.attr("rel"), link.attr("href")) else { continue };
        let rel = rel.to_ascii_lowercase();
        let is_touch = rel.split_ascii_whitespace().any(|t| t.starts_with("apple-touch-icon"));
        let is_icon = rel.split_ascii_whitespace().any(|t| t == "icon");
        if !is_touch && !is_icon {
            continue;
        }
        let href = href.trim();
        let declared_svg =
            link.attr("type").is_some_and(|t| t.to_ascii_lowercase().contains("svg"));
        let path = href.split(['?', '#']).next().unwrap_or(href).to_ascii_lowercase();
        if declared_svg || path.ends_with(".svg") {
            continue;
        }
        let Ok(address) = page.join(href) else { continue };
        if !matches!(address.scheme(), "http" | "https") {
            continue;
        }
        let list = if is_touch { &mut touch } else { &mut plain };
        if !list.contains(&address) {
            list.push(address);
        }
    }
    touch.extend(plain);
    touch
}

/// Is this a bitmap a webview will draw as a picture? Sniffed from the bytes
/// the way [`crate::websearch::fetch_image`] does, and for the same reason:
/// the `Content-Type` is the far end's claim. SVG is out for the reason
/// above. `.ico` is in: it is what `/favicon.ico` usually is, and
/// [`jatayu_core::media::sniff_mime`] has no signature for it.
fn is_raster_picture(bytes: &[u8]) -> bool {
    let is_ico = bytes.len() >= 6 && bytes[..4] == [0, 0, 1, 0] && (bytes[4] != 0 || bytes[5] != 0);
    is_ico
        || matches!(
            jatayu_core::media::sniff_mime(bytes),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        )
}

// ── Plumbing ─────────────────────────────────────────────────────────────

/// A page fetched for feed discovery: the bytes, where the request ended up,
/// and what the far end said it was.
struct Page {
    bytes: Vec<u8>,
    url: String,
    content_type: Option<String>,
}

impl Page {
    fn is_html(&self) -> bool {
        match webpage::classify(self.content_type.as_deref()) {
            Kind::Html => true,
            Kind::Unknown => matches!(webpage::sniff(&self.bytes), Kind::Html),
            _ => false,
        }
    }
}

async fn get_page(address: Url, reach: Reach) -> Result<Page, FetchError> {
    let response = send(get(address, reach)?.header(ACCEPT, PAGE_ACCEPT)).await?;
    let response = succeeded(response)?;
    let url = response.url().to_string();
    let content_type = content_type(response.headers());
    let bytes = read(response, MAX_FEED_BYTES).await?;
    Ok(Page { bytes, url, content_type })
}

/// Whether an address names a machine on the person's own network, by what
/// it says rather than what it resolves to. The one question that decides
/// the client; see the module doc.
fn on_own_network(address: &str) -> bool {
    Url::parse(address)
        .ok()
        .and_then(|url| url.host_str().map(jatayu_core::chat::is_private_network_host))
        .unwrap_or(false)
}

/// How far a link a page named may reach: as far as the page itself was.
fn reach_of(inside: bool) -> Reach {
    if inside { Reach::Inside } else { Reach::Outside }
}

/// A `GET` for `address` over the right client.
///
/// An address the person typed goes over the shared client, wherever it
/// points. Otherwise an address on the person's own network goes over the
/// shared client as long as `reach` says that is where the request came
/// from: a link a page named is `Inside` only if the page was. Anything
/// else goes over the public client,
/// which keeps private *names* out through its resolver; a private *literal*
/// address is never given to a resolver at all, so those are refused here --
/// including the odd ranges (multicast, `0.0.0.0/8`, benchmarking space) that
/// are not "the person's network" but are not the internet either.
fn get(address: Url, reach: Reach) -> Result<reqwest::RequestBuilder, FetchError> {
    if reach == Reach::Typed {
        let client = http::client().map_err(|e| fail(e.message))?;
        return Ok(client.get(address));
    }
    if address.host_str().is_some_and(jatayu_core::chat::is_private_network_host) {
        if reach == Reach::Outside {
            return Err(fail(
                "a page on the internet cannot point at an address on your own network.",
            ));
        }
        let client = http::client().map_err(|e| fail(e.message))?;
        return Ok(client.get(address));
    }
    if is_private_literal(&address) {
        return Err(fail("that address is not one a feed can be fetched from."));
    }
    let client = http::public_client().map_err(|e| fail(e.message))?;
    Ok(client.get(address).header(USER_AGENT, http::USER_AGENT))
}

async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, FetchError> {
    request.send().await.map_err(|e| fail(describe(&e)))
}

/// The response itself when the server said yes; otherwise the reason, and
/// the `Retry-After` a busy server sent with it.
fn succeeded(response: reqwest::Response) -> Result<reqwest::Response, FetchError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let mut hints = Hints::default();
    if matches!(status.as_u16(), 429 | 503) {
        hints.retry_after_secs = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| retry_after(v, Timestamp::now()));
    }
    Err(FetchError { message: explain_status(status), hints })
}

async fn read(response: reqwest::Response, cap: usize) -> Result<Vec<u8>, FetchError> {
    http::read_capped(response, cap, || {
        format!(
            "the site sent more than {} MB, which is more than this app will read",
            cap / 1_048_576
        )
    })
    .await
    .map_err(|e| fail(e.message))
}

/// Turn a status code into something worth reading.
fn explain_status(status: StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "the site refused the request. A private feed's address is secret and \
             can be revoked; it may need copying again."
            .to_string(),
        404 | 410 => "there is nothing at that address any more. The feed may have moved, \
             or been deleted."
            .to_string(),
        429 => "the site asked us to slow down. It will be tried again later.".to_string(),
        500..=599 => format!("the site is having trouble ({status})."),
        _ => format!("the site answered {status}."),
    }
}

/// Same job for a transport error.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the site did not answer in time.".to_string()
    } else if e.is_connect() {
        "could not reach the site. Check the address, and the network.".to_string()
    } else if e.is_redirect() {
        "the address redirected too many times.".to_string()
    } else {
        // Deliberately without the URL: an error string ends up in the
        // interface and in logs, and the URL may be a credential.
        format!("the feed could not be fetched: {}", http::strip_url(&e.to_string()))
    }
}

/// `address` as an `http(s)` URL, tolerating what people paste: no scheme at
/// all, and the `feed:` forms feed readers have used for twenty years. The
/// error is a sentence, and does not quote what was typed.
fn web_address(address: &str) -> Result<Url, String> {
    let typed = address.trim();
    let lower = typed.to_ascii_lowercase();
    let text = if lower.starts_with("feed://") {
        format!("https://{}", &typed["feed://".len()..])
    } else if lower.starts_with("feed:http") {
        typed["feed:".len()..].to_string()
    } else if !typed.contains("://") {
        format!("https://{typed}")
    } else {
        typed.to_string()
    };
    let url = Url::parse(&text).map_err(|_| "that is not a web address.".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only web addresses, starting http:// or https://, can be followed.".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("that web address has no site in it.".into());
    }
    Ok(url)
}

/// `address` as an `http(s)` URL that is not a literal private address -- the
/// one case [`crate::http::public_client`]'s resolver never sees.
fn public_address(address: &str) -> Option<Url> {
    let url = web_address(address).ok()?;
    (!is_private_literal(&url)).then_some(url)
}

fn is_private_literal(url: &Url) -> bool {
    http::names_forbidden_literal(url)
}

/// A stored validator as a header value, if it still makes one: it came from
/// a header, but a vault is a long-lived place.
fn validator(stored: Option<&str>) -> Option<HeaderValue> {
    stored.and_then(|v| HeaderValue::from_str(v).ok())
}

fn content_type(headers: &HeaderMap) -> Option<String> {
    headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn header_text(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// What a successful answer says about caching.
fn cache_hints(headers: &HeaderMap) -> Hints {
    Hints {
        max_age_secs: headers.get(CACHE_CONTROL).and_then(|v| v.to_str().ok()).and_then(max_age),
        ..Hints::default()
    }
}

/// `max-age` from a `Cache-Control` value, in seconds.
fn max_age(value: &str) -> Option<u32> {
    value.split(',').find_map(|directive| {
        let (name, seconds) = directive.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("max-age") {
            return None;
        }
        let seconds: u64 = seconds.trim().trim_matches('"').parse().ok()?;
        Some(u32::try_from(seconds).unwrap_or(u32::MAX))
    })
}

/// `Retry-After` in seconds from now: either a number of seconds, or the
/// HTTP date the server wants to be asked again at. A date already past is
/// zero.
fn retry_after(value: &str, now: Timestamp) -> Option<u32> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(u32::try_from(seconds).unwrap_or(u32::MAX));
    }
    // An HTTP date is an RFC 2822 one with `GMT` for the zone.
    let at = jiff::fmt::rfc2822::DateTimeParser::new().parse_timestamp(value).ok()?;
    let seconds = at.as_second().saturating_sub(now.as_second()).max(0);
    Some(u32::try_from(seconds).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_age_is_read_from_among_the_other_directives() {
        assert_eq!(max_age("max-age=600"), Some(600));
        assert_eq!(max_age("public, max-age=3600, must-revalidate"), Some(3600));
        assert_eq!(max_age("Max-Age = 90"), Some(90));
        assert_eq!(max_age(r#"max-age="120""#), Some(120));
        assert_eq!(max_age("max-age=0"), Some(0));
        assert_eq!(max_age("s-maxage=600"), None);
        assert_eq!(max_age("no-cache"), None);
        assert_eq!(max_age("max-age=soon"), None);
        assert_eq!(max_age("max-age=99999999999"), Some(u32::MAX));
    }

    #[test]
    fn retry_after_is_seconds_or_a_date() {
        let now: Timestamp = "2026-10-11T12:00:00Z".parse().unwrap();
        assert_eq!(retry_after("120", now), Some(120));
        assert_eq!(retry_after(" 7 ", now), Some(7));
        assert_eq!(retry_after("Sun, 11 Oct 2026 12:05:00 GMT", now), Some(300));
        // A date already gone is "now", not a negative wait.
        assert_eq!(retry_after("Sun, 11 Oct 2026 11:00:00 GMT", now), Some(0));
        assert_eq!(retry_after("later", now), None);
        assert_eq!(retry_after("", now), None);
    }

    #[test]
    fn an_address_is_tolerated_the_way_people_paste_it() {
        let ok = |s: &str| web_address(s).unwrap().to_string();
        assert_eq!(ok("https://example.com/feed.xml"), "https://example.com/feed.xml");
        assert_eq!(ok("  example.com/feed  "), "https://example.com/feed");
        assert_eq!(ok("feed://example.com/rss"), "https://example.com/rss");
        assert_eq!(ok("feed:https://example.com/rss"), "https://example.com/rss");
        assert_eq!(ok("FEED://example.com/rss"), "https://example.com/rss");
        assert_eq!(ok("http://localhost:8080/atom"), "http://localhost:8080/atom");
    }

    #[test]
    fn an_address_that_is_not_a_web_address_is_refused_without_being_quoted() {
        for typed in ["file:///etc/passwd", "ftp://example.com/feed", "https://", ""] {
            let message = web_address(typed).unwrap_err();
            assert!(!message.contains("passwd") && !message.contains("ftp:"), "{message}");
        }
    }

    #[test]
    fn a_literal_private_address_is_not_a_public_one() {
        assert!(public_address("http://127.0.0.1/").is_none());
        assert!(public_address("http://192.168.1.1/").is_none());
        assert!(public_address("http://[::1]/").is_none());
        assert!(public_address("https://example.com/").is_some());
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn an_address_on_the_persons_network_is_told_from_one_on_the_internet() {
        for own in [
            "http://127.0.0.1:8080/feed",
            "http://192.168.1.5/rss",
            "http://10.0.0.2/",
            "http://100.101.102.103/feed",
            "http://[fd7a:115c:a1e0::5]/feed",
            "https://bridge.tail1234.ts.net/feed",
            "http://nas.local/feed",
            "http://nas/feed",
            "http://localhost:3000/",
        ] {
            assert!(on_own_network(own), "{own}");
        }
        for stranger in [
            "https://example.com/feed",
            "http://8.8.8.8/",
            "https://100.63.0.1/",
            "https://example.com.local.evil.test/",
        ] {
            assert!(!on_own_network(stranger), "{stranger}");
        }
    }

    #[test]
    fn what_a_page_on_the_internet_names_is_not_fetched_from_inside() {
        // The assistant, or a stranger's page, naming a private address.
        for private in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:7398/",
            "http://192.168.0.1/feed",
            "http://router/feed",
            "http://box.local/feed",
        ] {
            assert!(get(url(private), Reach::Outside).is_err(), "{private} must be refused");
            // A page on the person's own network naming it, or the person
            // typing it themselves, is how a self-hosted feed is followed,
            // and still works.
            assert!(get(url(private), Reach::Inside).is_ok(), "{private} is the person's own");
            assert!(get(url(private), Reach::Typed).is_ok(), "{private} is the person's own");
        }
        // A public address is fine any way.
        for reach in [Reach::Outside, Reach::Inside, Reach::Typed] {
            assert!(get(url("https://example.com/feed"), reach).is_ok());
        }
    }

    #[test]
    fn an_odd_literal_address_is_refused_unless_the_person_typed_it() {
        // Multicast, "this network" and benchmarking space are not the
        // person's network, and the public client's resolver never sees a
        // literal, so nothing else would stop them from a page or the
        // assistant. Somebody typing one has said what they meant.
        for odd in [
            "http://224.0.0.1/",
            "http://0.1.2.3/",
            "http://198.18.0.1/",
            "http://[ff02::1]/",
            "http://[64:ff9b:1::1]/",
        ] {
            assert!(get(url(odd), Reach::Inside).is_err(), "{odd}");
            assert!(get(url(odd), Reach::Outside).is_err(), "{odd}");
        }
    }

    #[tokio::test]
    async fn advertised_feeds_on_a_private_address_are_dropped_from_a_public_page() {
        let found = vec![
            FoundFeed { url: "http://169.254.169.254/feed".into(), title: "meta".into() },
            FoundFeed { url: "http://nas/feed".into(), title: "nas".into() },
        ];
        // Nothing is fetched and nothing is offered: with no feed left to
        // read, the "offer them as advertised" fallback has nothing either.
        assert!(confirm_feeds(found, false).await.is_empty());
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let denied = explain_status(StatusCode::FORBIDDEN);
        assert!(denied.contains("secret"), "{denied}");
        let gone = explain_status(StatusCode::NOT_FOUND);
        assert!(gone.contains("moved"), "{gone}");
        let slow = explain_status(StatusCode::TOO_MANY_REQUESTS);
        assert!(slow.contains("slow down"), "{slow}");
        let broken = explain_status(StatusCode::BAD_GATEWAY);
        assert!(broken.contains("502"), "{broken}");
    }

    #[test]
    fn an_apple_touch_icon_comes_before_a_plain_icon_and_a_vector_one_is_left_out() {
        let page = Url::parse("https://blog.example.com/2026/post.html").unwrap();
        let html = r#"<html><head>
            <link rel="stylesheet" href="/s.css">
            <link rel="icon" href="/favicon-32.png" sizes="32x32">
            <link rel="shortcut icon" href="favicon.ico">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel="icon" href="/other.svg?v=2">
            <link rel="apple-touch-icon" href="//cdn.example.com/touch.png">
            <link rel="apple-touch-icon-precomposed" href="/touch2.png">
            <link rel="icon" href="javascript:alert(1)">
            <link rel="icon" href="/favicon-32.png">
            </head><body></body></html>"#;
        let links: Vec<String> = icon_links(html, &page).iter().map(Url::to_string).collect();
        assert_eq!(
            links,
            [
                "https://cdn.example.com/touch.png",
                "https://blog.example.com/touch2.png",
                "https://blog.example.com/favicon-32.png",
                "https://blog.example.com/2026/favicon.ico",
            ]
        );
    }

    #[test]
    fn a_page_naming_no_icon_names_none() {
        let page = Url::parse("https://example.com/").unwrap();
        assert!(icon_links("<html><head><title>x</title></head></html>", &page).is_empty());
        assert!(icon_links("", &page).is_empty());
    }

    #[test]
    fn only_bitmaps_are_pictures() {
        assert!(is_raster_picture(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(is_raster_picture(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(is_raster_picture(b"GIF89a\x01\0\x01\0"));
        assert!(is_raster_picture(b"RIFF\x24\0\0\0WEBPVP8 "));
        // An .ico with one image in it.
        assert!(is_raster_picture(&[0, 0, 1, 0, 1, 0, 16, 16]));
        // ...and one that claims none.
        assert!(!is_raster_picture(&[0, 0, 1, 0, 0, 0, 16, 16]));
        assert!(!is_raster_picture(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"));
        assert!(!is_raster_picture(b"<?xml version=\"1.0\"?><svg/>"));
        assert!(!is_raster_picture(b"<html><body>404</body></html>"));
        assert!(!is_raster_picture(b""));
    }
}
