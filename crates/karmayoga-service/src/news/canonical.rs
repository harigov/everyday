//! What makes two addresses the same story.
//!
//! The canonical form drops `utm_*` and the other tracking parameters people
//! append (`fbclid`, `gclid`, `ref`, `mc_cid`, ...), drops the fragment,
//! lower-cases the scheme and host, drops a default port and a trailing `/`
//! on a non-root path, and sorts what query is left -- so the same article
//! linked from its blog and from Hacker News hashes to one key.

use reqwest::Url;

/// Query parameters that say where a visitor came from and nothing about
/// what they came to read. `utm_*` is matched by prefix, separately.
const TRACKING: &[&str] = &[
    "fbclid", "gclid", "dclid", "mc_cid", "mc_eid", "ref", "ref_src", "igshid", "_hsenc", "_hsmi",
    "mkt_tok", "yclid", "msclkid", "s_cid",
];

/// The canonical form of `url`, or `None` when it is not an absolute
/// `http(s)` address.
pub fn canonicalise(url: &str) -> Option<String> {
    let mut url = Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    // The parser has already lower-cased the scheme and host and dropped a
    // port that is the scheme's default.
    url.set_fragment(None);

    let mut query: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| !is_tracking(name))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    if query.is_empty() {
        url.set_query(None);
    } else {
        query.sort();
        url.query_pairs_mut().clear().extend_pairs(&query);
    }

    if url.path().len() > 1 && url.path().ends_with('/') {
        let trimmed = url.path().trim_end_matches('/').to_string();
        url.set_path(if trimmed.is_empty() { "/" } else { &trimmed });
    }
    Some(url.to_string())
}

fn is_tracking(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("utm_") || TRACKING.contains(&name.as_str())
}

/// What a story's dedupe key is a hash of: its canonical address when it has
/// one, else `guid:` and its guid, else `title:` and its trimmed,
/// lower-cased title. Never empty for an item with any of the three.
pub fn identity(url: &str, guid: &str, title: &str) -> String {
    if let Some(canonical) = canonicalise(url) {
        return canonical;
    }
    let guid = guid.trim();
    if !guid.is_empty() {
        return format!("guid:{guid}");
    }
    let title = title.trim().to_lowercase();
    if !title.is_empty() {
        return format!("title:{title}");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_made_canonical() {
        let cases = [
            // The scheme and host are lower-cased; the path is not.
            ("HTTPS://Example.COM/Posts/Hello", "https://example.com/Posts/Hello"),
            // A default port goes, any other stays.
            ("https://example.com:443/a", "https://example.com/a"),
            ("http://example.com:80/a", "http://example.com/a"),
            ("https://example.com:8443/a", "https://example.com:8443/a"),
            // The fragment goes.
            ("https://example.com/a#comments", "https://example.com/a"),
            ("https://example.com/a?x=1#top", "https://example.com/a?x=1"),
            // A trailing slash goes from a path, not from the root.
            ("https://example.com/a/", "https://example.com/a"),
            ("https://example.com/a/b/", "https://example.com/a/b"),
            ("https://example.com/", "https://example.com/"),
            ("https://example.com", "https://example.com/"),
            // Tracking parameters go, whichever case they are in.
            ("https://example.com/a?utm_source=rss&utm_medium=feed", "https://example.com/a"),
            ("https://example.com/a?UTM_Campaign=x", "https://example.com/a"),
            ("https://example.com/a?fbclid=abc", "https://example.com/a"),
            ("https://example.com/a?gclid=1&dclid=2&yclid=3&msclkid=4", "https://example.com/a"),
            ("https://example.com/a?mc_cid=1&mc_eid=2", "https://example.com/a"),
            ("https://example.com/a?ref=hn&ref_src=twsrc", "https://example.com/a"),
            ("https://example.com/a?igshid=1&_hsenc=2&_hsmi=3", "https://example.com/a"),
            ("https://example.com/a?mkt_tok=1&s_cid=2", "https://example.com/a"),
            // What is left is sorted, so the same query in another order is
            // the same address.
            ("https://example.com/a?b=2&a=1", "https://example.com/a?a=1&b=2"),
            ("https://example.com/a?id=7&utm_source=x&page=2", "https://example.com/a?id=7&page=2"),
            // Everything at once.
            (
                "HTTP://News.Example.com:80/2026/10/story/?utm_source=x&id=9&fbclid=z#c",
                "http://news.example.com/2026/10/story?id=9",
            ),
            // Whitespace around the address is not part of it.
            ("  https://example.com/a  ", "https://example.com/a"),
        ];
        for (input, want) in cases {
            assert_eq!(canonicalise(input).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn canonicalising_twice_changes_nothing() {
        for input in [
            "https://example.com/a?b=2&a=1&utm_source=x",
            "https://example.com/a b?q=a+b&r=%C3%A9",
            "https://example.com/a/?x=",
            "https://Bücher.example/Straße?q=ü",
        ] {
            let once = canonicalise(input).expect(input);
            assert_eq!(canonicalise(&once).as_deref(), Some(once.as_str()), "{input}");
        }
    }

    #[test]
    fn only_absolute_web_addresses_have_a_canonical_form() {
        for input in [
            "",
            "   ",
            "/relative/path",
            "relative/path",
            "//example.com/protocol-relative",
            "mailto:someone@example.com",
            "javascript:alert(1)",
            "ftp://example.com/a",
            "file:///etc/passwd",
            "data:text/plain,hello",
            "https://",
            "not a url",
        ] {
            assert_eq!(canonicalise(input), None, "{input:?}");
        }
    }

    #[test]
    fn two_routes_to_one_article_have_one_identity() {
        let from_the_blog = identity("https://blog.example/post?utm_source=rss", "guid-1", "Hello");
        let from_hacker_news = identity("https://BLOG.example/post/#comments", "hn:41", "Hello!");
        assert_eq!(from_the_blog, "https://blog.example/post");
        assert_eq!(from_the_blog, from_hacker_news);
    }

    #[test]
    fn an_item_without_an_address_is_known_by_its_guid_then_its_title() {
        let cases = [
            // No address: the guid, trimmed.
            ("", "  urn:uuid:1234 ", "A title", "guid:urn:uuid:1234"),
            // An address that is not a web address does not count.
            ("/relative", "g-1", "A title", "guid:g-1"),
            ("mailto:a@b.example", "g-2", "A title", "guid:g-2"),
            // No guid either: the title, trimmed and lower-cased.
            ("", "", "  Hello, World  ", "title:hello, world"),
            ("", "   ", "Hello, World", "title:hello, world"),
            // Nothing at all.
            ("", "", "", ""),
            ("", "  ", "  ", ""),
        ];
        for (url, guid, title, want) in cases {
            assert_eq!(identity(url, guid, title), want, "{url:?} {guid:?} {title:?}");
        }
    }
}
