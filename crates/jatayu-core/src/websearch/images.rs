//! Looking for a picture of something.
//!
//! The library's cover picker and the assistant's `search_images` ask the
//! same question -- what does this look like -- and this is everything about
//! it but the socket: the addresses asked, and what the answers mean. The
//! shell fetches and hands the bytes back, as it does for every other search
//! in [`crate::websearch`].
//!
//! # Which searches, and why these
//!
//! Bing first, then Wikimedia Commons. Both need no key and no account, the
//! rule every outward lookup here keeps (see the parent module's "Which
//! sources, and why these").
//!
//! DuckDuckGo would have been the first choice, as it is for a plain web
//! search, but its image endpoint answers every request that is not a
//! browser running its script with a bot check (October 2026). Its pictures
//! are Bing's in any case, so asking Bing directly gets the same answers. The
//! cost is the same one [`Source::Web`](super::Source::Web) pays: this reads
//! a page meant for a browser, which can change without notice, and
//! [`ImageSource::Bing`]'s parser returns nothing rather than nonsense when
//! it does.
//!
//! Commons is the other kind of answer: a documented interface that will
//! not change underneath this file, and only freely licensed photographs --
//! good for a place, a dish or a plant, poor for a book's cover or a
//! bottle's label. So it is asked when Bing has nothing, or has stopped
//! answering in a shape this file can read.
//!
//! # What leaves the machine
//!
//! The words searched for, to Bing, and to Commons only if Bing had nothing.
//! No identifier and no history. Bing is asked for its moderate safe search,
//! which is what it gives an anonymous visitor anyway; it is written into
//! the address so that a change of its default does not change what lands
//! on a shelf.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{decode_entities, decode_percent, encode, http_url, is_invisible};
use crate::error::{Error, Result};

/// The most pictures one search asks for. A grid of thirty is already more
/// than anybody reads before searching again with better words.
pub const MAX_IMAGES: u32 = 30;

/// How many pictures a search asks for when the caller has no opinion.
pub const DEFAULT_IMAGES: u32 = 12;

/// Where a picture is looked for. See the module doc for why these two, and
/// why in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageSource {
    Bing,
    Commons,
}

impl ImageSource {
    /// The order a search tries them in: the next is asked only when the one
    /// before came back with nothing, or failed.
    pub const ATTEMPTS: [ImageSource; 2] = [ImageSource::Bing, ImageSource::Commons];

    /// The stable name an [`ImageResult::source`] carries.
    pub fn slug(self) -> &'static str {
        match self {
            ImageSource::Bing => "bing",
            ImageSource::Commons => "commons",
        }
    }

    /// What a failure's sentence calls it.
    pub fn label(self) -> &'static str {
        match self {
            ImageSource::Bing => "Bing",
            ImageSource::Commons => "Wikimedia Commons",
        }
    }

    /// What to send as `Accept`.
    pub fn accept(self) -> &'static str {
        match self {
            ImageSource::Bing => "text/html",
            ImageSource::Commons => "application/json",
        }
    }

    /// The address to ask. The only place one is built.
    pub fn url(self, query: &str, limit: u32) -> Result<String> {
        let query = query.trim();
        if query.is_empty() {
            return Err(Error::Invalid("a picture search needs something to search for".into()));
        }
        let q = encode(query);
        let limit = limit.clamp(1, MAX_IMAGES);
        Ok(match self {
            // The endpoint the results page fills its grid from as you
            // scroll: the same results, without the page around them.
            ImageSource::Bing => format!(
                "https://www.bing.com/images/async?q={q}&first=0&count={limit}&mmasync=1\
                 &adlt=moderate"
            ),
            // Files rather than articles (namespace 6), each with a copy
            // scaled to 960 pixels across -- a cover's worth, where an
            // original can be a fifty-megapixel photograph.
            ImageSource::Commons => format!(
                "https://commons.wikimedia.org/w/api.php?action=query&format=json\
                 &formatversion=2&generator=search&gsrsearch={q}&gsrnamespace=6\
                 &gsrlimit={limit}&prop=imageinfo&iiprop=url|size|mime|extmetadata\
                 &iiurlwidth={COMMONS_WIDTH}&iiextmetadatafilter=ObjectName"
            ),
        })
    }

    /// Turn a response body into pictures. Never panics on malformed input;
    /// a body that is not even the right kind of document is an error, so
    /// that "Bing changed its page" and "Bing found nothing" can be told
    /// apart in a log.
    pub fn parse(self, body: &str) -> Result<Vec<ImageResult>> {
        match self {
            ImageSource::Bing => Ok(parse_bing(body)),
            ImageSource::Commons => parse_commons(body),
        }
    }
}

