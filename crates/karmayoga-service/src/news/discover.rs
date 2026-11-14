//! Finding a feed from an address somebody pasted. Pure: given what came
//! back from the address, say where the feed is.
//!
//! In order: if the bytes are a feed, that is the feed (the caller tries
//! [`super::parse::parse_feed`] first); if they are HTML, every
//! `<link rel="alternate">` whose type is RSS, Atom or JSON Feed; otherwise
//! [`fallback_paths`] on the same origin.

use super::FoundFeed;
use dom_query::Document;
use reqwest::Url;

/// What a site's feed is most often called, tried in this order when its
/// page advertises none.
const FALLBACKS: &[&str] = &["/feed", "/rss", "/atom.xml", "/index.xml", "/feed.xml"];

/// Every feed an HTML page advertises, absolute, in document order, without
/// duplicates.
pub fn feed_links(html: &str, page_url: &str) -> Vec<FoundFeed> {
    let Ok(page) = Url::parse(page_url.trim()) else { return Vec::new() };
    let doc = Document::from(html);
    // A `<base href>` moves what the page's relative addresses are relative
    // to, exactly as it does for a browser.
    let base = doc.base_uri().and_then(|href| page.join(&href).ok()).unwrap_or(page);

    let links = doc.select("link");
    let mut found: Vec<FoundFeed> = Vec::new();
    for link in links.nodes() {
        let (Some(rel), Some(kind), Some(href)) =
            (link.attr("rel"), link.attr("type"), link.attr("href"))
        else {
            continue;
        };
        // `rel` is a list of words, and `type` can carry parameters.
        if !rel.split_ascii_whitespace().any(|word| word.eq_ignore_ascii_case("alternate")) {
            continue;
        }
        if !is_feed_type(kind.split(';').next().unwrap_or_default().trim()) {
            continue;
        }
        let Ok(url) = base.join(href.trim()) else { continue };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        let url = url.to_string();
        if found.iter().any(|feed| feed.url == url) {
            continue;
        }
        let title = link.attr("title").unwrap_or_default();
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        found.push(FoundFeed { url, title });
    }
    found
}

/// Is this the media type of a feed? `application/json` is on the list
/// because JSON Feed's first version used it, and is safe to take because
/// the caller only asks about links that say they are an alternate.
fn is_feed_type(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "application/rss+xml"
            | "application/atom+xml"
            | "application/feed+json"
            | "application/json"
            | "application/rdf+xml"
    )
}

