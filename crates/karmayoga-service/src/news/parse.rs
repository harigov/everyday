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
//!
//! # Podcasts
//!
//! An item that carries something to hear or watch -- an RSS `<enclosure>`,
//! an Atom `rel="enclosure"` link, a JSON Feed attachment, a `media:content`
//! -- comes out with an [`Enclosure`] in [`Incoming::media`], and the feed as
//! a whole says whether it is a podcast ([`ParsedFeed::podcast`]). `feed-rs`
//! reads most of what an episode says, but not all of it, and gets one thing
//! wrong that matters: an `itunes:duration` of `45:12` (minutes and seconds,
//! which is how most shows write it) is read as forty-five seconds. An RSS or
//! Atom feed that has anything podcast-shaped in it is therefore read a
//! second time, with `roxmltree`, for exactly what `feed-rs` loses -- the
//! duration, episode and season numbers, every transcript, the show's
//! picture and author -- and each episode's is matched to `feed-rs`'s by its
//! enclosure's address. A feed the second reading cannot make sense of keeps
//! what `feed-rs` said.

use super::{Incoming, ParsedFeed, canonical};
use feed_rs::model::{Content, Entry, Feed, FeedType, Link, Text};
use feed_rs::parser::{self, ParseErrorKind, ParseFeedError};
use jiff::Timestamp;
use karmayoga_core::news::{BodyOrigin, Enclosure};
use reqwest::Url;
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::{HashMap, HashSet};
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

    // Behind `catch_unwind`, as `karmayoga_mail::text::html_to_text` is: the
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
    // Contained for the same reason, and only for the formats that are XML:
    // a JSON Feed's attachments are all `feed-rs` has to be asked for.
    let podcasting = if matches!(feed.feed_type, FeedType::JSON) {
        None
    } else {
        catch_unwind(AssertUnwindSafe(|| read_podcasting(bytes, base.as_ref()))).ok().flatten()
    };
    Ok(convert(feed, base.as_ref(), feed_url, podcasting.as_ref()))
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

fn convert(
    feed: Feed,
    base: Option<&Url>,
    feed_url: &str,
    podcasting: Option<&Podcasting>,
) -> ParsedFeed {
    // Atom and JSON Feed say what kind of text a field holds, so their
    // plain text is plain. RSS says nothing, and what `feed-rs` calls plain
    // in an RSS feed is in practice HTML with its angle brackets escaped.
    let plain_is_text = matches!(feed.feed_type, FeedType::Atom | FeedType::JSON);

    let title = feed.title.as_ref().map(|t| title_text(t, plain_is_text)).unwrap_or_default();
    let site_url = page_link(&feed.links).map(|l| absolute(base, &l.href)).unwrap_or_default();
    let ttl_minutes = feed.ttl.filter(|&minutes| minutes > 0);
    let image_url = feed_image(&feed, base, podcasting);
    let author = feed_author(&feed, podcasting);

    // Two entries that are one story -- the same guid republished with a
    // corrected title, say -- are one item here, the first listed (feeds list
    // the newest first) winning, because the store would keep only the first
    // inserted anyway.
    let mut items: Vec<(String, Incoming)> = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let guid = entry.id.trim().to_string();
            item(entry, base, feed_url, plain_is_text, podcasting).map(|incoming| (guid, incoming))
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

    let declared = podcasting.is_some_and(|p| p.declared);
    let podcast = is_podcast(&items, declared);
    ParsedFeed { title, site_url, items, ttl_minutes, podcast, image_url, author }
}

/// Whether a feed is a podcast: its items carry something to hear or watch,
/// or, while it has no items yet, its `itunes:` tags say it will.
///
/// One audio attachment in a blog's hundred posts does not make the blog a
/// podcast, which would have every post of it downloaded: it takes at least
/// half of the items, or the feed's own `itunes:` declaration with at least
/// one of them.
fn is_podcast(items: &[Incoming], declared: bool) -> bool {
    if items.is_empty() {
        return declared;
    }
    let playable = items.iter().filter(|item| item.media.is_some()).count();
    playable > 0 && (declared || playable * 2 >= items.len())
}