/// One picture a search found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImageResult {
    /// What the page the picture is on calls it, or the file's own name.
    pub title: String,
    /// The picture itself, full size -- what a cover is made from.
    pub image_url: String,
    /// A small copy, for a grid to show before anything is chosen. Hosted
    /// by the search rather than the page, so it loads when the original's
    /// site refuses to be asked by anything that is not a browser.
    pub thumbnail_url: String,
    /// The page the picture is on.
    pub page_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The page's site, without its `www.`: who the picture belongs to, at
    /// a glance.
    pub site: String,
    /// Which [`ImageSource`] found it, by slug.
    pub source: String,
}

/// The width Commons is asked to scale each picture to.
const COMMONS_WIDTH: u32 = 960;

/// The width a Commons thumbnail is cut to for a grid: one of the sizes
/// Wikimedia keeps ready, so asking for it never makes a server render one.
const COMMONS_THUMB_WIDTH: u32 = 330;

// ── Bing ─────────────────────────────────────────────────────────────────

/// The class each result's link carries.
const BING_RESULT: &str = "class=\"iusc\"";

/// Pull pictures out of Bing's image results.
///
/// Each result is an anchor with class `iusc` whose `m` attribute is a JSON
/// object, entity-escaped: `murl` is the picture, `turl` Bing's thumbnail of
/// it, `purl` the page it is on and `t` that page's title. Its `href` carries
/// the picture's size as `expw` and `exph`. Everything else on the page is
/// ignored, and a result that cannot be read is skipped rather than guessed
/// at -- see [`parse_duckduckgo`](super::parse_duckduckgo), which reads its
/// page on the same terms.
fn parse_bing(html: &str) -> Vec<ImageResult> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = html[cursor..].find(BING_RESULT) {
        let at = cursor + offset;
        cursor = at + BING_RESULT.len();
        // The whole opening tag the class is in: back to its `<a`, forward
        // to the next result's. Attribute values are entity-escaped, so a
        // `>` inside one cannot end the tag early -- but there is no need to
        // find the end exactly, only to stop before the next result.
        let Some(start) = html[..at].rfind("<a") else { continue };
        let end = html[cursor..].find(BING_RESULT).map_or(html.len(), |next| cursor + next);
        let tag = &html[start..end];
        if let Some(found) = bing_result(tag) {
            out.push(found);
        }
    }
    out
}

fn bing_result(tag: &str) -> Option<ImageResult> {
    let meta: Value = serde_json::from_str(&decode_entities(&attribute(tag, "m")?)).ok()?;
    let field = |key: &str| meta.get(key).and_then(Value::as_str).unwrap_or_default().trim();
    let image_url = http_url(field("murl")).ok()?;
    let page_url = http_url(field("purl")).unwrap_or_default();
    let href = attribute(tag, "href").map(|h| decode_entities(&h)).unwrap_or_default();
    Some(ImageResult {
        title: tidy_title(field("t")),
        thumbnail_url: http_url(field("turl")).unwrap_or_default(),
        width: query_number(&href, "expw"),
        height: query_number(&href, "exph"),
        site: site_of(if page_url.is_empty() { &image_url } else { &page_url }),
        image_url,
        page_url,
        source: ImageSource::Bing.slug().to_string(),
    })
}

/// The value of ` name="…"` in a tag, as written (still entity-escaped).
///
/// With the leading space, so `m` is not found at the end of some other
/// attribute's name.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let at = tag.find(&needle)? + needle.len();
    let end = tag[at..].find('"')? + at;
    Some(tag[at..end].to_string())
}

/// A whole number from a query string: `exph=1800` in `…&exph=1800&…`.
fn query_number(href: &str, key: &str) -> Option<u32> {
    let query = href.split_once('?').map_or(href, |(_, q)| q);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| decode_percent(v).parse().ok()).flatten()
    })
}

/// A page title as a caption: Bing marks the words that matched with
/// characters from the private-use area, which no font draws, and a title is
/// one line.
fn tidy_title(title: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c) && !is_invisible(*c))
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `example.com` for `https://www.example.com/a/b?c`.
fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Credentials and a port are not part of whose site it is.
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    host.strip_prefix("www.").unwrap_or(&host).to_string()
}