/// The addresses worth trying when a page advertises nothing:
/// `/feed`, `/rss`, `/atom.xml`, `/index.xml`, `/feed.xml` on its origin.
pub fn fallback_paths(page_url: &str) -> Vec<String> {
    let Ok(page) = Url::parse(page_url.trim()) else { return Vec::new() };
    if !matches!(page.scheme(), "http" | "https") || page.host_str().is_none() {
        return Vec::new();
    }
    let origin = page.origin().ascii_serialization();
    FALLBACKS.iter().map(|path| format!("{origin}{path}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(feeds: &[FoundFeed]) -> Vec<(&str, &str)> {
        feeds.iter().map(|f| (f.url.as_str(), f.title.as_str())).collect()
    }

    /// The `<head>` of a WordPress site, which advertises more than one feed
    /// and a good deal that is not.
    const WORDPRESS: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>A Blog</title>
<link rel="stylesheet" href="/style.css" type="text/css">
<link rel="canonical" href="https://blog.example/">
<link rel="icon" href="/favicon.ico">
<link rel="alternate" type="application/rss+xml" title="A Blog &raquo; Feed"
  href="https://blog.example/feed/">
<link rel="alternate" type="application/rss+xml" title="A Blog &raquo; Comments Feed"
  href="https://blog.example/comments/feed/">
<link rel="alternate" type="application/json+oembed" href="https://blog.example/oembed?url=x">
<link rel="alternate" type="text/html" hreflang="fr" href="https://blog.example/fr/">
</head>
<body><p>Hello</p></body>
</html>"#;

    #[test]
    fn the_feeds_a_page_advertises_come_back_in_document_order_with_their_titles() {
        let feeds = feed_links(WORDPRESS, "https://blog.example/");
        assert_eq!(
            found(&feeds),
            [
                ("https://blog.example/feed/", "A Blog \u{bb} Feed"),
                ("https://blog.example/comments/feed/", "A Blog \u{bb} Comments Feed"),
            ]
        );
    }

    #[test]
    fn every_feed_type_counts_and_other_types_and_relations_do_not() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="/a">
            <link rel="alternate" type="application/atom+xml" href="/b">
            <link rel="alternate" type="application/feed+json" href="/c">
            <link rel="alternate" type="application/json" href="/d">
            <link rel="alternate" type="application/rdf+xml" href="/e">
            <link rel="alternate" type="text/xml" href="/not-text-xml">
            <link rel="alternate" type="text/html" href="/not-html">
            <link rel="alternate" href="/no-type">
            <link rel="self" type="application/atom+xml" href="/not-self">
            <link rel="stylesheet" type="application/rss+xml" href="/not-a-stylesheet">
            <link type="application/rss+xml" href="/no-rel">
            <link rel="alternate" type="application/rss+xml">
            </head>"#;
        let urls: Vec<String> =
            feed_links(html, "https://x.example/").into_iter().map(|f| f.url).collect();
        assert_eq!(
            urls,
            [
                "https://x.example/a",
                "https://x.example/b",
                "https://x.example/c",
                "https://x.example/d",
                "https://x.example/e",
            ]
        );
    }

    #[test]
    fn rel_is_a_list_of_words_and_type_may_have_parameters_and_any_case() {
        let html = r#"<head>
            <link rel="ALTERNATE" type="Application/RSS+XML" href="/a">
            <link rel="home alternate" type="application/atom+xml; charset=utf-8" href="/b">
            <link rel="alternate  " type=" application/rss+xml " href="/c">
            </head>"#;
        let urls: Vec<String> =
            feed_links(html, "https://x.example/").into_iter().map(|f| f.url).collect();
        assert_eq!(urls, ["https://x.example/a", "https://x.example/b", "https://x.example/c"]);
    }

    #[test]
    fn relative_addresses_resolve_against_the_page() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="/root.xml">
            <link rel="alternate" type="application/rss+xml" href="sibling.xml">
            <link rel="alternate" type="application/rss+xml" href="../up.xml">
            <link rel="alternate" type="application/rss+xml" href="//cdn.example/proto.xml">
            <link rel="alternate" type="application/rss+xml" href="https://other.example/abs.xml">
            </head>"#;
        let urls: Vec<String> = feed_links(html, "https://x.example/blog/2026/post.html?q=1#c")
            .into_iter()
            .map(|f| f.url)
            .collect();
        assert_eq!(
            urls,
            [
                "https://x.example/root.xml",
                "https://x.example/blog/2026/sibling.xml",
                "https://x.example/blog/up.xml",
                "https://cdn.example/proto.xml",
                "https://other.example/abs.xml",
            ]
        );
    }

    #[test]
    fn a_base_element_moves_what_relative_addresses_are_relative_to() {
        let html = r#"<html><head><base href="https://static.example/site/">
            <link rel="alternate" type="application/rss+xml" href="rss">
            </head><body></body></html>"#;
        let feeds = feed_links(html, "https://x.example/blog/");
        assert_eq!(found(&feeds), [("https://static.example/site/rss", "")]);
    }

    #[test]
    fn the_same_feed_twice_is_listed_once_by_its_first_title() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" title="  Main
                feed " href="/feed">
            <link rel="alternate" type="application/atom+xml" title="Atom" href="/atom">
            <link rel="alternate" type="application/rss+xml" title="Main again"
                  href="https://x.example/feed">
            </head>"#;
        let feeds = feed_links(html, "https://x.example/");
        assert_eq!(
            found(&feeds),
            [("https://x.example/feed", "Main feed"), ("https://x.example/atom", "Atom")]
        );
    }

    #[test]
    fn an_address_that_is_not_a_web_address_is_not_a_feed() {
        let html = r#"<head>
            <link rel="alternate" type="application/rss+xml" href="feed://x.example/rss">
            <link rel="alternate" type="application/rss+xml" href="javascript:void(0)">
            <link rel="alternate" type="application/rss+xml" href="mailto:me@x.example">
            <link rel="alternate" type="application/rss+xml" href="http://[bad">
            </head>"#;
        assert!(feed_links(html, "https://x.example/").is_empty());
    }

    #[test]
    fn a_page_with_no_links_or_no_address_has_no_feeds() {
        assert!(feed_links("", "https://x.example/").is_empty());
        assert!(feed_links("<p>Plain page</p>", "https://x.example/").is_empty());
        let html = r#"<link rel="alternate" type="application/rss+xml" href="/feed">"#;
        assert!(feed_links(html, "").is_empty(), "nothing to resolve against");
        assert!(feed_links(html, "not a url").is_empty());
    }

    #[test]
    fn a_link_in_the_body_still_counts() {
        let html = r#"<html><body><p>Subscribe</p>
            <link rel="alternate" type="application/rss+xml" href="/feed"></body></html>"#;
        assert_eq!(feed_links(html, "https://x.example/").len(), 1);
    }

    #[test]
    fn the_fallbacks_are_the_usual_names_on_the_pages_origin() {
        let want = [
            "https://example.com/feed",
            "https://example.com/rss",
            "https://example.com/atom.xml",
            "https://example.com/index.xml",
            "https://example.com/feed.xml",
        ];
        // Only the origin matters: not the path, query or fragment.
        assert_eq!(fallback_paths("https://example.com"), want);
        assert_eq!(fallback_paths("https://Example.com/blog/post?x=1#top"), want);
        assert_eq!(fallback_paths("  https://example.com:443/a "), want);
    }

    #[test]
    fn a_fallback_keeps_the_scheme_and_a_port_that_is_not_the_default() {
        assert_eq!(fallback_paths("http://example.com/a")[0], "http://example.com/feed");
        let with_port = fallback_paths("https://example.com:8443/a");
        assert_eq!(with_port[4], "https://example.com:8443/feed.xml");
    }

    #[test]
    fn nothing_is_guessed_for_an_address_that_is_not_a_web_page() {
        for page in ["", "not a url", "/relative", "mailto:me@example.com", "file:///etc/hosts"] {
            assert!(fallback_paths(page).is_empty(), "{page:?}");
        }
    }
}
