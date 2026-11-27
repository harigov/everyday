//! "Fetch the full article": the linked page, over
//! [`crate::http::public_client`], through FiveFilters' rules for its site
//! ([`super::rules`]) and `dom_smoothie`'s Readability port, sanitised by
//! [`super::sanitize::body`] and kept as the story's body.
//!
//! The page is a stranger's, named by a feed, so it goes over the client
//! that refuses private addresses -- the same line [`crate::webpage`] draws
//! for the assistant's reading, and with the same caps. What comes out is
//! sanitised exactly as a feed's own content is, because a Readability port
//! decides what is the article, not what is safe.

use super::{rules, sanitize};
use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::http::BROWSER_USER_AGENT;
use crate::webpage::{self, Kind, literal_ip};
use dom_smoothie::{Config, Readability};
use jatayu_core::Vault;
use jatayu_core::id::StoryId;
use jatayu_core::news::{BodyOrigin, StoryBody};

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
///
/// The page goes through the site's rules first ([`rules`]): what they strip
/// is gone before anything else reads it, and where the site has a rule for
/// its article, that article is the one kept -- unless Readability finds far
/// more on the same page, which is a rule the site has outgrown
/// ([`rules::holds_up`]). Readability reads every page either way, for what
/// the page says about itself.
///
/// Besides the article, what a reader is told above it: who wrote it and
/// what the publication calls itself, from the site's rules or the page's
/// own metadata, and the page's picture for itself when the article starts
/// without one -- a lead picture often sits outside the element kept.
fn extract(story: StoryId, html: &str, page_url: &str) -> CommandResult<StoryBody> {
    let prepared = rules::prepare(html, page_url);
    let config = Config { max_elements_to_parse: MAX_ELEMENTS, ..Config::default() };
    let parsed = Readability::new(prepared.page.as_str(), Some(page_url), Some(config))
        .ok()
        .and_then(|mut readability| readability.parse().ok())
        .filter(|article| !article.text_content.trim().is_empty());
    let found = parsed.as_ref().map_or(0, |article| letters(&article.text_content));
    let rule = prepared
        .article
        .as_ref()
        .filter(|rule| rules::holds_up(rule.text_chars, found))
        .map(|rule| rule.html.as_str());
    let image = parsed.as_ref().and_then(|article| article.image.as_deref());
    // The rule's article first when it holds up, then Readability's: the first
    // with words left once it is sanitised is the body.
    let candidates = rule.into_iter().chain(parsed.as_ref().map(|article| &*article.content));
    for content in candidates {
        let content = with_lead_picture(content, image, page_url);
        let mut body = sanitize::body(story, &content, page_url, BodyOrigin::Extracted);
        if body.text.trim().is_empty() {
            continue;
        }
        body.byline = byline(prepared.byline.as_deref())
            .or_else(|| byline(parsed.as_ref().and_then(|article| article.byline.as_deref())));
        body.site_name = label(parsed.as_ref().and_then(|article| article.site_name.as_deref()));
        return Ok(body);
    }
    Err(no_article())
}

/// How many letters `text` has, not counting the space between them: what a
/// site rule's article and Readability's are weighed by.
fn letters(text: &str) -> usize {
    text.split_whitespace().map(str::len).sum()
}

/// The most characters a byline or a publication's name is kept at. Past
/// this it is a sentence Readability took for one, not a name.
const MAX_LABEL_CHARS: usize = 120;

/// A name from the page's metadata, on one line, or `None` when there is no
/// name there worth showing.
fn label(raw: Option<&str>) -> Option<String> {
    let text = raw?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty() && text.chars().count() <= MAX_LABEL_CHARS).then_some(text)
}

/// Who wrote it, as a name: a page's byline usually says "By Jane Doe", and
/// the header it is shown in says the rest.
fn byline(raw: Option<&str>) -> Option<String> {
    let text = label(raw)?;
    if text.eq_ignore_ascii_case("by") {
        return None;
    }
    // A page's `article:author` is as often its author's page as a name.
    if reqwest::Url::parse(&text).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
        return None;
    }
    let named = match text.get(..3) {
        Some(by) if by.eq_ignore_ascii_case("by ") => text[3..].trim_start(),
        _ => text.as_str(),
    };
    label(Some(named))
}