/// The feed's own picture: `itunes:image`, which is the square one shows are
/// asked to provide, else the feed's logo or icon.
fn feed_image(feed: &Feed, base: Option<&Url>, podcasting: Option<&Podcasting>) -> String {
    podcasting
        .map(|p| p.image.clone())
        .filter(|image| !image.is_empty())
        .or_else(|| {
            [feed.logo.as_ref(), feed.icon.as_ref()]
                .into_iter()
                .flatten()
                .map(|image| absolute(base, &image.uri))
                .find(|url| !url.is_empty())
        })
        .unwrap_or_default()
}

/// `itunes:author`, else the first author the feed names, as plain text.
fn feed_author(feed: &Feed, podcasting: Option<&Podcasting>) -> String {
    podcasting
        .map(|p| p.author.clone())
        .filter(|author| !author.is_empty())
        .or_else(|| {
            feed.authors
                .iter()
                .filter_map(|person| person.name.as_deref())
                .map(plain)
                .find(|name| !name.is_empty())
        })
        .unwrap_or_default()
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

fn item(
    entry: Entry,
    base: Option<&Url>,
    feed_url: &str,
    plain_is_text: bool,
    podcasting: Option<&Podcasting>,
) -> Option<Incoming> {
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
        media: episode(&entry, base, podcasting),
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

/// What `feed-rs` leaves out of a podcast, or gets wrong, found by reading
/// the feed's XML a second time. See the module doc.
#[derive(Debug, Default)]
struct Podcasting {
    /// The channel carries `itunes:` tags of its own: it was written for
    /// podcast directories, whatever its items turn out to hold.
    declared: bool,
    /// `itunes:image`, absolute.
    image: String,
    /// `itunes:author`, as plain text.
    author: String,
    /// What each item said about its episode, by the address of the file it
    /// names, as [`absolute`] writes it.
    episodes: HashMap<String, Episode>,
}

/// What an item says about its episode in `itunes:` and `podcast:` tags.
#[derive(Debug, Clone, Default, PartialEq)]
struct Episode {
    duration_ms: Option<u64>,
    episode: Option<u32>,
    season: Option<u32>,
    /// The best transcript offered: its address and its type.
    transcript: Option<(String, String)>,
}

impl Episode {
    /// What this reading says about an enclosure, over what `feed-rs` said:
    /// the second reading is the more careful one, so wherever it found
    /// something it is that which stands.
    fn apply(&self, media: &mut Enclosure) {
        if self.duration_ms.is_some() {
            media.duration_ms = self.duration_ms;
        }
        if self.episode.is_some() {
            media.episode = self.episode;
        }
        if self.season.is_some() {
            media.season = self.season;
        }
        if let Some((url, mime)) = &self.transcript {
            media.transcript_url = url.clone();
            media.transcript_mime = mime.clone();
        }
    }
}

/// The namespaces whose tags matter here, as [`in_namespace`] compares them.
const ITUNES_NS: &str = "www.itunes.com/dtds/podcast-1.0.dtd";
const PODCAST_NS: &str = "podcastindex.org/namespace/1.0";
const MEDIA_NS: &str = "search.yahoo.com/mrss";

/// Whether `node` is in `namespace`, ignoring what publishers get differently
/// from one another: the scheme, a trailing slash, and the case -- Apple's
/// own documentation spells the `itunes` address in lower case and a great
/// many feeds in `DTDs/Podcast`.
fn in_namespace(node: &Node, namespace: &str) -> bool {
    node.tag_name().namespace().is_some_and(|ns| {
        let ns = ns.trim().trim_end_matches('/');
        let ns = ns.strip_prefix("https://").or_else(|| ns.strip_prefix("http://")).unwrap_or(ns);
        ns.eq_ignore_ascii_case(namespace)
    })
}

/// Read a feed's XML for what `feed-rs` loses of a podcast. `None` when the
/// feed has nothing podcast-shaped in it, is not UTF-8, or is not XML that
/// `roxmltree` accepts -- in each of which `feed-rs`'s own reading stands.
fn read_podcasting(bytes: &[u8], base: Option<&Url>) -> Option<Podcasting> {
    let text = xml_text(bytes)?;
    let text = text.trim_start_matches('\u{feff}');
    // A feed with none of these words cannot have anything to find, which is
    // nearly every feed, and a second pass over a few megabytes of blog is
    // not worth making to learn it.
    if !["itunes", "enclosure", "podcastindex"].iter().any(|word| text.contains(word)) {
        return None;
    }
    // A DTD is allowed for the reason OPML's reader allows one: old feeds
    // carry one, and `roxmltree` fetches nothing from it. The node limit is
    // what keeps a hostile file's memory in check.
    let options =
        ParsingOptions { allow_dtd: true, nodes_limit: 1_000_000, ..ParsingOptions::default() };
    let doc = Document::parse_with_options(text, options).ok()?;
    let root = doc.root_element();

    let mut podcasting = Podcasting::default();
    // RSS 2.0 keeps the show's own tags in `<channel>`; Atom keeps them in
    // `<feed>`, which is the root.
    let channel = root
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "channel")
        .unwrap_or(root);
    for tag in channel.children().filter(|node| node.is_element() && in_namespace(node, ITUNES_NS))
    {
        podcasting.declared = true;
        match tag.tag_name().name() {
            "image" => {
                if let Some(href) = tag.attribute("href") {
                    podcasting.image = absolute(base, href);
                }
            }
            "author" => podcasting.author = text_of(&tag),
            _ => {}
        }
    }

    for item in doc
        .descendants()
        .filter(|node| node.is_element() && matches!(node.tag_name().name(), "item" | "entry"))
    {
        let (addresses, episode) = read_episode(&item, base);
        for address in addresses {
            podcasting.episodes.entry(address).or_insert_with(|| episode.clone());
        }
    }
    Some(podcasting)
}

/// The feed as text, for a second reading: as it is when it is UTF-8, and
/// otherwise in the encoding its XML declaration names, which for an older
/// feed is `ISO-8859-1` or `windows-1252` as often as not. `None` when it
/// names none, names one that is not a text encoding for this, or is not
/// written in the one it names.
fn xml_text(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(text.into());
    }
    // The declaration is ASCII and the first thing in the file.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    let declared = head.split_once("encoding=")?.1;
    let quote = declared.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let label = declared[1..].split(quote).next()?;
    let encoding = encoding_rs::Encoding::for_label(label.trim().as_bytes())?;
    if encoding == encoding_rs::UTF_8 || encoding.output_encoding() != encoding {
        return None;
    }
    encoding.decode_without_bom_handling_and_without_replacement(bytes)
}

