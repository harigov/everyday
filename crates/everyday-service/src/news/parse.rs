//! RSS 0.9x/1.0/2.0, Atom and JSON Feed, through `feed-rs`, into
//! [`Incoming`] items. Pure: bytes in, items out.
//!
//! What this owns: picking the best of what each format offers (an Atom
//! entry's `content` over its `summary`, an RSS item's `content:encoded` over
//! its `description`), resolving relative addresses against `xml:base`, the
//! item link or the feed's own address, and turning `feed-rs`'s `chrono`
//! dates into `jiff` at this one boundary.
//!
//! `summary` and `content` come out as HTML either way, so nothing after
//! this has to ask which it was: plain text is escaped here, and where an
//! item has a `content` and no `summary` the summary is the content again,
//! for the refresher to cut down to a plain line. Titles and author names
//! come out as plain text: publishers put entities and even tags in them,
//! and a row in a list is no place to draw either.

use super::{Incoming, ParsedFeed, canonical};
use everyday_core::news::BodyOrigin;
use feed_rs::model::{Content, Entry, Feed, FeedType, Link, Text};
use feed_rs::parser::{self, ParseErrorKind, ParseFeedError};
use jiff::Timestamp;
use reqwest::Url;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The most items kept from one feed. A feed that lists more than this
/// (a full archive, say) is cut to its newest: the rest were published long
/// before anybody followed it, and the store would only expire them.
const MAX_ITEMS: usize = 200;

const NOT_A_FEED: &str = "This is not a feed.";

/// Parse a feed fetched from `feed_url`. `Err` carries a sentence fit to
/// show a person ("this is not a feed", "the feed is malformed at ...").
pub fn parse_feed(bytes: &[u8], feed_url: &str) -> Result<ParsedFeed, String> {
    // The address the feed came from is what its relative links are
    // relative to, unless it says otherwise with `xml:base`. `feed-rs` fails
    // outright on a base it cannot read, so one that does not parse is left
    // out rather than passed on.
    let base = Url::parse(feed_url.trim()).ok();

    // Behind `catch_unwind`, as `everyday_mail::text::html_to_text` is: the
    // bytes are a stranger's, a panic here would take the refresher down
    // with it, and a feed that panics the parser is no more use than one
    // that does not parse.
    //
    // The ids `feed-rs` invents for an item without a guid are left empty
    // rather than let be a hash of the link and title (or, with neither, a
    // fresh UUID that would make the item new again at every refresh):
    // `canonical::identity` has its own, stable, order of things to fall
    // back to.
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        parser::Builder::new()
            .base_uri(base.as_ref().map(Url::as_str))
            .id_generator(|_, _, _| String::new())
            .build()
            .parse(bytes)
    }));
    let feed = match parsed {
        Ok(Ok(feed)) => feed,
        Ok(Err(error)) => return Err(explain(&error)),
        Err(_) => return Err("The feed could not be read.".into()),
    };
    Ok(convert(feed, base.as_ref(), feed_url))
}

/// The sentence for a parse that failed.
fn explain(error: &ParseFeedError) -> String {
    match error {
        ParseFeedError::ParseError(ParseErrorKind::NoFeedRoot) => NOT_A_FEED.into(),
        ParseFeedError::JsonUnsupportedVersion(_) => {
            "This JSON Feed is a version that is not supported.".into()
        }
        // Well-formed JSON that is not shaped like a feed at all.
        ParseFeedError::JsonSerde(e) if e.is_data() => NOT_A_FEED.into(),
        ParseFeedError::JsonSerde(e) => format!("The feed is malformed: {e}."),
        ParseFeedError::XmlReader(e) => format!("The feed is malformed: {e}."),
        ParseFeedError::ParseError(e) => format!("The feed is malformed: {e}."),
        other => format!("The feed could not be read: {other}."),
    }
}

