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
//! And, once something is picked from a plain web search or an address is
//! pasted, one request to that page, to read what it says about itself --
//! a recipe's ingredients, an article's picture. It goes through
//! [`webpage::fetch`], the assistant's own careful way of reading a
//! stranger's page, because the address came off the internet.
//!
//! A search for pictures ([`search_images`]) sends its words to Bing, and to
//! Wikimedia Commons when Bing has nothing; see
//! [`everyday_core::websearch::images`] for why those two. The grid that
//! shows the answers asks for each thumbnail through [`preview`], on the
//! same terms as a cover, and keeps none of them.
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

use everyday_core::id::BlobId;
use everyday_core::websearch::images::{ImageResult, ImageSource, MAX_IMAGES};
use everyday_core::websearch::{
    MAX_IMAGE_BYTES, MAX_RESPONSE_BYTES, Request, SearchRequest, SearchResult, Source, http_url,
};
use everyday_core::{Item, Kind, Vault};

/// How many results a lookup asks for when the caller has no opinion.
pub use everyday_core::websearch::DEFAULT_LIMIT;

use crate::error::{CommandError, CommandResult, codes};
use crate::{covers, http, webpage};

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
    // A shelf that never looks anything up -- people -- sends nothing, even
    // when a client asks it to.
    if !kind.looks_things_up() {
        return Ok(Vec::new());
    }
    // A pasted address is answered with itself, and nothing is searched for
    // it: the person has already said which page they mean.
    if let Some(page) = SearchResult::for_address(query) {
        return Ok(vec![page]);
    }
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
/// picture. As they came: making them a cover's size is [`covers::store`]'s
/// job, at the moment they are kept.
///
/// The type is *sniffed*, not read from the `Content-Type` header, for the
/// reason [`everyday_core::media::sniff_mime`] exists: a header is a claim
/// by somebody else's server, and what ends up in the vault should be what
/// the bytes actually are. Anything that is not a recognised image is
/// refused rather than stored — a shelf full of `application/octet-stream`
/// covers would render as broken pictures with no way to find out why.
///
/// Over [`http::public_client`], whatever the picture is for. A cover's
/// address is always somebody else's choice -- a catalogue's answer, the
/// head of a page somebody picked, an address copied out of a browser -- and
/// a page can name `http://192.168.1.1/…` as its picture as easily as a
/// photograph. So no name may resolve to a private network, no redirect may
/// lead to one, and a literal private address is refused before anything is
/// sent; mail's remote images are fetched on the same terms.
pub async fn fetch_image(url: &str) -> CommandResult<Vec<u8>> {
    let too_big = || {
        format!(
            "that picture is larger than {} MB, which is more than a cover should be",
            MAX_IMAGE_BYTES / 1_048_576
        )
    };
    fetch_picture(url, MAX_IMAGE_BYTES, too_big).await.map(|(bytes, _)| bytes)
}

/// The largest preview [`preview`] will pass on. A grid's thumbnail is tens
/// of kilobytes; this is room for a search whose thumbnail is the original.
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

