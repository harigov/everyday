//! "Fetch the full article": the linked page, over
//! [`crate::http::public_client`], through `dom_smoothie`'s Readability
//! port, sanitised by [`super::sanitize::body`] and kept as the story's body.
//!
//! The page is a stranger's, named by a feed, so it goes over the client
//! that refuses private addresses -- the same line [`crate::webpage`] draws
//! for the assistant's reading, and with the same caps. What comes out is
//! sanitised exactly as a feed's own content is, because a Readability port
//! decides what is the article, not what is safe.

use super::fetch::BROWSER_USER_AGENT;
use super::sanitize;
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::webpage::{self, Kind};
use dom_smoothie::{Config, Readability};
use everyday_core::Vault;
use everyday_core::id::StoryId;
use everyday_core::news::{BodyOrigin, StoryBody};

const ACCEPT: &str = "text/html, application/xhtml+xml;q=0.9, */*;q=0.5";

/// The most elements Readability will score before it gives up on a page.
/// Scoring is the slow part, and a page this large is an index or an
/// application, not an article.
const MAX_ELEMENTS: usize = 30_000;

/// Fetch, extract, sanitise and store the article `story` links to, and
/// return its new body. Replaces any body it had.
///
/// The vault is read before the request and written after it, each in one
/// short call made where it stands: a `&Vault` cannot be sent to the
/// blocking pool, and the half that is real work -- parsing the page and
/// sanitising what comes out -- is done there, on owned data.
pub async fn full_article(vault: &Vault, story: StoryId) -> CommandResult<StoryBody> {
    let record = vault.story(story)?;
    let address = record.url.trim();
    if address.is_empty() {
        return Err(CommandError::new(
            codes::INVALID,
            "this story has no page of its own to read the article from",
        ));
    }
    let url = webpage::parse(address).map_err(|e| CommandError::new(codes::INVALID, e))?;
    if let Some(ip) = literal_ip(&url)
        && http::is_forbidden_address(ip, false)
    {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "that address is on a private network, which this app will not read from",
        ));
    }

    let response = http::public_client()?
        .get(url)
        .header(reqwest::header::ACCEPT, ACCEPT)
        .header(reqwest::header::USER_AGENT, BROWSER_USER_AGENT)
        .send()
        .await
        .map_err(|e| describe(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CommandError::new(codes::NETWORK, explain_status(status)));
    }
    let landed = response.url().to_string();
    let declared = webpage::classify(
        response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
    );
    // Refused before the body is read, when the header already says it is not
    // a page: a forty-megabyte PDF is not worth downloading to find out.
    if matches!(declared, Kind::Unreadable(_)) {
        return Err(not_a_page());
    }
    let body = http::read_capped(response, webpage::MAX_PAGE_BYTES, || {
        format!(
            "that page is larger than {} MB, which is more than this app will read",
            webpage::MAX_PAGE_BYTES / 1_048_576
        )
    })
    .await?;
    let kind = match declared {
        Kind::Unknown => webpage::sniff(&body),
        known => known,
    };
    if !matches!(kind, Kind::Html) {
        return Err(not_a_page());
    }

    // Lossy rather than fatal, for the reason `webpage::render` gives: one
    // mangled character in an article beats refusing it.
    let html = String::from_utf8_lossy(&body).into_owned();
    drop(body);
    let id = record.id;
    let article =
        tokio::task::spawn_blocking(move || extract(id, &html, &landed)).await.map_err(|e| {
            CommandError::new(codes::INTERNAL, format!("reading the page failed: {e}"))
        })??;

    vault.save_story_body(&article)?;
    Ok(article)
}

/// The article on a page, sanitised for `story`. Pure: the half of
/// [`full_article`] that needs no socket and no vault.
fn extract(story: StoryId, html: &str, page_url: &str) -> CommandResult<StoryBody> {
    let config = Config { max_elements_to_parse: MAX_ELEMENTS, ..Config::default() };
    let mut readability =
        Readability::new(html, Some(page_url), Some(config)).map_err(|_| no_article())?;
    let article = readability.parse().map_err(|_| no_article())?;
    if article.text_content.trim().is_empty() {
        return Err(no_article());
    }
    Ok(sanitize::body(story, &article.content, page_url, BodyOrigin::Extracted))
}

fn no_article() -> CommandError {
    CommandError::new(
        codes::UNREADABLE,
        "no article could be found on that page. Open the original to read it there.",
    )
}