fn convert(feed: Feed, base: Option<&Url>, feed_url: &str) -> ParsedFeed {
    // Atom and JSON Feed say what kind of text a field holds, so their
    // plain text is plain. RSS says nothing, and what `feed-rs` calls plain
    // in an RSS feed is in practice HTML with its angle brackets escaped.
    let plain_is_text = matches!(feed.feed_type, FeedType::Atom | FeedType::JSON);

    let title = feed.title.as_ref().map(|t| title_text(t, plain_is_text)).unwrap_or_default();
    let site_url = page_link(&feed.links).map(|l| absolute(base, &l.href)).unwrap_or_default();
    let ttl_minutes = feed.ttl.filter(|&minutes| minutes > 0);

    // Two entries that are one story -- the same guid republished with a
    // corrected title, say -- are one item here, the first listed (feeds list
    // the newest first) winning, because the store would keep only the first
    // inserted anyway.
    let mut items: Vec<(String, Incoming)> = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let guid = entry.id.trim().to_string();
            item(entry, base, feed_url, plain_is_text).map(|incoming| (guid, incoming))
        })
        .collect();
    distinguish_shared_links(&mut items);
    let mut seen = HashSet::new();
    let mut items: Vec<Incoming> = items
        .into_iter()
        .map(|(_, incoming)| incoming)
        .filter(|incoming| seen.insert(incoming.identity.clone()))
        .collect();
    keep_newest(&mut items);

    ParsedFeed { title, site_url, items, ttl_minutes }
}

/// An address several items of one feed share is not any one item's
/// identity.
///
/// A changelog, a status page or a podcast's "episodes" page links every item
/// to the same place, and keyed by that address alone they would collapse
/// into whichever came first -- every later entry lost as a duplicate. When
/// two or more items share an address and carry guids of their own, the guid
/// is what tells them apart, and is what they are keyed by. The cost is that
/// such an item is not folded into the same story arriving from another
/// source; but the address it shares never named one story to begin with.
fn distinguish_shared_links(items: &mut [(String, Incoming)]) {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, incoming) in items.iter() {
        *counts.entry(incoming.identity.clone()).or_default() += 1;
    }
    for (guid, incoming) in items.iter_mut() {
        let shared = counts.get(&incoming.identity).is_some_and(|&n| n > 1);
        if shared && !guid.is_empty() && incoming.identity == incoming_url_identity(incoming) {
            incoming.identity = canonical::identity("", guid, &incoming.title);
        }
    }
}

/// The identity an item would have from its address alone, or empty when its
/// address is not one -- so [`distinguish_shared_links`] only rekeys items
/// whose identity *is* their address.
fn incoming_url_identity(incoming: &Incoming) -> String {
    canonical::canonicalise(&incoming.url).unwrap_or_default()
}

fn item(entry: Entry, base: Option<&Url>, feed_url: &str, plain_is_text: bool) -> Option<Incoming> {
    let url = item_link(&entry.links).map(|l| absolute(base, &l.href)).unwrap_or_default();
    // What a relative address inside the item resolves against: the item
    // itself, where a reader following the link would be standing; failing
    // that, the feed.
    let item_base = Url::parse(&url).ok().or_else(|| base.cloned());

    let title = entry.title.as_ref().map(|t| title_text(t, plain_is_text)).unwrap_or_default();
    let content = entry.content.as_ref().and_then(|c| content_html(c, plain_is_text));
    let mut summary =
        entry.summary.as_ref().map(|s| text_html(s, plain_is_text)).unwrap_or_default();
    if summary.is_empty() {
        summary = content.clone().unwrap_or_default();
    }
    // A video or a podcast episode says what it is in its media
    // description -- a YouTube channel's feed has nothing else -- so that is
    // the summary when there is nothing more ordinary to use.
    if summary.is_empty()
        && let Some(description) = entry.media.iter().find_map(|m| m.description.as_ref())
    {
        summary = text_html(description, plain_is_text);
    }
    if title.is_empty() && url.is_empty() && summary.is_empty() {
        return None;
    }

    let mut identity = canonical::identity(&url, &entry.id, &title);
    if identity.is_empty() {
        // No address, no guid and no title: all that tells this item from
        // the next is what it says.
        identity = format!("text:{}", blake3::hash(summary.as_bytes()).to_hex());
    }

    Some(Incoming {
        identity,
        title,
        author: author(&entry),
        published: entry.published.or(entry.updated).and_then(to_jiff),
        summary,
        content,
        base_url: if url.is_empty() { feed_url.to_string() } else { url.clone() },
        image_url: lead_image(&entry, item_base.as_ref()),
        url,
        origin: BodyOrigin::Feed,
        ..Incoming::default()
    })
}

