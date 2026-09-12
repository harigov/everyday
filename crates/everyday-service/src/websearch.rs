//! Looking things up on the web, and bringing a cover home.
//!
//! The other half of [`everyday_core::websearch`], and deliberately the
//! smaller one. The core builds every URL, parses every reply and decides
//! what a result does to an item; this file opens the socket and hands the
//! bytes back. Nothing here knows what a book is.
//!
//! That is the same split [`crate::feeds`] makes for calendar feeds, and it
//! is why "the lookup got the year wrong" is a test in the core rather than
//! a network capture — see the core module's docs for the argument.
//!
//! # What leaves the machine, and when
//!
//! A query string, to one endpoint, because somebody pressed a button. There
//! is no lookup as you type, no background enrichment of a shelf and no
//! identifier of yours in any request — the core builds the URLs and adds
//! nothing to them. Every endpoint works without an API key or an account,
//! which is a deliberate constraint and not a coincidence: see
//! [`Source`](everyday_core::websearch::Source).
//!
//! # Why a cover is downloaded rather than linked
//!
//! [`fetch_image`] pulls the picture into the vault's blob store, where it
//! is chunk-encrypted like every photograph in a journal entry, and the
//! interface renders it through the `everyday://` protocol. A
//! `<img src="https://covers…">` would have been less code and would have
//! told a stranger's server which books are on your shelf every time you
//! opened the app. It is also not available: the webview's content security
//! policy allows images from `'self'` and `everyday:` and from nowhere else,
//! which is the check that survives somebody later forgetting the reason.

use std::sync::Arc;

use everyday_core::websearch::{
    MAX_IMAGE_BYTES, MAX_RESPONSE_BYTES, Request, SearchRequest, SearchResult, http_url,
};
use everyday_core::{Item, Kind, Vault};

/// How many results a lookup asks for when the caller has no opinion.
pub use everyday_core::websearch::DEFAULT_LIMIT;

use crate::error::{CommandError, CommandResult, codes};
use crate::http;

/// Run one search: build the request in the core, fetch it here, parse it in
/// the core.
pub async fn search(request: &SearchRequest) -> CommandResult<Vec<SearchResult>> {
    let built = request.source.request(request)?;
    let body = get(&built).await?;
    let mut hits = request.source.parse(&body).map_err(|e| {
        // A parse failure is a source that changed its mind about its own
        // format, which is not the person's fault and not something they can
        // act on. Say what happened rather than quoting a serde error.
        tracing::warn!(source = request.source.slug(), error = %e, "could not read the results");
        CommandError::new(
            codes::UNREADABLE,
            format!("{} answered with something this app could not read", request.source.label()),
        )
    })?;
    hits.truncate(request.limit.clamp(1, 50) as usize);
    Ok(hits)
}

/// Look something up on behalf of a shelf, trying the sources the core says
/// to try, in the order it says to try them.
///
/// The order — and the fallback to a plain web search when a specialised
/// source draws a blank — is
/// [`SearchRequest::attempts`](everyday_core::websearch::SearchRequest::attempts),
/// so this function and the core's synchronous
/// [`WebSearch`](everyday_core::websearch::WebSearch) cannot drift apart on
/// the policy. All this adds is the `await`.
pub async fn lookup(query: &str, kind: &Kind, limit: u32) -> CommandResult<Vec<SearchResult>> {
    let mut last: Option<CommandError> = None;
    for attempt in SearchRequest::for_kind(query, kind).limit(limit).attempts() {
        match search(&attempt).await {
            Ok(hits) if !hits.is_empty() => return Ok(hits),
            Ok(_) => {}
            // A source that is down must not stop the fallback from running.
            // The error is kept in case *every* attempt fails, because "no
            // results" and "the network is off" are different things to be
            // told.
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e),
        None => Ok(Vec::new()),
    }
}

/// GET one prepared request and return the body as text.
async fn get(request: &Request) -> CommandResult<String> {
    let response = http::client()?
        .get(&request.url)
        .header(reqwest::header::ACCEPT, request.accept)
        .send()
        .await
        .map_err(|e| describe(&e, request))?;

    let status = response.status();
    if !status.is_success() {
        return Err(CommandError::new(codes::NETWORK, explain_status(status, request)));
    }

    let body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
        format!("{} sent more than this app will read", request.source.label())
    })
    .await?;
    // Not every endpoint is scrupulous about its encoding. Lossy rather than
    // fatal: one mangled character in a blurb beats refusing the lookup.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Fetch a cover and return the bytes, having checked that they are a