/// How far into an article a picture still counts as its lead: past the
/// first paragraph, or this many bytes when there is none, a picture is one
/// of the article's own and the lead is missing.
const LEAD_WINDOW: usize = 2_000;

/// `content` with the page's own picture of itself (`og:image`) above it,
/// when it starts without a picture and does not have that one further down.
///
/// The picture goes through the sanitiser like any other, so it is proxied
/// and held back with the rest when the source has pictures off.
fn with_lead_picture<'a>(
    content: &'a str,
    image: Option<&str>,
    page_url: &str,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    let Some(src) = image.and_then(|image| lead_address(image, page_url)) else {
        return Cow::Borrowed(content);
    };
    if looks_generic(&src) {
        return Cow::Borrowed(content);
    }
    let lower = content.to_ascii_lowercase();
    let first_paragraph = lower.find("</p>").unwrap_or_else(|| {
        let mut end = LEAD_WINDOW.min(lower.len());
        while !lower.is_char_boundary(end) {
            end -= 1;
        }
        end
    });
    let head = &lower[..first_paragraph];
    if head.contains("<img") || head.contains("<picture") || head.contains("<video") {
        return Cow::Borrowed(content);
    }
    // The same picture further down, at another size: the file's name, less
    // its extension and any `-1200x630` a publisher's resizing put on it, is
    // what stays the same across its copies.
    let name = src
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let stem = without_size(name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem));
    if stem.len() >= 6 && picture_addresses(content).contains(stem) {
        return Cow::Borrowed(content);
    }
    let src = src.as_str().replace('&', "&amp;").replace('"', "&quot;");
    Cow::Owned(format!("<figure class=\"lead\"><img src=\"{src}\" alt=\"\"></figure>\n{content}"))
}

/// What a page's own picture of itself is named when it is the site's and
/// not the article's: its logo, an icon, the card it shares every page with.
/// The name is all there is to go on.
const GENERIC_PICTURES: [&str; 7] =
    ["logo", "default", "placeholder", "fallback", "favicon", "icon", "avatar"];

/// Whether the page's picture is the site's rather than the article's, so it
/// is no lead: one of [`GENERIC_PICTURES`] as a word of its file's name
/// (`social-default.png`, `site_logo.jpg`, `sitelogo.png`, never
/// `silicon-valley.jpg`), or a drawing.
fn looks_generic(src: &reqwest::Url) -> bool {
    let name = src.path().rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    if name.ends_with(".svg") || name.ends_with(".ico") {
        return true;
    }
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| GENERIC_PICTURES.contains(&word) || word.ends_with("logo"))
}

/// Every picture address in `content`, lower-cased in one string: `src`,
/// `srcset` and the lazy-loading attributes of its `<img>` and `<source>`
/// elements, and nothing of its text.
fn picture_addresses(content: &str) -> String {
    let doc = dom_query::Document::fragment(content);
    let mut out = String::new();
    for node in doc.select("img, source").nodes() {
        for name in ["src", "srcset", "data-src", "data-srcset"] {
            if let Some(value) = node.attr(name) {
                out.push_str(&value.to_ascii_lowercase());
                out.push(' ');
            }
        }
    }
    out
}

/// `stem` without a size at its end, as in `photo-1200x630`.
fn without_size(stem: &str) -> &str {
    let Some((head, size)) = stem.rsplit_once('-') else { return stem };
    let numbers = size.split_once('x').is_some_and(|(w, h)| {
        !w.is_empty() && !h.is_empty() && (w.bytes().chain(h.bytes())).all(|b| b.is_ascii_digit())
    });
    if numbers { head } else { stem }
}