/// The page a feed or an item is about: the `alternate` link (which is what
/// Atom means by a link with no `rel`, and says so), else the first link
/// that is neither a relation to something else -- `self`, `replies`, a
/// comments feed -- nor a file attached to the item.
fn page_link(links: &[Link]) -> Option<&Link> {
    with_address(links)
        .find(|l| l.rel.as_deref().is_some_and(|rel| rel.eq_ignore_ascii_case("alternate")))
        .or_else(|| {
            with_address(links).find(|l| l.rel.is_none() && l.target.is_none() && !is_attachment(l))
        })
}

/// An item's own link: its page, else whatever link it has.
fn item_link(links: &[Link]) -> Option<&Link> {
    page_link(links).or_else(|| with_address(links).next())
}

/// The links that point somewhere.
fn with_address(links: &[Link]) -> impl Iterator<Item = &Link> {
    links.iter().filter(|l| !l.href.trim().is_empty())
}

fn is_attachment(link: &Link) -> bool {
    link.media_type.as_deref().map(essence).is_some_and(|mime| {
        mime.starts_with("image/") || mime.starts_with("audio/") || mime.starts_with("video/")
    })
}

/// A media type without its parameters: `text/html`, not
/// `text/html; charset=utf-8`.
fn essence(mime: &str) -> &str {
    mime.split(';').next().unwrap_or(mime).trim()
}

fn is_image(mime: &str) -> bool {
    essence(mime).starts_with("image/")
}

/// The first picture the item names, as an absolute address: a
/// `media:thumbnail`, else `media:content` or an enclosure that is an image.
fn lead_image(entry: &Entry, base: Option<&Url>) -> String {
    let thumbnails =
        entry.media.iter().flat_map(|m| m.thumbnails.iter()).map(|t| t.image.uri.as_str());
    let contents = entry
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .filter(|c| c.content_type.as_ref().is_some_and(|mime| is_image(mime.as_str())))
        .filter_map(|c| c.url.as_ref().map(|url| url.as_str()));
    // A JSON Feed attachment and an Atom `rel="enclosure"` are links.
    let attachments = entry
        .links
        .iter()
        .filter(|l| l.media_type.as_deref().is_some_and(is_image))
        .map(|l| l.href.as_str());
    thumbnails
        .chain(contents)
        .chain(attachments)
        .map(|href| absolute(base, href))
        .find(|url| !url.is_empty())
        .unwrap_or_default()
}

/// The first author who has a name, as plain text.
fn author(entry: &Entry) -> String {
    entry
        .authors
        .iter()
        .filter_map(|person| person.name.as_deref())
        .map(plain)
        .map(|name| {
            // RSS 2.0's `<author>` is `address (Name)`, and `feed-rs` keeps
            // the brackets with the name.
            let inside = name.strip_prefix('(').and_then(|n| n.strip_suffix(')'));
            inside.map_or_else(|| name.clone(), |inner| inner.trim().to_string())
        })
        .find(|name| !name.is_empty())
        .unwrap_or_default()
}

/// A title, as the one line of plain text it is drawn as.
fn title_text(text: &Text, plain_is_text: bool) -> String {
    if plain_is_text && essence(text.content_type.as_str()) == "text/plain" {
        collapse(&text.content)
    } else {
        plain(&text.content)
    }
}

/// A summary, as HTML.
fn text_html(text: &Text, plain_is_text: bool) -> String {
    let body = text.content.trim();
    if plain_is_text && essence(text.content_type.as_str()) == "text/plain" {
        plain_to_html(body)
    } else {
        body.to_string()
    }
}