/// picture.
///
/// The type is *sniffed*, not read from the `Content-Type` header, for the
/// reason [`everyday_core::media::sniff_mime`] exists: a header is a claim
/// by somebody else's server, and what ends up in the vault should be what
/// the bytes actually are. Anything that is not a recognised image is
/// refused rather than stored — a shelf full of `application/octet-stream`
/// covers would render as broken pictures with no way to find out why.
pub async fn fetch_image(url: &str) -> CommandResult<Vec<u8>> {
    // Checked before the request, not after: the address comes off the
    // internet, and `file:///etc/passwd` reaching a fetcher is the bug this
    // ordering prevents.
    let url = http_url(url).map_err(CommandError::from)?;
    let response = http::client()?
        .get(&url)
        .header(reqwest::header::ACCEPT, "image/*")
        .send()
        .await
        .map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("the picture could not be fetched: {}", http::strip_url(&e.to_string())),
            )
        })?;
    if !response.status().is_success() {
        return Err(CommandError::new(
            codes::NETWORK,
            format!("the picture could not be fetched ({}).", response.status()),
        ));
    }
    let bytes = http::read_capped(response, MAX_IMAGE_BYTES, || {
        format!(
            "that picture is larger than {} MB, which is more than a cover should be",
            MAX_IMAGE_BYTES / 1_048_576
        )
    })
    .await?;

    let mime = everyday_core::media::sniff_mime(&bytes);
    if !mime.starts_with("image/") {
        return Err(CommandError::new(
            codes::NOT_AN_IMAGE,
            "that address did not return a picture, so nothing has been saved.",
        ));
    }
    Ok(bytes)
}

fn explain_status(status: reqwest::StatusCode, request: &Request) -> String {
    let who = request.source.label();
    match status.as_u16() {
        401 | 403 => format!("{who} refused the request."),
        404 | 410 => format!("{who} had nothing at that address."),
        429 => format!("{who} asked us to slow down. Try again in a minute."),
        500..=599 => format!("{who} is having trouble ({status})."),
        _ => format!("{who} answered {status}."),
    }
}

fn describe(e: &reqwest::Error, request: &Request) -> CommandError {
    let who = request.source.label();
    let message = if e.is_timeout() {
        format!("{who} did not answer in time.")
    } else if e.is_connect() {
        format!("could not reach {who}. Check the network.")
    } else if e.is_redirect() {
        format!("{who} redirected too many times.")
    } else {
        // Deliberately without the URL: it carries the query, and a query is
        // something the person typed.
        format!("the lookup failed: {}", http::strip_url(&e.to_string()))
    };
    CommandError::new(codes::NETWORK, message)
}

/// Apply a result to an item and bring its cover home, in that order.
///
/// The two belong together because the second depends on the first: the core
/// decides whether `cover_url` is allowed to change, and only then is there
/// an address worth fetching. Splitting them across two call sites is how a
/// re-fetch ends up downloading the old picture.
///
/// Best-effort about the picture, and deliberately so. A cover that will not
/// download is a card without artwork; refusing the metadata over it would
/// be a card with no title either, which is worse in every case. The failure
/// is logged, `cover_url` is kept, and the interface can try again later.
pub async fn apply_and_cover(
    vault: &Arc<Vault>,
    result: &SearchResult,
    kind: &Kind,
    item: &mut Item,
    overwrite: bool,
) {
    everyday_core::websearch::apply(result, kind, item, overwrite);

    let wanted = item.cover_url.trim().to_string();
    if wanted.is_empty() || (item.cover.is_some() && !overwrite) {
        return;
    }
    match fetch_image(&wanted).await {
        Ok(bytes) => {
            let vault = vault.clone();
            // Sealing and writing a few hundred kilobytes is disk work; it
            // does not belong on the async runtime's threads.
            let stored = tokio::task::spawn_blocking(move || vault.put_blob(&bytes)).await;
            match stored {
                Ok(Ok(blob)) => item.cover = Some(blob),
                Ok(Err(e)) => tracing::warn!(error = %e, "could not store a cover"),
                Err(e) => tracing::warn!(error = %e, "the cover write panicked"),
            }
        }
        Err(e) => tracing::info!(error = %e, "could not fetch a cover"),
    }
}