/// The lead picture's address, absolute against the page, when it is a web
/// address at all.
fn lead_address(image: &str, page_url: &str) -> Option<reqwest::Url> {
    let base = reqwest::Url::parse(page_url).ok()?;
    let url = base.join(image.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
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
        assert!(body.html.contains(&format!("jatayu://news/img/{story}/")), "{}", body.html);
    }

    /// [`page`] with what a publisher's head says about it.
    fn page_with_head(head: &str) -> String {
        page().replace("<head>", &format!("<head>{head}"))
    }

    #[test]
    fn the_page_says_who_wrote_it_and_what_it_calls_itself() {
        let html = page_with_head(
            r#"<meta name="author" content="By  Ada Lovelace">
               <meta property="og:site_name" content="The Gazette">"#,
        );
        let body =
            extract(StoryId::new(), &html, "https://gazette.example/2026/10/harbour").unwrap();
        assert_eq!(body.byline.as_deref(), Some("Ada Lovelace"));
        assert_eq!(body.site_name.as_deref(), Some("The Gazette"));

        let plain =
            extract(StoryId::new(), &page(), "https://gazette.example/2026/10/harbour").unwrap();
        assert_eq!(plain.site_name, None);
    }

    #[test]
    fn the_pages_picture_leads_an_article_that_starts_without_one() {
        let story = StoryId::new();
        let html = page_with_head(r#"<meta property="og:image" content="/img/lead-photo.jpg">"#);
        let body = extract(story, &html, "https://gazette.example/2026/10/harbour").unwrap();

        let urls: Vec<&str> = body.images.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://gazette.example/img/lead-photo.jpg",
                "https://gazette.example/img/harbour.jpg"
            ]
        );
        let lead = body.html.find("class=\"lead\"").expect("a lead figure");
        assert!(lead < body.html.find("breakwater").unwrap(), "{}", body.html);
    }

    #[test]
    fn the_pages_picture_is_not_added_twice() {
        // The article already has it, further down and at another size.
        let html = page_with_head(
            r#"<meta property="og:image" content="https://cdn.gazette.example/1200/harbour.jpg">"#,
        );
        let body =
            extract(StoryId::new(), &html, "https://gazette.example/2026/10/harbour").unwrap();
        assert_eq!(body.images.len(), 1, "{:?}", body.images);
        assert!(!body.html.contains("class=\"lead\""), "{}", body.html);
    }

    #[test]
    fn a_lead_is_only_added_where_one_is_missing() {
        let page_url = "https://gazette.example/a";
        let lead = |content: &str, image: Option<&str>| {
            with_lead_picture(content, image, page_url).into_owned()
        };
        // Already led by a picture.
        let pictured = "<p><img src=\"/a.jpg\">Caption</p><p>Text</p>";
        assert_eq!(lead(pictured, Some("/hero-image.jpg")), pictured);
        // The site's own picture, not the article's.
        assert_eq!(lead("<p>Text</p>", Some("/assets/social-default.png")), "<p>Text</p>");
        assert_eq!(lead("<p>Text</p>", Some("/brand/site-logo-1200.jpg")), "<p>Text</p>");
        assert_eq!(lead("<p>Text</p>", Some("/brand/mark.svg")), "<p>Text</p>");
        assert_eq!(lead("<p>Text</p>", Some("/img/sitelogo.png")), "<p>Text</p>");
        assert!(lead("<p>Text</p>", Some("/2026/silicon-valley.jpg")).contains("class=\"lead\""));
        // A word in the text that happens to be the picture's name is not
        // the picture.
        let named =
            lead("<p>Set up by harbour-view, the committee.</p>", Some("/up/harbour-view.jpg"));
        assert!(named.starts_with("<figure class=\"lead\">"), "{named}");
        // The same picture further down at another size.
        let resized = "<p>Text</p><p><img src=\"/up/photo-of-it-640x426.jpg\"></p>";
        assert_eq!(lead(resized, Some("/up/photo-of-it-1152x648.jpg?w=1")), resized);
        assert_eq!(without_size("photo-1200x630"), "photo");
        assert_eq!(without_size("well-known"), "well-known");
        assert_eq!(without_size("a-x1"), "a-x1");
        // Nothing to add, or nothing a browser should fetch.
        assert_eq!(lead("<p>Text</p>", None), "<p>Text</p>");
        assert_eq!(lead("<p>Text</p>", Some("javascript:alert(1)")), "<p>Text</p>");
        // Quotes in the address cannot leave the attribute.
        let quoted = lead("<p>Text</p>", Some("/a\"b.jpg?x=1&y=2"));
        assert!(
            quoted.contains("src=\"https://gazette.example/a%22b.jpg?x=1&amp;y=2\""),
            "{quoted}"
        );
        // A long article with no paragraph tags, and letters past the window
        // that are more than one byte, is looked through without a panic.
        // The one-byte "a" puts the window's edge inside a two-byte letter.
        let wide = format!("a{}", "é".repeat(LEAD_WINDOW));
        assert!(lead(&wide, Some("/hero-image.jpg")).starts_with("<figure class=\"lead\">"));
    }

    #[test]
    fn a_byline_is_a_name_on_one_line() {
        assert_eq!(byline(Some("By Jane Doe")).as_deref(), Some("Jane Doe"));
        assert_eq!(byline(Some("by\n  Jane   Doe ")).as_deref(), Some("Jane Doe"));
        assert_eq!(byline(Some("Byron Katie")).as_deref(), Some("Byron Katie"));
        assert_eq!(byline(Some("By ")), None);
        assert_eq!(byline(Some("   ")), None);
        assert_eq!(byline(None), None);
        assert_eq!(
            byline(Some("https://www.facebook.com/somepaper")),
            None,
            "an address is no name"
        );
        assert_eq!(label(Some(&"word ".repeat(40))), None, "a sentence is not a name");
    }

    /// A page on a site with its own rules in the bundle: `allthingsd.com`,
    /// whose article is its `.article-body` without the `memo` quotes in it,
    /// and whose author is named in its header. (An exact name: the
    /// `.allthingsd.com` wildcard further up the bundle is an older set,
    /// which it takes the place of.)
    fn ruled_page(post: &str, elsewhere: &str) -> String {
        format!(
            r#"<html><head><title>A post</title></head><body>
            <div class="page-header article-header clearfix"><p class="title">Kara  Swisher</p></div>
            <div class="article-body">{post}
              <blockquote class="memo">An internal memo, quoted whole, that the rule drops</blockquote>
            </div>
            {elsewhere}
            </body></html>"#
        )
    }

    #[test]
    fn a_sites_own_rule_says_where_its_article_is_and_who_wrote_it() {
        let paragraph =
            "<p>The committee met on Tuesday to discuss the harbour and the ferries.</p>";
        let html = ruled_page(&paragraph.repeat(6), "<div id=\"comments\"><p>First!</p></div>");
        let body =
            extract(StoryId::new(), &html, "https://www.allthingsd.com/2013/05/a-post/").unwrap();
        assert!(body.html.contains("harbour and the ferries"), "{}", body.html);
        assert!(!body.html.contains("internal memo"), "the rule strips it: {}", body.html);
        assert!(
            !body.html.contains("First!"),
            "the rule's article is only the body: {}",
            body.html
        );
        assert_eq!(body.byline.as_deref(), Some("Kara Swisher"));
    }

    #[test]
    fn a_rule_that_finds_only_a_teaser_gives_way_to_readability() {
        // The site moved its article; the rule still matches, but a line.
        let story = "<p>The committee met on Tuesday to discuss the harbour, the ferries and \
            the long-running question of who should pay for the new breakwater.</p>";
        let html = ruled_page(
            "<p>Read the story.</p>",
            &format!("<article>{}</article>", story.repeat(8)),
        );
        let body =
            extract(StoryId::new(), &html, "https://allthingsd.com/2013/05/a-post/").unwrap();
        assert!(body.html.contains("breakwater"), "{}", body.html);
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