/// One item's addresses for a file -- an `<enclosure>`, an Atom
/// `rel="enclosure"` link, a `media:content` -- and what it says about the
/// episode.
fn read_episode(item: &Node, base: Option<&Url>) -> (Vec<String>, Episode) {
    let mut addresses = Vec::new();
    let mut episode = Episode::default();
    // The `podcast:` spellings of the numbers only count when the item has no
    // `itunes:` one, which is the one most players read.
    let (mut itunes_episode, mut podcast_episode) = (None, None);
    let (mut itunes_season, mut podcast_season) = (None, None);
    let mut transcripts = Vec::new();

    for tag in item.children().filter(Node::is_element) {
        let name = tag.tag_name().name();
        if in_namespace(&tag, ITUNES_NS) {
            match name {
                "duration" => episode.duration_ms = parse_duration_ms(&text_of(&tag)),
                "episode" => itunes_episode = whole_number(&text_of(&tag)),
                "season" => itunes_season = whole_number(&text_of(&tag)),
                _ => {}
            }
        } else if in_namespace(&tag, PODCAST_NS) {
            match name {
                "episode" => podcast_episode = whole_number(&text_of(&tag)),
                "season" => podcast_season = whole_number(&text_of(&tag)),
                "transcript" => {
                    if let (Some(url), Some(mime)) = (tag.attribute("url"), tag.attribute("type")) {
                        transcripts.push((absolute(base, url), mime.to_string()));
                    }
                }
                _ => {}
            }
        } else if in_namespace(&tag, MEDIA_NS) {
            // `media:content` is either the item's own or inside a
            // `media:group`.
            let contents = if name == "group" {
                tag.children().filter(Node::is_element).collect()
            } else {
                vec![tag]
            };
            for content in contents.iter().filter(|c| c.tag_name().name() == "content") {
                addresses.extend(content.attribute("url").map(|url| absolute(base, url)));
            }
        } else if name == "enclosure" {
            addresses.extend(tag.attribute("url").map(|url| absolute(base, url)));
        } else if name == "link" && tag.attribute("rel").is_some_and(is_enclosure_rel) {
            addresses.extend(tag.attribute("href").map(|href| absolute(base, href)));
        }
    }

    episode.episode = itunes_episode.or(podcast_episode);
    episode.season = itunes_season.or(podcast_season);
    episode.transcript = best_transcript(transcripts);
    addresses.retain(|address| !address.is_empty());
    (addresses, episode)
}