/// An entry's content, as HTML. `None` for content that is not something a
/// person reads (a base64 attachment, inline XML) or that is empty or only
/// a link to somewhere else.
fn content_html(content: &Content, plain_is_text: bool) -> Option<String> {
    let body = content.body.as_deref()?.trim();
    if body.is_empty() {
        return None;
    }
    match essence(content.content_type.as_str()) {
        "text/plain" if plain_is_text => Some(plain_to_html(body)),
        "text/plain" | "text/html" | "application/xhtml+xml" => Some(body.to_string()),
        _ => None,
    }
}

/// Plain text, as the HTML that reads the same: escaped, with a blank line
/// between paragraphs and a line break for any other newline.
fn plain_to_html(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| format!("<p>{}</p>", escape(paragraph).replace('\n', "<br>")))
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// What `text` reads as once any markup is gone and any entity is decoded,
/// on one line. The HTML reader is the browser's own algorithm, so
/// `&amp;amp;`, `&nbsp;` and `<b>` in a title come out the way they would in
/// a page, and a script's text is not mistaken for the title's.
fn plain(text: &str) -> String {
    let text = text.trim();
    if !text.contains(['<', '&']) {
        return collapse(text);
    }
    let doc = dom_query::Document::fragment(text);
    doc.select("script, style").remove();
    collapse(&doc.formatted_text())
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `href` as an absolute web address: resolved against `base` when there is
/// one, and empty when it is not a web address at all (`javascript:`,
/// `mailto:`, a relative path with nothing to be relative to).
fn absolute(base: Option<&Url>, href: &str) -> String {
    let href = href.trim();
    let resolved = match base {
        Some(base) => base.join(href),
        None => Url::parse(href),
    };
    match resolved {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.into(),
        _ => String::new(),
    }
}

/// `chrono` to `jiff`, at the one place the two meet in this module.
fn to_jiff(at: chrono::DateTime<chrono::Utc>) -> Option<Timestamp> {
    Timestamp::new(at.timestamp(), at.timestamp_subsec_nanos() as i32).ok()
}

/// Cut `items` to the [`MAX_ITEMS`] newest, leaving them in the order the
/// feed listed them. An item with no date counts as the newest: its date
/// will be the moment it was first seen.
fn keep_newest(items: &mut Vec<Incoming>) {
    if items.len() <= MAX_ITEMS {
        return;
    }
    let mut newest_first: Vec<usize> = (0..items.len()).collect();
    newest_first
        .sort_by_key(|&at| std::cmp::Reverse(items[at].published.unwrap_or(Timestamp::MAX)));
    let keep: HashSet<usize> = newest_first.into_iter().take(MAX_ITEMS).collect();
    let mut at = 0;
    items.retain(|_| {
        at += 1;
        keep.contains(&(at - 1))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    const RSS2: &str = include_str!("../../tests/fixtures/news/rss2.xml");
    const RSS1: &str = include_str!("../../tests/fixtures/news/rss1.rdf");
    const ATOM: &str = include_str!("../../tests/fixtures/news/atom-base.xml");
    const JSON: &str = include_str!("../../tests/fixtures/news/jsonfeed.json");
    const UNDATED: &str = include_str!("../../tests/fixtures/news/undated-and-duplicates.xml");
    const MALFORMED: &str = include_str!("../../tests/fixtures/news/malformed.xml");
    const ENTITIES: &str = include_str!("../../tests/fixtures/news/entities.xml");

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn parse(xml: &str, feed_url: &str) -> ParsedFeed {
        parse_feed(xml.as_bytes(), feed_url).expect("a feed")
    }

    #[test]
    fn an_rss_item_is_its_content_not_its_description() {
        let feed = parse(RSS2, "https://quiet.example/feed.xml");
        assert_eq!(feed.title, "The Quiet Compiler");
        assert_eq!(feed.site_url, "https://quiet.example/");
        assert_eq!(feed.ttl_minutes, Some(90));
        assert_eq!(feed.items.len(), 3);

        let first = &feed.items[0];
        assert_eq!(first.title, "Why small programs win");
        // The address is the publisher's, tracking and all; only the
        // identity is canonical.
        assert_eq!(
            first.url,
            "https://quiet.example/posts/small-programs/?utm_source=rss&utm_medium=feed"
        );
        assert_eq!(first.identity, "https://quiet.example/posts/small-programs");
        assert_eq!(first.base_url, first.url);
        assert_eq!(first.author, "Priya Raman");
        assert_eq!(first.published, Some(ts("2026-10-08T09:30:00Z")));
        assert_eq!(first.summary, "<p>A short teaser.</p>");
        assert_eq!(
            first.content.as_deref(),
            Some(
                "<p>The <em>full</em> article, with a <a href=\"/posts/other\">relative link</a>.\
                 </p><img src=\"/img/small.png\" alt=\"\">"
            )
        );
        assert_eq!(first.image_url, "https://quiet.example/img/small-thumb.png");
        assert_eq!(first.origin, BodyOrigin::Feed);
        assert_eq!((first.score, first.comments, first.discussion_url.as_str()), (None, None, ""));
    }

    #[test]
    fn an_rss_item_with_only_a_description_has_no_content() {
        let feed = parse(RSS2, "https://quiet.example/feed.xml");
        let second = &feed.items[1];
        assert_eq!(second.title, "Notes from the week");
        assert_eq!(second.url, "https://quiet.example/posts/notes-week-41");
        assert_eq!(second.identity, second.url);
        assert_eq!(second.summary, "Seven links and a half-finished thought.");
        assert_eq!(second.content, None);
        assert_eq!(second.published, Some(ts("2026-10-05T07:00:00Z")));
        // `address (Name)` is a name.
        assert_eq!(second.author, "Quentin Hale");
        // A relative image enclosure is resolved against the feed.
        assert_eq!(second.image_url, "https://quiet.example/img/week41.jpg");
    }

    #[test]
    fn an_audio_enclosure_is_not_a_picture() {
        let feed = parse(RSS2, "https://quiet.example/feed.xml");
        assert_eq!(feed.items[2].image_url, "");
        assert_eq!(feed.items[2].author, "");
    }

    #[test]
    fn rss_one_point_zero_reads_its_rdf_items() {
        let feed = parse(RSS1, "https://lab.example/index.rdf");
        assert_eq!(feed.title, "Lab Notebook");
        assert_eq!(feed.site_url, "https://lab.example/");
        assert_eq!(feed.ttl_minutes, None);
        assert_eq!(feed.items.len(), 2);

        let first = &feed.items[0];
        assert_eq!(first.title, "Titration, redone");
        assert_eq!(first.url, "https://lab.example/2026/10/titration");
        assert_eq!(first.author, "M. Okafor");
        // Written with an offset; kept as the instant it names.
        assert_eq!(first.published, Some(ts("2026-10-09T12:00:00Z")));
        assert_eq!(first.summary, "Second attempt, with fresh buffer.");
        assert_eq!(
            first.content.as_deref(),
            Some("<p>Second attempt, with <b>fresh</b> buffer.</p>")
        );

        // A date with no time is midnight.
        assert_eq!(feed.items[1].published, Some(ts("2026-09-21T00:00:00Z")));
        assert_eq!(feed.items[1].content, None);
    }

    #[test]
    fn atom_resolves_relative_links_against_xml_base() {
        let feed = parse(ATOM, "https://blog.example/journal/feed.atom");
        assert_eq!(feed.title, "Field Journal");
        assert_eq!(feed.site_url, "https://blog.example/journal/");
        assert_eq!(feed.items.len(), 2);

        let first = &feed.items[0];
        assert_eq!(first.title, "Frost on the hedgerow");
        // The `alternate` link, not the `replies` one that follows it.
        assert_eq!(first.url, "https://blog.example/journal/2026/10/frost");
        assert_eq!(first.identity, first.url);
        assert_eq!(first.base_url, first.url);
        assert_eq!(first.author, "Dana Whitlock");
        assert_eq!(first.published, Some(ts("2026-10-10T07:15:00Z")));
        // Relative addresses inside the content are left for the sanitiser
        // to resolve against `base_url`.
        assert_eq!(
            first.content.as_deref(),
            Some(
                "<p>Frost on the <a href=\"../hedges\">hedgerow</a>.</p>\
                 <img src=\"frost.jpg\" alt=\"frost\">"
            )
        );
        // Plain text in Atom is plain text, so it is escaped on its way to
        // being HTML.
        assert_eq!(first.summary, "<p>First frost of the year, &amp; a robin.</p>");
        assert_eq!(first.image_url, "https://blog.example/journal/img/frost-thumb.jpg");

        // An `xml:base` on an entry applies to the entry; with no
        // `published`, the date is `updated`.
        let second = &feed.items[1];
        assert_eq!(second.url, "https://blog.example/elsewhere/hedges");
        assert_eq!(second.published, Some(ts("2026-10-03T12:00:00Z")));
        assert_eq!(second.author, "");
        assert_eq!(second.content, None);
        assert_eq!(second.summary, "<p>Notes on laying a hedge.</p>");
    }

    #[test]
    fn json_feed_reads_its_html_and_its_text_content() {
        let feed = parse(JSON, "https://weeknotes.example/feed.json");
        assert_eq!(feed.title, "Weeknotes");
        assert_eq!(feed.site_url, "https://weeknotes.example/");
        assert_eq!(feed.items.len(), 2);

        let first = &feed.items[0];
        assert_eq!(first.url, "https://weeknotes.example/2026/41/");
        assert_eq!(first.identity, "https://weeknotes.example/2026/41");
        assert_eq!(first.author, "Jo Park");
        assert_eq!(first.published, Some(ts("2026-10-09T17:00:00Z")));
        assert_eq!(first.content.as_deref(), Some("<p>Shipped the <strong>importer</strong>.</p>"));
        assert_eq!(first.summary, "<p>Shipped the importer.</p>");
        // An image attachment is the lead picture.
        assert_eq!(first.image_url, "https://weeknotes.example/img/41.png");

        // A relative `url` is resolved against the feed, an item with no
        // author of its own takes the feed's, and text content becomes
        // paragraphs. With no summary of its own, the summary is the content.
        let second = &feed.items[1];
        assert_eq!(second.url, "https://weeknotes.example/2026/40/");
        assert_eq!(second.author, "Sam Ito");
        assert_eq!(second.content.as_deref(), Some("<p>Rested.</p><p>Read a lot.</p>"));
        assert_eq!(second.summary, "<p>Rested.</p><p>Read a lot.</p>");
        assert_eq!(second.published, Some(ts("2026-10-02T17:00:00Z")));
        assert_eq!(second.image_url, "");
    }

    #[test]
    fn an_item_with_no_date_has_none_and_an_item_with_no_address_has_the_feeds() {
        let feed = parse(UNDATED, "https://changes.example/rss.xml");
        let note = feed.items.last().unwrap();
        assert_eq!(note.title, "A note with no address");
        assert_eq!(note.published, None);
        assert_eq!(note.url, "");
        // Its relative addresses, if it had any, would be the feed's.
        assert_eq!(note.base_url, "https://changes.example/rss.xml");
        assert_eq!(note.identity, "guid:note-17");
    }

    #[test]
    fn two_items_sharing_a_guid_and_an_address_are_one_item() {
        let feed = parse(UNDATED, "https://changes.example/rss.xml");
        let titles: Vec<&str> = feed.items.iter().map(|i| i.title.as_str()).collect();
        // The first listed wins.
        assert_eq!(
            titles,
            ["1.4.0 released (corrected)", "1.3.0 released", "A note with no address"]
        );
        assert_eq!(feed.items[0].published, Some(ts("2026-10-07T10:00:00Z")));
    }

    #[test]
    fn a_video_with_only_a_media_description_is_summarised_by_it() {
        // The shape of a YouTube channel's feed: everything is in media:group.
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"
            xmlns:media="http://search.yahoo.com/mrss/">
            <title>Channel</title>
            <entry>
              <id>yt:video:abc</id>
              <title>A video</title>
              <link rel="alternate" href="https://www.youtube.com/watch?v=abc"/>
              <published>2026-10-01T12:00:00+00:00</published>
              <media:group>
                <media:title>A video</media:title>
                <media:content url="https://www.youtube.com/v/abc"
                    type="application/x-shockwave-flash" width="640" height="390"/>
                <media:thumbnail url="https://i.ytimg.com/vi/abc/hq.jpg" width="480" height="360"/>
                <media:description>What the video is about.</media:description>
              </media:group>
            </entry></feed>"#;
        let feed = parse(xml, "https://www.youtube.com/feeds/videos.xml?channel_id=x");
        assert!(feed.items[0].summary.contains("What the video is about."));
        assert_eq!(feed.items[0].image_url, "https://i.ytimg.com/vi/abc/hq.jpg");
    }

    #[test]
    fn items_sharing_an_address_but_not_a_guid_are_kept_apart_by_their_guids() {
        let xml = r#"<rss version="2.0"><channel><title>T</title>
            <item><title>1.2</title><link>https://t.example/changes</link><guid>v1.2</guid></item>
            <item><title>1.1</title><link>https://t.example/changes</link><guid>v1.1</guid></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items.len(), 2);
        assert_eq!(feed.items[0].identity, "guid:v1.2");
        assert_eq!(feed.items[1].identity, "guid:v1.1");
    }

    #[test]
    fn two_items_sharing_a_guid_but_not_an_address_are_two() {
        let xml = r#"<rss version="2.0"><channel><title>T</title>
            <item><title>One</title><link>https://t.example/1</link><guid>same</guid></item>
            <item><title>Two</title><link>https://t.example/2</link><guid>same</guid></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items.len(), 2);
        assert_ne!(feed.items[0].identity, feed.items[1].identity);
    }

    #[test]
    fn entities_and_tags_in_a_title_are_decoded_and_stripped() {
        let feed = parse(ENTITIES, "https://qa.example/feed");
        assert_eq!(feed.title, "Q&A & Friends");
        let titles: Vec<&str> = feed.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                // Escaped twice, a numeric reference, a named one the XML
                // parser does not know, and tags written as text.
                "Tom & Jerry\u{2019}s big day out \u{2013} part 2",
                "Markup in a title",
                "A title over two lines",
            ]
        );
    }

    #[test]
    fn plain_text_titles_in_atom_are_left_as_they_were_written() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>F</title>
            <entry><title type="text">Vec&lt;T&gt; explained</title><id>a</id>
              <link href="https://f.example/a"/></entry>
            <entry><title type="html">Hedges &amp;amp; &lt;b&gt;ditches&lt;/b&gt;</title><id>b</id>
              <link href="https://f.example/b"/></entry>
            </feed>"#;
        let feed = parse(xml, "https://f.example/feed");
        assert_eq!(feed.items[0].title, "Vec<T> explained");
        assert_eq!(feed.items[1].title, "Hedges & ditches");
    }

    #[test]
    fn relative_addresses_resolve_against_the_feed_when_nothing_else_says() {
        let xml = r#"<rss version="2.0"><channel><title>T</title><link>/</link>
            <item><title>Rel</title><link>/posts/1</link>
              <enclosure url="/img/a.png" type="image/png" length="1"/></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://rel.example/blog/feed.xml");
        assert_eq!(feed.site_url, "https://rel.example/");
        assert_eq!(feed.items[0].url, "https://rel.example/posts/1");
        assert_eq!(feed.items[0].base_url, "https://rel.example/posts/1");
        assert_eq!(feed.items[0].image_url, "https://rel.example/img/a.png");

        // With nothing to be relative to, a relative address is no address,
        // and the item is known by its title instead.
        let feed = parse(xml, "");
        assert_eq!(feed.items[0].url, "");
        assert_eq!(feed.items[0].base_url, "");
        assert_eq!(feed.items[0].identity, "title:rel");
    }

    #[test]
    fn an_address_that_is_not_a_web_address_is_not_kept() {
        let xml = r#"<rss version="2.0"><channel><title>T</title>
            <item><title>Odd</title><link>javascript:alert(1)</link><guid>g</guid></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items[0].url, "");
        assert_eq!(feed.items[0].identity, "guid:g");
    }

    #[test]
    fn an_item_with_nothing_to_show_is_dropped() {
        let xml = r#"<rss version="2.0"><channel><title>T</title>
            <item><guid>only-a-guid</guid></item>
            <item><title>Kept</title></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.items[0].identity, "title:kept");
    }

    #[test]
    fn an_item_that_is_only_words_is_known_by_what_it_says() {
        let xml = r#"<rss version="2.0"><channel><title>T</title>
            <item><description>First thought</description></item>
            <item><description>Second thought</description></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items.len(), 2);
        assert!(feed.items[0].identity.starts_with("text:"));
        assert_ne!(feed.items[0].identity, feed.items[1].identity);
        // And the same words are the same identity the next time.
        let again = parse(xml, "https://t.example/feed");
        assert_eq!(feed.items[0].identity, again.items[0].identity);
    }

    #[test]
    fn a_feed_of_more_than_two_hundred_keeps_the_newest_in_the_order_listed() {
        let start = ts("2026-01-01T00:00:00Z");
        let mut xml = String::from(r#"<rss version="2.0"><channel><title>Big</title>"#);
        // Oldest first, the opposite of how a feed lists them, so that
        // keeping the first two hundred would keep the wrong ones.
        for i in 0..250 {
            let at = start + SignedDuration::from_hours(i);
            xml.push_str(&format!(
                "<item><title>Item {i}</title><link>https://big.example/{i}</link>\
                 <pubDate>{at}</pubDate></item>"
            ));
        }
        xml.push_str("</channel></rss>");

        let feed = parse(&xml, "https://big.example/feed");
        assert_eq!(feed.items.len(), 200);
        assert_eq!(feed.items.first().unwrap().title, "Item 50");
        assert_eq!(feed.items.last().unwrap().title, "Item 249");
    }

    #[test]
    fn things_that_are_not_feeds_say_so() {
        let not_feeds: [&[u8]; 6] = [
            b"",
            b"   \n",
            b"just some words",
            b"<html><head><title>A web page</title></head><body><p>Hello</p></body></html>",
            b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            br#"{"hello": "world"}"#,
        ];
        for bytes in not_feeds {
            assert_eq!(
                parse_feed(bytes, "https://x.example/").unwrap_err(),
                "This is not a feed.",
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn broken_xml_and_json_say_the_feed_is_malformed() {
        let broken: [&[u8]; 2] = [
            MALFORMED.as_bytes(),
            br#"{"version": "https://jsonfeed.org/version/1.1", "title": "Cut off", "items": [{"#,
        ];
        for bytes in broken {
            let error = parse_feed(bytes, "https://x.example/").unwrap_err();
            assert!(error.starts_with("The feed is malformed: "), "{error}");
            assert!(error.ends_with('.'), "{error}");
        }
    }

    #[test]
    fn a_json_feed_of_a_version_nobody_wrote_is_unsupported() {
        let json = br#"{"version": "https://jsonfeed.org/version/9", "title": "x", "items": []}"#;
        assert_eq!(
            parse_feed(json, "https://x.example/").unwrap_err(),
            "This JSON Feed is a version that is not supported."
        );
    }

    #[test]
    fn an_empty_feed_is_a_feed() {
        let feed = parse(r#"<rss version="2.0"><channel><title>Quiet</title></channel></rss>"#, "");
        assert_eq!(feed.title, "Quiet");
        assert!(feed.items.is_empty());
    }
}