// ── Wikimedia Commons ────────────────────────────────────────────────────

fn parse_commons(body: &str) -> Result<Vec<ImageResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(pages) = root.pointer("/query/pages").and_then(Value::as_array) else {
        // No `query` at all is what Commons sends for a search that matched
        // nothing.
        return Ok(Vec::new());
    };
    // A generator's pages arrive in no particular order; `index` is the
    // search's own ranking.
    let mut ranked: Vec<(i64, &Value)> = pages
        .iter()
        .map(|p| (p.get("index").and_then(Value::as_i64).unwrap_or(i64::MAX), p))
        .collect();
    ranked.sort_by_key(|(index, _)| *index);

    let mut out = Vec::new();
    for (_, page) in ranked {
        let Some(info) = page.pointer("/imageinfo/0") else { continue };
        let text = |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default().trim();
        // Files are also sound, video and scanned books. Only a picture can
        // be a cover; `thumburl` is a JPEG or PNG rendering even of an SVG
        // or a TIFF, which is why it, and not `url`, is what is kept.
        if !text("mime").starts_with("image/") {
            continue;
        }
        let Ok(image_url) = http_url(&without_tracking(text("thumburl"))) else { continue };
        let thumbnail_url = image_url
            .replace(&format!("/{COMMONS_WIDTH}px-"), &format!("/{COMMONS_THUMB_WIDTH}px-"));
        let named = info
            .pointer("/extmetadata/ObjectName/value")
            .and_then(Value::as_str)
            .map(tidy_title)
            .filter(|t| !t.is_empty());
        let title = named.unwrap_or_else(|| {
            let file = page.get("title").and_then(Value::as_str).unwrap_or_default();
            let file = file.strip_prefix("File:").unwrap_or(file);
            let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            tidy_title(stem)
        });
        let size = |key: &str| info.get(key).and_then(Value::as_u64).map(|n| n as u32);
        out.push(ImageResult {
            title,
            image_url,
            thumbnail_url,
            page_url: http_url(text("descriptionurl")).unwrap_or_default(),
            width: size("thumbwidth"),
            height: size("thumbheight"),
            site: "commons.wikimedia.org".to_string(),
            source: ImageSource::Commons.slug().to_string(),
        });
    }
    Ok(out)
}