fn is_enclosure_rel(rel: &str) -> bool {
    rel.trim().eq_ignore_ascii_case("enclosure")
}

/// An element's text, as plain text.
fn text_of(node: &Node) -> String {
    plain(&node.descendants().filter(Node::is_text).filter_map(|n| n.text()).collect::<String>())
}

/// `itunes:duration`, in milliseconds: seconds (`3600`), minutes and seconds
/// (`45:12`), or hours, minutes and seconds (`1:02:03`), the last of them
/// with a fraction if the publisher wrote one. `None` for anything else, and
/// for no time at all.
fn parse_duration_ms(text: &str) -> Option<u64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0_f64;
    for part in parts {
        let part: f64 = part.trim().parse().ok()?;
        if !part.is_finite() || part < 0.0 {
            return None;
        }
        seconds = seconds * 60.0 + part;
    }
    let millis = (seconds * 1000.0).round();
    (millis > 0.0 && millis < 1e12).then_some(millis as u64)
}

/// An episode or season number, which publishers write as `7`, or as `7.0`.
fn whole_number(text: &str) -> Option<u32> {
    let text = text.trim();
    text.parse().ok().or_else(|| text.parse::<f32>().ok().and_then(whole))
}

/// `number` as a count, when it is one.
fn whole(number: f32) -> Option<u32> {
    (number.is_finite() && number >= 0.0 && number.fract() == 0.0).then_some(number as u32)
}

/// Milliseconds in `duration`, when there are any.
fn millis(duration: std::time::Duration) -> Option<u64> {
    u64::try_from(duration.as_millis()).ok().filter(|&millis| millis > 0)
}

/// The transcript to prefer of the ones an episode offers, as (address,
/// type): WebVTT, then SubRip, then JSON, then HTML, then plain text. Those
/// are the ones that can be read with times; one of any other type is not
/// offered, and of two of one type the first listed is.
fn best_transcript(
    offered: impl IntoIterator<Item = (String, String)>,
) -> Option<(String, String)> {
    offered
        .into_iter()
        .filter(|(url, _)| !url.is_empty())
        .filter_map(|(url, mime)| {
            let mime = essence(&mime).to_ascii_lowercase();
            let rank = match mime.as_str() {
                "text/vtt" => 0,
                "application/x-subrip" | "application/srt" | "application/x-srt" | "text/srt" => 1,
                "application/json" => 2,
                "text/html" => 3,
                "text/plain" => 4,
                _ => return None,
            };
            Some((rank, url, mime))
        })
        .min_by_key(|(rank, ..)| *rank)
        .map(|(_, url, mime)| (url, mime))
}

/// The episode an item is, if it carries something to hear or watch: the
/// first playable file it names -- a `media:content`, an RSS enclosure, an
/// Atom `rel="enclosure"` link, a JSON Feed attachment -- with what the feed
/// said about it. A file that is a picture, or a PDF, is no episode; it is
/// [`lead_image`]'s, or nobody's.
fn episode(
    entry: &Entry,
    base: Option<&Url>,
    podcasting: Option<&Podcasting>,
) -> Option<Enclosure> {
    let named = entry.media.iter().flat_map(|m| m.content.iter()).filter_map(|content| {
        let href = content.url.as_ref()?.as_str();
        let mime = content.content_type.as_ref().map(ToString::to_string).unwrap_or_default();
        Some((href, mime, content.size, content.duration))
    });
    let linked = entry
        .links
        .iter()
        .filter(|link| {
            link.rel.as_deref().is_some_and(is_enclosure_rel)
                || link.media_type.as_deref().is_some_and(is_playable_type)
        })
        .map(|link| {
            (link.href.as_str(), link.media_type.clone().unwrap_or_default(), link.length, None)
        });

    let mut media = named.chain(linked).find_map(|(href, mime, length, duration)| {
        let enclosure = Enclosure {
            url: absolute(base, href),
            mime: essence(&mime).to_ascii_lowercase(),
            length: length.filter(|&bytes| bytes > 0),
            duration_ms: duration.and_then(millis),
            transcript_url: String::new(),
            transcript_mime: String::new(),
            episode: None,
            season: None,
        };
        (!enclosure.url.is_empty() && enclosure.is_playable()).then_some(enclosure)
    })?;

    // What `feed-rs` read of the episode itself. It folds `itunes:` and
    // `podcast:` tags into the item's media, so these are what a feed that
    // `roxmltree` could not read still says.
    media.duration_ms =
        media.duration_ms.or_else(|| entry.media.iter().find_map(|m| m.duration).and_then(millis));
    media.episode =
        entry.media.iter().find_map(|m| m.episode.as_ref()).and_then(|e| whole(e.number));
    media.season = entry.media.iter().find_map(|m| m.season.as_ref()).map(|s| s.number);
    let transcripts = entry
        .media
        .iter()
        .flat_map(|m| m.transcripts.iter())
        .map(|t| (absolute(base, t.url.as_str()), t.content_type.to_string()));
    if let Some((url, mime)) = best_transcript(transcripts) {
        media.transcript_url = url;
        media.transcript_mime = mime;
    }

    if let Some(said) = podcasting.and_then(|p| p.episodes.get(&media.url)) {
        said.apply(&mut media);
    }
    Some(media)
}