/// A picture for a grid to show before anything is chosen, as a `data:`
/// address.
///
/// Not stored: a cover picker shows a dozen of these and keeps at most one,
/// and the one it keeps is fetched again, full size, by [`fetch_image`]. The
/// webview cannot load the address itself -- see this module's doc on why
/// its content security policy allows `data:` and not the internet -- so the
/// bytes come through here, on the same terms as a cover's.
pub async fn preview(url: &str) -> CommandResult<String> {
    use base64::Engine;
    let too_big = || "that preview is larger than a preview should be".to_string();
    let (bytes, mime) = fetch_picture(url, MAX_PREVIEW_BYTES, too_big).await?;
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

/// Look for pictures of something: the core's
/// [`ImageSource::ATTEMPTS`](everyday_core::websearch::images::ImageSource::ATTEMPTS)
/// in turn, until one finds any.
///
/// The way [`lookup`] falls back, and for its reason: a search that is down
/// must not stop the next from being asked, and "nothing found" and "the
/// network is off" are different things to be told when every one fails.
pub async fn search_images(query: &str, limit: u32) -> CommandResult<Vec<ImageResult>> {
    let mut last: Option<CommandError> = None;
    for source in ImageSource::ATTEMPTS {
        match images_from(source, query, limit).await {
            Ok(found) if !found.is_empty() => return Ok(found),
            Ok(_) => {}
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e),
        None => Ok(Vec::new()),
    }
}

async fn images_from(
    source: ImageSource,
    query: &str,
    limit: u32,
) -> CommandResult<Vec<ImageResult>> {
    let url = source.url(query, limit).map_err(CommandError::from)?;
    let who = source.label();
    let response = http::client()?
        .get(&url)
        .header(reqwest::header::ACCEPT, source.accept())
        .send()
        .await
        .map_err(|e| {
            // Without the URL, as `describe` leaves it out: it carries the
            // words searched for.
            CommandError::new(
                codes::NETWORK,
                format!("could not search {who}: {}", http::strip_url(&e.to_string())),
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(CommandError::new(codes::NETWORK, format!("{who} answered {status}.")));
    }
    let body = http::read_capped(response, MAX_RESPONSE_BYTES, || {
        format!("{who} sent more than this app will read")
    })
    .await?;
    let mut found = source.parse(&String::from_utf8_lossy(&body)).map_err(|e| {
        tracing::warn!(source = source.slug(), error = %e, "could not read the pictures");
        CommandError::new(
            codes::UNREADABLE,
            format!("{who} answered with something this app could not read"),
        )
    })?;
    found.truncate(limit.clamp(1, MAX_IMAGES) as usize);
    Ok(found)
}

/// GET a picture somebody else chose the address of, refusing anything on a
/// private network and anything that is not a picture once it arrives. The
/// bytes, and what they were sniffed to be. See [`fetch_image`] for the
/// argument; `too_big` is the sentence for a picture over `cap`.
async fn fetch_picture(
    url: &str,
    cap: usize,
    too_big: impl Fn() -> String,
) -> CommandResult<(Vec<u8>, &'static str)> {
    // Checked before the request, not after: the address comes off the
    // internet, and `file:///etc/passwd` reaching a fetcher is the bug this
    // ordering prevents.
    let url = http_url(url).map_err(CommandError::from)?;
    let parsed = webpage::parse(&url).map_err(|e| CommandError::new(codes::INVALID, e))?;
    if webpage::literal_ip(&parsed).is_some_and(|ip| http::is_forbidden_address(ip, false)) {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "that picture is on a private network, which this app will not fetch from",
        ));
    }
    let response = http::public_client()?
        .get(parsed)
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
    let bytes = http::read_capped(response, cap, too_big).await?;

    let mime = everyday_core::media::sniff_mime(&bytes);
    if !mime.starts_with("image/") {
        return Err(CommandError::new(
            codes::NOT_AN_IMAGE,
            "that address did not return a picture, so nothing has been saved.",
        ));
    }
    Ok((bytes, mime))
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

/// Ask a source for the rest of what it knows about the one result somebody
/// chose.
///
/// Steam has a store page to add, and a plain web search has the page
/// itself -- see
/// [`Source::detail`](everyday_core::websearch::Source::detail) for why the
/// request is made on a pick rather than for every hit in a list. Everything
/// here is best-effort by construction: no second request, a refused one, an
/// unreadable answer or a store that has never heard of the game all leave
/// the result exactly as the search returned it, which is already enough to
/// put on a shelf.
///
/// Also how the assistant's `look_up_item` reads a page it was given the
/// address of.
pub(crate) async fn enriched(result: &SearchResult) -> SearchResult {
    let source = Source::from_slug(&result.source);
    let Some(request) = source.detail(result) else { return result.clone() };
    let fetched = if request.page {
        page(&request.url).await
    } else {
        get(&request).await.map(|body| (body, request.url.clone()))
    };
    let (body, landed) = match fetched {
        Ok(fetched) => fetched,
        Err(e) => {
            tracing::info!(source = source.slug(), error = %e, "no details this time");
            return result.clone();
        }
    };
    // Off the runtime's threads: a page can be a few megabytes of markup,
    // and reading it is real work -- the reason `webpage::read` gives.
    let mut filled = result.clone();
    let merged = tokio::task::spawn_blocking(move || {
        if request.page {
            everyday_core::websearch::merge_page_at(&mut filled, &body, &landed);
            Ok(filled)
        } else {
            source.merge_detail(&mut filled, &body).map(|()| filled)
        }
    })
    .await;
    match merged {
        Ok(Ok(filled)) => filled,
        Ok(Err(e)) => {
            tracing::warn!(source = source.slug(), error = %e, "could not read the details");
            result.clone()
        }
        Err(e) => {
            tracing::warn!(source = source.slug(), error = %e, "reading the details panicked");
            result.clone()
        }
    }
}

/// A page somebody picked, as text, and the address it was found at after
/// any redirects -- fetched the way the assistant fetches one: see
/// [`webpage::fetch`] for the checks. Only a page that is markup is worth
/// reading for what it says about itself.
async fn page(address: &str) -> CommandResult<(String, String)> {
    let fetched = webpage::fetch(address).await?;
    if fetched.kind != webpage::Kind::Html {
        return Err(CommandError::new(codes::UNSUPPORTED, "that address is not a web page"));
    }
    Ok((String::from_utf8_lossy(&fetched.body).into_owned(), fetched.landed.to_string()))
}

/// Apply a result to an item and bring its cover home, in that order.
///
/// The two belong together because the second depends on the first: the core
/// decides whether `cover_url` is allowed to change, and only then is there
/// an address worth fetching. Splitting them across two call sites is how a
/// re-fetch ends up downloading the old picture.
///
/// The detail request in front of it is best-effort for the same reason and
/// in the same way -- see [`enriched`].
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
    let detailed = enriched(result).await;
    everyday_core::websearch::apply_detailed(result, &detailed, kind, item, overwrite);

    let wanted = item.cover_url.trim().to_string();
    if wanted.is_empty() || (item.cover.is_some() && !overwrite) {
        return;
    }
    match fetch_image(&wanted).await {
        // Made a cover's size on the way in, off the runtime's threads -- see
        // `covers::store`.
        Ok(bytes) => match covers::store(vault, bytes).await {
            Ok(blob) => item.cover = Some(blob),
            Err(e) => tracing::warn!(error = %e, "could not store a cover"),
        },
        Err(e) => tracing::info!(error = %e, "could not fetch a cover"),
    }
}

/// The core's library tools that take a cover by address, in `image_url`.
/// Named here for [`prefetch_cover`], and read by the assistant's gate, which
/// holds that address to the check a page's is held to.
pub(crate) const COVER_TOOLS: [&str; 2] = ["create_item", "update_item"];

/// What a library tool's `image_url` came to, fetched before the tool ran.
/// See [`prefetch_cover`].
#[derive(Debug)]
pub(crate) enum CoverFetch {
    /// The picture is in the blob store, waiting for the item.
    Fetched { url: String, blob: BlobId },
    /// It could not be fetched, and `image_url` was taken out of the call so
    /// that the item keeps whatever cover it had.
    Failed { reason: String },
}

/// Fetch the picture a `create_item` or `update_item` call names in its
/// `image_url`, before the call runs; `None` for any other call, or one with
/// no picture in it.
///
/// Before rather than after, for what a failure does. The core tool forgets
/// an item's old cover the moment it is given a new address (see
/// `set_cover_address` in `everyday_core::agent::tools::library`); a picture
/// that then refused to download would leave a book that had a cover with
/// none. Fetched first, a refusal takes `image_url` out of `arguments` and
/// the rest of the call goes ahead exactly as if no picture had been named --
/// a new item without artwork, an old one with the cover it had -- and
/// [`settle_cover`] tells the model why, so it can try another. The same
/// trade [`apply_and_cover`] makes: a card without artwork beats no card.
///
/// Through [`fetch_image`], so on a cover's terms: public addresses only,
/// and bytes that sniff as a picture. Whether this address may be fetched
/// *at all* is the caller's to have decided -- the assistant's gate holds
/// it to the provenance check `read_web_page` is held to, and a script needs
/// the web scope.
pub(crate) async fn prefetch_cover(
    vault: &Arc<Vault>,
    tool: &str,
    arguments: &mut serde_json::Value,
) -> Option<CoverFetch> {
    if !COVER_TOOLS.contains(&tool) {
        return None;
    }
    let given = arguments.get("image_url")?.as_str()?.trim().to_string();
    // An empty address takes a cover away, and a malformed one is the core's
    // to refuse in words the model will read. Neither is fetched.
    let url = http_url(&given).ok()?;
    let fetched = match fetch_image(&url).await {
        // Made a cover's size on the way in, as every cover is -- see
        // `covers::store`.
        Ok(bytes) => covers::store(vault, bytes)
            .await
            .map_err(|e| format!("it could not be stored: {}", e.message)),
        Err(e) => Err(e.message),
    };
    Some(match fetched {
        Ok(blob) => CoverFetch::Fetched { url, blob },
        Err(reason) => {
            if let Some(map) = arguments.as_object_mut() {
                map.remove("image_url");
            }
            tracing::info!(tool, reason = %reason, "a cover the assistant named was not fetched");
            CoverFetch::Failed { reason }
        }
    })
}

/// Finish what [`prefetch_cover`] started, once the tool has run and
/// answered `result`: put the fetched picture on the item the call saved,
/// and say in `result` what became of it.
///
/// Synchronous, because both callers already run the tool on the blocking
/// pool and this is one read and one write beside it. The item is matched by
/// the address as well as the id: a dream's create is a proposal with no
/// item behind it, and a cover is only put on an item whose `cover_url`
/// still names the picture that was fetched.
pub(crate) fn settle_cover(
    vault: &Vault,
    fetched: Option<CoverFetch>,
    result: &mut serde_json::Value,
) {
    let Some(fetched) = fetched else { return };
    let note = match fetched {
        CoverFetch::Failed { reason } => format!(
            "not saved -- the picture could not be fetched ({}). Everything else was saved; \
             try another picture.",
            reason.trim_end_matches('.')
        ),
        CoverFetch::Fetched { url, blob } => {
            let id = result.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
            let attached =
                id.parse().ok().and_then(|id| vault.item(id).ok()).and_then(|mut item| {
                    (item.cover_url == url).then(|| {
                        item.cover = Some(blob);
                        vault.save_item(&item)
                    })
                });
            match attached {
                Some(Ok(())) => "saved".to_string(),
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "could not put a fetched cover on its item");
                    "not saved -- the item could not be updated with it".to_string()
                }
                None => return,
            }
        }
    };
    if let Some(map) = result.as_object_mut() {
        map.insert("cover".into(), serde_json::Value::String(note));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_vault() -> (Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            dir.path(),
            everyday_core::VaultConfig { password: None, ..Default::default() },
        )
        .unwrap();
        vault.seed_library().unwrap();
        (Arc::new(vault), dir)
    }

    fn a_book(vault: &Vault, cover_url: &str) -> Item {
        let shelf = vault.kinds().unwrap().into_iter().find(|k| k.slug == "book").unwrap();
        let mut item = Item::new(shelf.id, "Dune");
        item.cover_url = cover_url.to_string();
        vault.save_item(&item).unwrap();
        item
    }

    #[tokio::test]
    async fn only_a_library_write_with_a_web_address_for_a_cover_fetches_anything() {
        let (vault, _dir) = test_vault();
        for (tool, mut arguments) in [
            ("create_note", json!({ "image_url": "https://example.com/a.png" })),
            ("create_item", json!({ "title": "Dune" })),
            ("update_item", json!({ "item_id": "x", "image_url": "  " })),
            ("update_item", json!({ "item_id": "x", "image_url": "file:///etc/passwd" })),
        ] {
            let before = arguments.clone();
            assert!(
                prefetch_cover(&vault, tool, &mut arguments).await.is_none(),
                "{tool} {before}"
            );
            assert_eq!(arguments, before, "and the call is left as it was");
        }
    }

    #[tokio::test]
    async fn a_cover_that_will_not_come_is_taken_out_of_the_call_and_said_so() {
        let (vault, _dir) = test_vault();
        let item = a_book(&vault, "");
        // Refused before anything is sent, so this needs no network.
        let mut arguments =
            json!({ "item_id": item.id.to_string(), "image_url": "http://192.168.1.1/cover.jpg" });
        let fetched = prefetch_cover(&vault, "update_item", &mut arguments).await;
        assert!(matches!(fetched, Some(CoverFetch::Failed { .. })), "got {fetched:?}");
        assert!(
            arguments.get("image_url").is_none(),
            "the rest of the call goes ahead, leaving the cover it had"
        );

        let mut result = json!({ "ok": true, "action": "updated", "id": item.id.to_string() });
        settle_cover(&vault, fetched, &mut result);
        let note = result["cover"].as_str().unwrap();
        assert!(note.starts_with("not saved") && note.contains("private network"), "got {note}");
    }

    #[test]
    fn a_fetched_cover_lands_on_the_item_that_still_names_it() {
        let (vault, _dir) = test_vault();
        let url = "https://covers.example/dune.jpg";
        let item = a_book(&vault, url);
        let blob = vault.put_blob(b"a picture").unwrap();

        let mut result = json!({ "ok": true, "action": "created", "id": item.id.to_string() });
        settle_cover(&vault, Some(CoverFetch::Fetched { url: url.into(), blob }), &mut result);
        assert_eq!(result["cover"], "saved");
        assert_eq!(vault.item(item.id).unwrap().cover, Some(blob));

        // An item that names another picture by now is left alone, and so is
        // a result with no item behind it -- a dream's proposal.
        let other = a_book(&vault, "https://covers.example/other.jpg");
        let mut result = json!({ "ok": true, "id": other.id.to_string() });
        settle_cover(&vault, Some(CoverFetch::Fetched { url: url.into(), blob }), &mut result);
        assert!(result.get("cover").is_none(), "got {result}");
        assert_eq!(vault.item(other.id).unwrap().cover, None);
        let mut proposed = json!({ "ok": true, "action": "proposed" });
        settle_cover(&vault, Some(CoverFetch::Fetched { url: url.into(), blob }), &mut proposed);
        assert!(proposed.get("cover").is_none(), "got {proposed}");
    }

    #[tokio::test]
    async fn a_picture_on_a_private_network_is_refused_before_anything_is_sent() {
        // What a page can name as its picture as easily as a photograph:
        // this machine, the router, a cloud's metadata service.
        for url in [
            "http://127.0.0.1:6167/_matrix/key",
            "http://192.168.1.1/cgi-bin/reboot",
            "http://[::1]/cover.jpg",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let err = fetch_image(url).await.unwrap_err();
            assert_eq!(err.code, codes::FORBIDDEN, "{url}: {}", err.message);
        }
    }
}