/// Wikimedia appends `?utm_source=…` to every address it hands out. The
/// picture is the same without it, and an address kept on an item should
/// not be a campaign's.
fn without_tracking(url: &str) -> String {
    match url.split_once("?utm_") {
        Some((bare, _)) => bare.to_string(),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two results as Bing sent them, the markup around them cut down, and
    /// one result with no picture address that must be skipped.
    const BING: &str = r#"<div class="dgControl"><ul><li><div class="iuscp"><div class="imgpt">
<a aria-label="Image result for dune frank herbert book cover" role="link" class="iusc" style="height:334px;width:194px" m="{&quot;sid&quot;:&quot;&quot;,&quot;cturl&quot;:&quot;&quot;,&quot;cid&quot;:&quot;TDxIRXx0&quot;,&quot;purl&quot;:&quot;https://www.amazon.com/Dune-Frank-Herbert/dp/0441172717&quot;,&quot;murl&quot;:&quot;https://m.media-amazon.com/images/I/81nq+ewtkcL.jpg&quot;,&quot;turl&quot;:&quot;https://ts3.mm.bing.net/th?id=OIP.TDxIRXx07ImIaVFrtUV6QgHaNQ&amp;pid=15.1&quot;,&quot;md5&quot;:&quot;4c3c48457c74ec898869516bb5457a42&quot;,&quot;t&quot;:&quot;Amazon.com: Dune: 0074748335380: Herbert, Frank: Books&quot;,&quot;desc&quot;:&quot;Amazon.com: Dune&quot;}" data-fnvg="1" data-idpos="0.90" href="/images/search?view=detailV2&amp;ccid=TDxIRXx0&amp;mediaurl=https%3a%2f%2fm.media-amazon.com%2fimages%2fI%2f81nq%2bewtkcL.jpg&amp;exph=2560&amp;expw=1430&amp;q=dune+frank+herbert+book+cover&amp;FORM=IRPRST" h="ID=images,5025.1"><div class="img_cont hoff"><img class="mimg" src="https://ts3.mm.bing.net/th?id=OIP.TDxIRXx07ImIaVFrtUV6QgHaNQ&amp;pid=15.1" alt="Dune" /></div></a></div></div></li>
<li><div class="iuscp"><a class="iusc" m="{&quot;purl&quot;:&quot;https://example.org/broken&quot;,&quot;t&quot;:&quot;No picture here&quot;}" href="/images/search?view=detailV2"></a></div></li>
<li><div class="iuscp"><div class="imgpt">
<a aria-label="Image result for dune frank herbert book cover" role="link" class="iusc" style="height:323px;width:194px" m="{&quot;sid&quot;:&quot;&quot;,&quot;cid&quot;:&quot;olQzVl9E&quot;,&quot;purl&quot;:&quot;https://www.shakespeareandcompany.com/books/dune-5&quot;,&quot;murl&quot;:&quot;https://covers.shakespeareandcompany.com/97813996/9781399611176.jpg&quot;,&quot;turl&quot;:&quot;https://ts3.mm.bing.net/th?id=OIP.olQzVl9EUg_dvpOz6FtTQAAAAA&amp;pid=15.1&quot;,&quot;t&quot;:&quot;Dune by Frank Herbert | Shakespeare &amp; Company&quot;}" data-fnvg="1" href="/images/search?view=detailV2&amp;ccid=olQzVl9E&amp;exph=640&amp;expw=382&amp;q=dune" h="ID=images,5103.1"></a></div></div></li></ul></div>"#;

    #[test]
    fn bing_results_are_read_from_each_links_m_attribute() {
        let found = ImageSource::Bing.parse(BING).unwrap();
        assert_eq!(found.len(), 2, "the result with no picture address is skipped: {found:#?}");

        let first = &found[0];
        assert_eq!(first.image_url, "https://m.media-amazon.com/images/I/81nq+ewtkcL.jpg");
        assert_eq!(
            first.thumbnail_url,
            "https://ts3.mm.bing.net/th?id=OIP.TDxIRXx07ImIaVFrtUV6QgHaNQ&pid=15.1",
            "the thumbnail's own `&amp;` is decoded once, with the attribute"
        );
        assert_eq!(first.page_url, "https://www.amazon.com/Dune-Frank-Herbert/dp/0441172717");
        assert_eq!(first.site, "amazon.com");
        assert_eq!(
            first.title, "Amazon.com: Dune: 0074748335380: Herbert, Frank: Books",
            "the match markers are dropped"
        );
        assert_eq!((first.width, first.height), (Some(1430), Some(2560)));
        assert_eq!(first.source, "bing");

        let second = &found[1];
        assert_eq!(second.title, "Dune by Frank Herbert | Shakespeare & Company");
        assert_eq!(second.site, "shakespeareandcompany.com");
        assert_eq!((second.width, second.height), (Some(382), Some(640)));
    }

    #[test]
    fn a_bing_page_with_no_results_or_a_new_layout_reads_as_nothing() {
        assert!(ImageSource::Bing.parse("").unwrap().is_empty());
        assert!(ImageSource::Bing.parse("<html><body>Captcha</body></html>").unwrap().is_empty());
        // A class with no tag around it, and an `m` that is not JSON.
        assert!(ImageSource::Bing.parse(r#"class="iusc""#).unwrap().is_empty());
        let garbled = r#"<a class="iusc" m="{&quot;murl&quot;:" href="/x"></a>"#;
        assert!(ImageSource::Bing.parse(garbled).unwrap().is_empty());
    }

    #[test]
    fn a_bing_picture_that_is_not_a_web_address_is_refused() {
        let tag = r#"<a class="iusc" m="{&quot;murl&quot;:&quot;file:///etc/passwd&quot;}" href="/x"></a>"#;
        assert!(ImageSource::Bing.parse(tag).unwrap().is_empty());
    }

    /// Commons' answer, trimmed: out of order on purpose, as a generator's
    /// pages can be, with a sound file among the pictures.
    const COMMONS: &str = r#"{"batchcomplete":true,"query":{"pages":[
        {"pageid":2,"ns":6,"title":"File:Seattle from Space Needle June 2018 003.jpg","index":2,
         "imageinfo":[{"size":4010101,"width":6000,"height":4000,
           "thumburl":"https://thumb.wikimedia.org/wikipedia/commons/thumb/f/fd/Seattle_from_Space_Needle_June_2018_003.jpg/960px-Seattle_from_Space_Needle_June_2018_003.jpg?utm_source=commons.wikimedia.org&utm_campaign=imageinfo&utm_content=thumbnail",
           "thumbwidth":960,"thumbheight":640,
           "url":"https://upload.wikimedia.org/wikipedia/commons/f/fd/Seattle_from_Space_Needle_June_2018_003.jpg",
           "descriptionurl":"https://commons.wikimedia.org/wiki/File:Seattle_from_Space_Needle_June_2018_003.jpg",
           "mime":"image/jpeg","extmetadata":{}}]},
        {"pageid":1,"ns":6,"title":"File:SpaceNeedleTopClose.jpg","index":1,
         "imageinfo":[{"size":720827,"width":2860,"height":2672,
           "thumburl":"https://thumb.wikimedia.org/wikipedia/commons/thumb/7/77/SpaceNeedleTopClose.jpg/960px-SpaceNeedleTopClose.jpg?utm_source=commons.wikimedia.org&utm_campaign=imageinfo&utm_content=thumbnail",
           "thumbwidth":960,"thumbheight":897,
           "url":"https://upload.wikimedia.org/wikipedia/commons/7/77/SpaceNeedleTopClose.jpg",
           "descriptionurl":"https://commons.wikimedia.org/wiki/File:SpaceNeedleTopClose.jpg",
           "mime":"image/jpeg",
           "extmetadata":{"ObjectName":{"value":"Space Needle, top","source":"commons-desc-page"}}}]},
        {"pageid":3,"ns":6,"title":"File:Space Needle tour.ogg","index":3,
         "imageinfo":[{"thumburl":"https://upload.wikimedia.org/x.ogg","mime":"audio/ogg"}]}
    ]}}"#;

    #[test]
    fn commons_pictures_come_back_in_the_searchs_own_order() {
        let found = ImageSource::Commons.parse(COMMONS).unwrap();
        assert_eq!(found.len(), 2, "the sound file is not a picture: {found:#?}");

        let first = &found[0];
        assert_eq!(first.title, "Space Needle, top", "the file's own name for itself");
        assert_eq!(
            first.image_url,
            "https://thumb.wikimedia.org/wikipedia/commons/thumb/7/77/SpaceNeedleTopClose.jpg/960px-SpaceNeedleTopClose.jpg",
            "the scaled copy, without the campaign tags"
        );
        assert_eq!(
            first.thumbnail_url,
            "https://thumb.wikimedia.org/wikipedia/commons/thumb/7/77/SpaceNeedleTopClose.jpg/330px-SpaceNeedleTopClose.jpg"
        );
        assert_eq!(
            first.page_url,
            "https://commons.wikimedia.org/wiki/File:SpaceNeedleTopClose.jpg"
        );
        assert_eq!((first.width, first.height), (Some(960), Some(897)));
        assert_eq!(first.site, "commons.wikimedia.org");
        assert_eq!(first.source, "commons");

        assert_eq!(
            found[1].title, "Seattle from Space Needle June 2018 003",
            "no name of its own: the file's, without the extension"
        );
    }

    #[test]
    fn a_commons_search_that_found_nothing_is_empty_and_not_an_error() {
        assert!(ImageSource::Commons.parse(r#"{"batchcomplete":true}"#).unwrap().is_empty());
        assert!(ImageSource::Commons.parse("<html>").is_err(), "not JSON at all is an error");
    }

    #[test]
    fn the_addresses_carry_the_words_and_nothing_else() {
        let bing = ImageSource::Bing.url("pad thai & rice", 5).unwrap();
        assert!(bing.starts_with("https://www.bing.com/images/async?q=pad%20thai%20%26%20rice&"));
        assert!(bing.contains("&count=5&"));
        assert!(bing.contains("adlt=moderate"));

        let commons = ImageSource::Commons.url("space needle", 500).unwrap();
        assert!(commons.starts_with("https://commons.wikimedia.org/w/api.php?"));
        assert!(commons.contains("gsrsearch=space%20needle&"));
        assert!(commons.contains(&format!("gsrlimit={MAX_IMAGES}&")), "the limit is capped");

        assert!(ImageSource::Bing.url("   ", 5).is_err());
    }

    #[test]
    fn a_site_is_its_host_without_www_or_a_port() {
        assert_eq!(site_of("https://www.Example.com:8443/a?b"), "example.com");
        assert_eq!(site_of("http://user@cdn.example.org/x.jpg"), "cdn.example.org");
    }
}