fn is_playable_type(mime: &str) -> bool {
    let mime = essence(mime);
    mime.starts_with("audio/") || mime.starts_with("video/")
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
        assert!(!feed.podcast);
    }

    // ── podcasts ─────────────────────────────────────────────────────────

    /// A show as a publisher writes one: the `itunes` namespace spelled the way
    /// most feeds spell it (`DTDs/Podcast`), the `podcast` one over `http`,
    /// durations in the three shapes they come in, and an episode whose
    /// enclosure says nothing of its type.
    const PODCAST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0"
             xmlns:itunes="http://www.itunes.com/DTDs/Podcast-1.0.dtd"
             xmlns:podcast="http://podcastindex.org/namespace/1.0"
             xmlns:content="http://purl.org/rss/1.0/modules/content/">
          <channel>
            <title>Build Times</title>
            <link>https://build.example/</link>
            <description>Forty minutes at a time.</description>
            <image><url>https://build.example/small.png</url><title>Build Times</title></image>
            <itunes:author>Rhea &amp; Amit</itunes:author>
            <itunes:image href="https://build.example/art/3000.jpg"/>
            <itunes:category text="Technology"/>
            <item>
              <title>12. The sound of compilers</title>
              <link>https://build.example/episodes/12</link>
              <guid isPermaLink="false">build-12</guid>
              <pubDate>Fri, 02 Oct 2026 06:00:00 GMT</pubDate>
              <description>Forty minutes on build times.</description>
              <enclosure url="https://cdn.example/12.mp3" length="38211000" type="audio/mpeg"/>
              <itunes:duration>45:12</itunes:duration>
              <itunes:episode>12</itunes:episode>
              <itunes:season>2</itunes:season>
              <podcast:transcript url="https://build.example/12.html" type="text/html"/>
              <podcast:transcript url="/12.srt" type="application/x-subrip"/>
              <podcast:transcript url="https://build.example/12.vtt" type="text/vtt"/>
              <podcast:transcript url="https://build.example/12.pdf" type="application/pdf"/>
            </item>
            <item>
              <title>11. Linkers</title>
              <link>https://build.example/episodes/11</link>
              <guid isPermaLink="false">build-11</guid>
              <pubDate>Fri, 25 Sep 2026 06:00:00 GMT</pubDate>
              <enclosure url="https://cdn.example/11.mp3?token=a&amp;b=1" length="0"/>
              <itunes:duration>01:02:03</itunes:duration>
              <podcast:episode>11</podcast:episode>
            </item>
            <item>
              <title>10. Show notes only</title>
              <link>https://build.example/episodes/10</link>
              <guid isPermaLink="false">build-10</guid>
              <pubDate>Fri, 18 Sep 2026 06:00:00 GMT</pubDate>
              <enclosure url="https://cdn.example/10-notes.pdf" length="900" type="application/pdf"/>
              <itunes:duration>3600</itunes:duration>
            </item>
          </channel>
        </rss>"#;

    #[test]
    fn a_podcasts_enclosure_carries_what_the_episode_says() {
        let feed = parse(PODCAST, "https://build.example/feed.xml");
        assert!(feed.podcast);
        assert_eq!(feed.items.len(), 3);

        let media = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(media.url, "https://cdn.example/12.mp3");
        assert_eq!(media.mime, "audio/mpeg");
        assert_eq!(media.length, Some(38_211_000));
        // `45:12` is minutes and seconds, not forty-five seconds.
        assert_eq!(media.duration_ms, Some((45 * 60 + 12) * 1000));
        assert_eq!((media.episode, media.season), (Some(12), Some(2)));
        // WebVTT over SubRip over HTML, and a PDF is no transcript.
        assert_eq!(media.transcript_url, "https://build.example/12.vtt");
        assert_eq!(media.transcript_mime, "text/vtt");
        // The item itself is an ordinary story.
        assert_eq!(feed.items[0].title, "12. The sound of compilers");
        assert_eq!(feed.items[0].url, "https://build.example/episodes/12");
        assert_eq!(feed.items[0].image_url, "");
    }

    #[test]
    fn a_podcasts_picture_and_author_are_the_itunes_ones() {
        let feed = parse(PODCAST, "https://build.example/feed.xml");
        // Not the `<image>` that is smaller, and the entity is decoded.
        assert_eq!(feed.image_url, "https://build.example/art/3000.jpg");
        assert_eq!(feed.author, "Rhea & Amit");
        assert_eq!(feed.title, "Build Times");
    }

    #[test]
    fn an_enclosure_with_no_type_is_judged_by_its_address_and_the_podcast_namespace_fills_in() {
        let feed = parse(PODCAST, "https://build.example/feed.xml");
        let media = feed.items[1].media.as_ref().expect("an episode");
        assert_eq!(media.url, "https://cdn.example/11.mp3?token=a&b=1");
        assert_eq!(media.mime, "");
        assert_eq!(media.length, None, "a length of nought is not a length");
        assert_eq!(media.duration_ms, Some((3600 + 2 * 60 + 3) * 1000));
        assert_eq!((media.episode, media.season), (Some(11), None));
        assert_eq!(media.transcript_url, "");
    }

    #[test]
    fn a_file_that_is_not_audio_or_video_is_not_an_episode() {
        let feed = parse(PODCAST, "https://build.example/feed.xml");
        assert_eq!(feed.items[2].media, None);
        // Nor is a picture, and neither is the lead picture of anything.
        let feed = parse(RSS2, "https://quiet.example/feed.xml");
        assert_eq!(feed.items[1].media, None);
        assert_eq!(feed.items[1].image_url, "https://quiet.example/img/week41.jpg");
    }

    #[test]
    fn one_audio_attachment_does_not_make_a_blog_a_podcast() {
        let feed = parse(RSS2, "https://quiet.example/feed.xml");
        let media = feed.items[2].media.as_ref().expect("the one episode");
        assert_eq!(media.url, "https://quiet.example/audio/12.mp3");
        assert_eq!(media.kind(), karmayoga_core::news::EnclosureKind::Audio);
        assert!(!feed.podcast);
        assert_eq!(feed.image_url, "");
        assert_eq!(feed.author, "");
        assert!(feed.items[..2].iter().all(|item| item.media.is_none()));
    }

    #[test]
    fn a_feed_that_says_it_is_a_podcast_with_one_episode_among_posts_is_one() {
        let xml = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel><title>Mostly words</title><itunes:owner><itunes:name>Me</itunes:name></itunes:owner>
            <item><title>A post</title><link>https://m.example/1</link></item>
            <item><title>Another</title><link>https://m.example/2</link></item>
            <item><title>An episode</title><link>https://m.example/3</link>
              <enclosure url="https://m.example/3.m4a" type="audio/x-m4a"/></item>
            </channel></rss>"#;
        assert!(parse(xml, "https://m.example/feed").podcast);
        // Without the declaration it is the blog it looks like.
        let plain = xml.replace("<itunes:owner><itunes:name>Me</itunes:name></itunes:owner>", "");
        assert!(!parse(&plain, "https://m.example/feed").podcast);
    }

    #[test]
    fn a_new_show_with_no_episodes_yet_is_a_podcast_by_its_declaration() {
        let xml = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel><title>Soon</title><itunes:category text="News"/></channel></rss>"#;
        let feed = parse(xml, "https://soon.example/feed");
        assert!(feed.podcast);
        assert!(feed.items.is_empty());
    }

    #[test]
    fn an_atom_enclosure_link_is_the_episode() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
            <title>Atom Radio</title>
            <author><name>The Station</name></author>
            <entry><id>tag:radio.example,2026:2</id><title>Two</title>
              <link rel="alternate" href="https://radio.example/2"/>
              <link rel="enclosure" type="audio/ogg" length="5000" href="/audio/2.ogg"/>
              <link rel="enclosure" type="image/png" href="/art/2.png"/>
              <updated>2026-10-02T10:00:00Z</updated></entry>
            <entry><id>tag:radio.example,2026:1</id><title>One</title>
              <link rel="alternate" href="https://radio.example/1"/>
              <link rel="enclosure" type="video/mp4" href="https://radio.example/video/1.mp4"/>
              <updated>2026-10-01T10:00:00Z</updated></entry>
            </feed>"#;
        let feed = parse(xml, "https://radio.example/feed.atom");
        assert!(feed.podcast);
        assert_eq!(feed.author, "The Station");
        let two = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(two.url, "https://radio.example/audio/2.ogg");
        assert_eq!(two.mime, "audio/ogg");
        assert_eq!(two.length, Some(5000));
        assert_eq!(two.kind(), karmayoga_core::news::EnclosureKind::Audio);
        // The page is still the page, and the picture still the picture.
        assert_eq!(feed.items[0].url, "https://radio.example/2");
        assert_eq!(feed.items[0].image_url, "https://radio.example/art/2.png");
        let one = feed.items[1].media.as_ref().expect("an episode");
        assert_eq!(one.kind(), karmayoga_core::news::EnclosureKind::Video);
    }

    #[test]
    fn a_json_feed_attachment_is_the_episode() {
        let json = r#"{
          "version": "https://jsonfeed.org/version/1.1",
          "title": "Voice Notes",
          "icon": "https://notes.example/icon.png",
          "authors": [{ "name": "Ines" }],
          "items": [
            { "id": "2", "url": "https://notes.example/2", "title": "Two",
              "attachments": [
                { "url": "https://notes.example/2.mp3", "mime_type": "audio/mpeg",
                  "size_in_bytes": 1234 } ] },
            { "id": "1", "url": "https://notes.example/1", "title": "One",
              "attachments": [
                { "url": "https://notes.example/1.png", "mime_type": "image/png" },
                { "url": "https://notes.example/1.m4a", "mime_type": "audio/mp4" } ] }
          ]
        }"#;
        let feed = parse(json, "https://notes.example/feed.json");
        assert!(feed.podcast);
        assert_eq!(feed.image_url, "https://notes.example/icon.png");
        assert_eq!(feed.author, "Ines");
        let two = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(two.url, "https://notes.example/2.mp3");
        assert_eq!((two.mime.as_str(), two.length), ("audio/mpeg", Some(1234)));
        let one = feed.items[1].media.as_ref().expect("an episode");
        assert_eq!(one.url, "https://notes.example/1.m4a", "the picture is not the episode");
        assert_eq!(feed.items[1].image_url, "https://notes.example/1.png");
        // The ordinary JSON Feed has nothing to play.
        let plain = parse(JSON, "https://weeknotes.example/feed.json");
        assert!(!plain.podcast);
        assert!(plain.items.iter().all(|item| item.media.is_none()));
    }

    #[test]
    fn a_media_content_that_is_audio_is_an_episode_and_one_that_is_flash_is_not() {
        let xml = r#"<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/">
            <channel><title>Mixed</title>
            <item><title>Audio</title><link>https://x.example/a</link>
              <media:content url="https://x.example/a.mp3" type="audio/mpeg" fileSize="999"
                  duration="90"/></item>
            <item><title>Flash</title><link>https://x.example/f</link>
              <media:content url="https://x.example/v" type="application/x-shockwave-flash"/></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://x.example/feed");
        let media = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(media.url, "https://x.example/a.mp3");
        assert_eq!((media.length, media.duration_ms), (Some(999), Some(90_000)));
        assert_eq!(feed.items[1].media, None);
    }

    #[test]
    fn a_feed_with_a_doctype_is_still_read_for_its_episodes() {
        let xml = r#"<?xml version="1.0"?>
            <!DOCTYPE rss [<!ENTITY unused "Old Show">]>
            <rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel><title>Old Show</title>
            <item><title>One</title><link>https://old.example/1</link>
              <enclosure url="https://old.example/1.mp3" type="audio/mpeg"/>
              <itunes:duration>12:34</itunes:duration></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://old.example/feed");
        assert_eq!(feed.title, "Old Show");
        let media = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(media.duration_ms, Some((12 * 60 + 34) * 1000));
    }

    #[test]
    fn durations_are_read_in_the_shapes_publishers_write_them() {
        let cases = [
            ("3600", Some(3_600_000)),
            ("45:12", Some(2_712_000)),
            ("1:02:03", Some(3_723_000)),
            ("01:02:03.5", Some(3_723_500)),
            ("  12:34 ", Some(754_000)),
            ("90.5", Some(90_500)),
            ("0", None),
            ("0:00", None),
            ("", None),
            ("soon", None),
            ("1:2:3:4", None),
            ("-5", None),
            ("1:-5", None),
            ("inf", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_duration_ms(text), expected, "{text:?}");
        }
    }

    #[test]
    fn episode_and_season_numbers_are_whole_numbers() {
        assert_eq!(whole_number(" 7 "), Some(7));
        assert_eq!(whole_number("7.0"), Some(7));
        assert_eq!(whole_number("7.5"), None);
        assert_eq!(whole_number("-1"), None);
        assert_eq!(whole_number("bonus"), None);
    }

    #[test]
    fn the_best_transcript_is_the_one_that_keeps_its_times() {
        let offered = |types: &[&str]| {
            types
                .iter()
                .enumerate()
                .map(|(at, mime)| (format!("https://t.example/{at}"), mime.to_string()))
                .collect::<Vec<_>>()
        };
        let best = |types: &[&str]| best_transcript(offered(types)).map(|(_, mime)| mime);
        assert_eq!(best(&["text/plain", "text/html", "text/vtt"]), Some("text/vtt".into()));
        assert_eq!(
            best(&["text/html", "application/json", "application/srt"]),
            Some("application/srt".into())
        );
        assert_eq!(
            best(&["text/plain", "text/html", "application/json"]),
            Some("application/json".into())
        );
        assert_eq!(best(&["text/plain", "text/html"]), Some("text/html".into()));
        assert_eq!(best(&["text/plain; charset=utf-8"]), Some("text/plain".into()));
        // Of two of a kind, the first listed.
        let two = vec![
            ("https://t.example/a".to_string(), "text/vtt".to_string()),
            ("https://t.example/b".to_string(), "TEXT/VTT".to_string()),
        ];
        assert_eq!(best_transcript(two).unwrap().0, "https://t.example/a");
        assert_eq!(best(&["application/pdf", "audio/mpeg"]), None);
        assert_eq!(best(&[]), None);
        assert_eq!(best_transcript([(String::new(), "text/vtt".to_string())]), None);
    }

    #[test]
    fn a_feed_in_an_older_encoding_is_read_for_its_episodes_too() {
        let mut xml = br#"<?xml version="1.0" encoding="ISO-8859-1"?>
            <rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel><title>Caf"#
            .to_vec();
        xml.push(0xE9);
        xml.extend_from_slice(
            br#"</title>
            <item><title>One</title><link>https://c.example/1</link>
              <enclosure url="https://c.example/1.mp3" type="audio/mpeg"/>
              <itunes:duration>12:34</itunes:duration></item>
            </channel></rss>"#,
        );
        let feed = parse_feed(&xml, "https://c.example/feed").expect("a feed");
        assert_eq!(feed.title, "Caf\u{e9}");
        let media = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(media.url, "https://c.example/1.mp3");
        assert_eq!(media.duration_ms, Some((12 * 60 + 34) * 1000));
        assert!(feed.podcast);
    }

    #[test]
    fn a_feed_the_second_reading_cannot_make_sense_of_keeps_feed_rss_reading() {
        // An entity no DTD defines is something `feed-rs` lets pass and
        // `roxmltree` does not, so the second reading is skipped and what the
        // first made of the episode -- here with an `itunes:summary`, which
        // is what makes it keep the rest -- stands.
        let xml = r#"<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
            <channel><title>Odd</title>
            <item><title>One</title><link>https://o.example/1</link>
              <description>Wait&hellip;</description>
              <enclosure url="https://o.example/1.mp3" type="audio/mpeg"/>
              <itunes:summary>What it is.</itunes:summary>
              <itunes:duration>3600</itunes:duration><itunes:episode>4</itunes:episode></item>
            </channel></rss>"#;
        let feed = parse(xml, "https://o.example/feed");
        let media = feed.items[0].media.as_ref().expect("an episode");
        assert_eq!(media.url, "https://o.example/1.mp3");
        assert_eq!(media.duration_ms, Some(3_600_000));
        assert_eq!(media.episode, Some(4));
        assert!(feed.podcast);
    }
}