fn not_a_page() -> CommandError {
    CommandError::new(
        codes::UNSUPPORTED,
        "that address is not a web page, so there is no article to read. Open the original.",
    )
}

/// The address itself, when the host is one rather than a name -- the one
/// case the public client's resolver never sees. Brackets off an IPv6
/// literal first, for the reason `http`'s redirect policy gives.
fn literal_ip(url: &reqwest::Url) -> Option<std::net::IpAddr> {
    url.host_str()?.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

fn explain_status(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => format!("that site refused to show the page to this app ({status})."),
        404 | 410 => "there is no page at that address any more.".to_string(),
        429 => "that site asked us to slow down. Try again in a minute.".to_string(),
        500..=599 => format!("that site is having trouble ({status})."),
        _ => format!("that site answered {status}."),
    }
}

/// A transport failure in words, without the address.
fn describe(e: &reqwest::Error) -> CommandError {
    // The public client's resolver refuses a private answer with a sentence
    // of its own; reqwest buries it a few sources down.
    let mut source: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = source {
        if err.to_string().contains("private network") {
            return CommandError::new(
                codes::FORBIDDEN,
                "that address points at a private network, which this app will not read from",
            );
        }
        source = err.source();
    }
    let message = if e.is_timeout() {
        "that site did not answer in time.".to_string()
    } else if e.is_connect() {
        "could not reach that site. Check the address, and the network.".to_string()
    } else if e.is_redirect() {
        "that site redirected too many times, or somewhere this app will not follow.".to_string()
    } else {
        format!("reading the page failed: {}", http::strip_url(&e.to_string()))
    };
    CommandError::new(codes::NETWORK, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page long enough, and plain enough, for Readability to settle on:
    /// an article inside a page of furniture.
    fn page() -> String {
        let paragraph = "The committee met on Tuesday to discuss the harbour, the ferries and \
            the long-running question of who should pay for the new breakwater. ";
        format!(
            r#"<html><head><title>Harbour plans - The Gazette</title></head><body>
            <nav><a href="/">Home</a> <a href="/news">News</a></nav>
            <article>
              <h1>Harbour plans</h1>
              <p>{p}{p}{p}</p>
              <p>{p}{p}{p}</p>
              <img src="/img/harbour.jpg" width="600" height="400">
              <p>{p}{p}{p}</p>
              <script>steal()</script>
            </article>
            <footer>Copyright 2026</footer></body></html>"#,
            p = paragraph
        )
    }

    #[test]
    fn the_article_is_found_sanitised_and_marked_extracted() {
        let story = StoryId::new();
        let body = extract(story, &page(), "https://gazette.example/2026/10/harbour").unwrap();

        assert_eq!(body.story_id, story);
        assert_eq!(body.origin, BodyOrigin::Extracted);
        assert!(body.text.contains("breakwater"), "{}", body.text);
        assert!(body.html.contains("breakwater"), "{}", body.html);
        assert!(!body.html.contains("steal"), "{}", body.html);
        // The picture was made absolute against the page, then proxied.
        assert_eq!(body.images.len(), 1);
        assert_eq!(body.images[0].url, "https://gazette.example/img/harbour.jpg");
        assert!(body.html.contains(&format!("everyday://news/img/{story}/")), "{}", body.html);
    }

    #[test]
    fn a_page_with_no_article_says_so_rather_than_storing_nothing() {
        let err = extract(StoryId::new(), "<html><body></body></html>", "https://gazette.example/")
            .unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
        assert!(err.message.contains("Open the original"), "{}", err.message);
    }

    #[test]
    fn a_relative_page_address_is_refused_by_the_extractor() {
        let err = extract(StoryId::new(), &page(), "/not/absolute").unwrap_err();
        assert_eq!(err.code, codes::UNREADABLE);
    }

    #[test]
    fn a_status_is_explained_in_words() {
        assert!(explain_status(reqwest::StatusCode::NOT_FOUND).contains("no page"));
        assert!(explain_status(reqwest::StatusCode::FORBIDDEN).contains("refused"));
        assert!(explain_status(reqwest::StatusCode::BAD_GATEWAY).contains("502"));
    }

    #[test]
    fn only_a_literal_address_counts_as_one() {
        let ip = |s: &str| literal_ip(&reqwest::Url::parse(s).unwrap());
        assert!(ip("http://127.0.0.1/a").is_some());
        assert!(ip("http://[::1]/a").is_some());
        assert!(ip("https://example.com/a").is_none());
    }
}
