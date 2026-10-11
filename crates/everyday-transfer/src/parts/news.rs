//! The Feed: what you follow, and what you kept from it.
//!
//! ```text
//!   feed/
//!     subscriptions.opml   the web feeds, as every feed reader reads them
//!     sources.json         every source, whole
//!     saved.md             the stories you saved, as a list of links
//! ```
//!
//! # Why the folder is `feed/` and the part is `news`
//!
//! The folder is the one thing in this part somebody will see, and the app is
//! called the Feed. `news` is what the code calls it, because `feed` already
//! means a subscribed calendar everywhere else in the tree.
//!
//! # Two files for one list
//!
//! OPML is what lets you take your subscriptions to any other reader, and it
//! can say four things about each: a name, an address, a site and a folder.
//! A Hacker News list, a newsletter's sender, a mute word, how long stories
//! are kept: those are Every Day's own and have nowhere to go in it. So the
//! OPML is written for the programs that will read it and `sources.json` for
//! this one, and an import reads only the second -- it already holds every
//! source the first does, and reading OPML takes an XML parser, which this
//! crate does not have. (The Feed's own import reads OPML, for lists that
//! come from somewhere else.)
//!
//! # What is not here
//!
//! Stories you did not save. News is not writing you would walk away with:
//! a feed hands it over again whenever it is asked, and a copy of last
//! Tuesday's front page is not a thing anybody means by "my data". What you
//! saved is, so it is written out as a page of links that any Markdown
//! reader opens, and is not read back in either: a link to a story and a
//! story are different things, and a saved story only exists in the vault
//! beside the source that brought it.
//!
//! # What an import will not do on its own
//!
//! It leaves "keep newsletters out of the inbox" off. That switch is the one
//! thing in the Feed that moves mail, and it is off until somebody turns it
//! on; a file that says otherwise is not somebody turning it on. The import
//! says which sources had it, so they can.

use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::news::opml::{self, Outline};
use everyday_core::news::{NewsSource, SourceKind};
use everyday_core::store::JournalStore;
use everyday_core::store::news::{NewsStore, StoryQuery};
use everyday_core::{Error, NewsSourceId, Result, Story};
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub struct NewsPart;
pub static NEWS: NewsPart = NewsPart;

static SPEC: Spec = Spec {
    id: "feed",
    label: "Feed",
    summary: "What you follow — the sites, the Hacker News lists and the newsletter senders — \
              as the subscription list every feed reader opens, and the stories you saved, \
              with their summaries. Only the list comes back in: stories you did not save \
              are not exported, because news is not writing you would walk away with.",
    format: "OPML 2.0, with a JSON sidecar for the rest and a Markdown page of saved stories",
    media: false,
    imports: true,
};

const OPML: &str = "subscriptions.opml";
const SOURCES: &str = "sources.json";
const SAVED: &str = "saved.md";

/// The title of the OPML document, which a reader shows as the name of the
/// list it just imported.
const OPML_TITLE: &str = "Every Day: Feed subscriptions";

/// What `sources.json` holds, as written.
#[derive(serde::Serialize)]
struct Sidecar<'a> {
    sources: &'a [NewsSource],
}

/// The same, as read. Each source is left as a raw value until it is its turn
/// to be read, so one that cannot be understood costs that source and not the
/// file -- a hand-edited list should lose a line, not the other forty.
#[derive(serde::Deserialize)]
struct Unread {
    #[serde(default)]
    sources: Vec<serde_json::Value>,
}

impl Portable for NewsPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(news) = store.news() else { return Ok(None) };
        Ok(Some(news.list_sources()?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(news) = store.news() else { return Ok(()) };
        let sources = in_sidebar_order(news.list_sources()?);
        // Stories only exist beside a source, so a feed with no sources has
        // nothing in it at all and writes nothing.
        if sources.is_empty() {
            return Ok(());
        }

        let outlines: Vec<Outline> = sources.iter().filter_map(outline).collect();
        if !outlines.is_empty() {
            out.text(OPML, opml::write(OPML_TITLE, &outlines))?;
        }

        // A source's icon is a pointer into this vault's blob store, and
        // points at nothing in a file. Left out rather than written as an id
        // that means something only here.
        let portable: Vec<NewsSource> =
            sources.iter().cloned().map(|source| NewsSource { icon: None, ..source }).collect();
        let json = serde_json::to_string_pretty(&Sidecar { sources: &portable })?;
        out.records(SOURCES, format!("{json}\n"), portable.len() as u64)?;

        let saved = saved_stories(news)?;
        if !saved.is_empty() {
            out.records(SAVED, saved_page(&saved, &sources), saved.len() as u64)?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(news) = store.news() else {
            report.problem(SPEC.label, "this vault's backend does not store a feed");
            return Ok(report);
        };
        let Some(text) = src.text(SOURCES) else {
            if src.get(OPML).is_some() {
                report.problem(
                    OPML,
                    "there is no sources.json beside it, so nothing was read; import the OPML \
                     from the Feed itself",
                );
            }
            return Ok(report);
        };
        let unread: Unread = match serde_json::from_str(text) {
            Ok(unread) => unread,
            Err(e) => {
                report.problem(SOURCES, e);
                return Ok(report);
            }
        };

        let mut have = news.list_sources()?;
        for (n, value) in unread.sources.into_iter().enumerate() {
            let source = match serde_json::from_value::<NewsSource>(value) {
                Ok(source) => source,
                Err(e) => {
                    report.problem(SOURCES, format!("source {} could not be read: {e}", n + 1));
                    continue;
                }
            };
            if let Err(e) = read_source(news, source, &mut have, mode, &mut report) {
                report.problem(SOURCES, e);
            }
        }
        Ok(report)
    }
}

/// Unfiled first, then each folder alphabetically; within one, the person's
/// own order and then by name. The sidebar's order, so an export reads the
/// way the app does -- and so two exports of one vault are the same file.
fn in_sidebar_order(mut sources: Vec<NewsSource>) -> Vec<NewsSource> {
    sources.sort_by(|a, b| {
        let (fa, fb) = (a.folder.trim().to_lowercase(), b.folder.trim().to_lowercase());
        (!fa.is_empty())
            .cmp(&!fb.is_empty())
            .then_with(|| fa.cmp(&fb))
            .then_with(|| a.sort_order.cmp(&b.sort_order))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    sources
}

/// A web feed as OPML says it. Hacker News and newsletters are not feeds
/// another reader could fetch, so they are left to the sidecar.
fn outline(source: &NewsSource) -> Option<Outline> {
    let SourceKind::Web { url } = &source.kind else { return None };
    Some(Outline {
        title: source.title.clone(),
        xml_url: url.clone(),
        html_url: source.site_url.clone(),
        folder: source.folder.trim().to_string(),
    })
}

/// Every saved story, newest first, a page at a time.
fn saved_stories(news: &dyn NewsStore) -> Result<Vec<Story>> {
    let mut all = Vec::new();
    let mut before = None;
    loop {
        let page = news.list_stories(&StoryQuery {
            saved_only: true,
            before,
            limit: StoryQuery::MAX_LIMIT,
            ..Default::default()
        })?;
        let empty = page.stories.is_empty();
        all.extend(page.stories);
        match page.next {
            Some(next) if !empty => before = Some(next),
            _ => return Ok(all),
        }
    }
}

/// The saved stories as a list of links, each with where it came from and
/// when, and its summary indented beneath.
///
/// The date is the publisher's, in UTC: the same page from the same vault is
/// the same text wherever it is made.
fn saved_page(stories: &[Story], sources: &[NewsSource]) -> String {
    let names: BTreeMap<NewsSourceId, &str> =
        sources.iter().map(|s| (s.id, s.title.as_str())).collect();
    let mut page = String::from("# Saved stories\n\n");
    for story in stories {
        let source = names.get(&story.source_id).copied().unwrap_or_default();
        let date = story.published_at.to_zoned(jiff::tz::TimeZone::UTC).date();
        let _ = write!(page, "- {}", link(&story.title, &story.url));
        match source.trim() {
            "" => {
                let _ = write!(page, " — {date}");
            }
            source => {
                let _ = write!(page, " — {}, {date}", escape(source));
            }
        }
        if !story.discussion_url.trim().is_empty() && story.discussion_url != story.url {
            let _ = write!(page, " ({})", link("discussion", &story.discussion_url));
        }
        page.push('\n');

        // One line: a summary with a blank line in it would end the list item,
        // and the rest of it would read as a paragraph of its own.
        let summary = story.summary.split_whitespace().collect::<Vec<_>>().join(" ");
        if !summary.is_empty() {
            let _ = writeln!(page, "  {}", unlisted(&escape(&summary)));
        }
    }
    page
}

/// `[title](url)`, or just the title when there is no address -- an Ask HN
/// post, or a newsletter issue that lives in mail.
fn link(title: &str, url: &str) -> String {
    let title = match title.trim() {
        "" => "(untitled)",
        title => title,
    };
    let url = url.trim();
    if url.is_empty() {
        return escape(title);
    }
    // The characters that would end the address early.
    let url = url.replace(' ', "%20").replace('(', "%28").replace(')', "%29");
    format!("[{}]({url})", escape(title))
}

/// Backslash the characters that would turn text into markup it is not.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A line that starts with a list or heading marker would, indented under a
/// list item, become a nested list or a heading. Backslash it.
fn unlisted(line: &str) -> String {
    match line.chars().next() {
        Some('-' | '+' | '*' | '>' | '#') => format!("\\{line}"),
        _ => line.to_string(),
    }
}

/// Why a source from a file cannot be followed, if it cannot. The rules the
/// vault holds a source to when it is saved through the app, restated here
/// because an import writes to the store directly.
fn unusable(source: &NewsSource) -> Option<&'static str> {
    if source.title.trim().is_empty() {
        return Some("a source needs a name");
    }
    match &source.kind {
        SourceKind::Web { url } => {
            let url = url.trim().to_ascii_lowercase();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Some("a feed's address must start with https:// or http://");
            }
        }
        SourceKind::HackerNews { .. } => {}
        SourceKind::Mail { sender } => {
            if !sender.contains('@') {
                return Some("a newsletter source needs the sender's address");
            }
        }
    }
    if source.retention.is_some_and(|r| r.read_days == 0 || r.unread_days == 0) {
        return Some("stories must be kept for at least a day");
    }
    None
}

/// Is this the same thing to follow? An address is compared with its host in
/// lower case and a final slash ignored, a sender without regard to case, and
/// a Hacker News list by which list and how many points.
fn same_feed(a: &SourceKind, b: &SourceKind) -> bool {
    fn address(url: &str) -> String {
        let url = url.trim().trim_end_matches('/');
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        match rest.split_once('/') {
            Some((host, path)) => format!("{}/{path}", host.to_ascii_lowercase()),
            None => rest.to_ascii_lowercase(),
        }
    }
    match (a, b) {
        (SourceKind::Web { url: a }, SourceKind::Web { url: b }) => address(a) == address(b),
        (SourceKind::Mail { sender: a }, SourceKind::Mail { sender: b }) => {
            a.trim().eq_ignore_ascii_case(b.trim())
        }
        (a, b) => a == b,
    }
}

/// Land one source from the file.
///
/// A source whose id is already here is left as it is, or overwritten from
/// the file under [`Mode::Replace`]. One whose id is new but which follows
/// something already followed is not added a second time: the same feed
/// twice in a sidebar is a mistake nobody asked for, and the two would only
/// fetch the same stories.
fn read_source(
    news: &dyn NewsStore,
    mut source: NewsSource,
    have: &mut Vec<NewsSource>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    if let Some(why) = unusable(&source) {
        report.problem(SOURCES, format!("{}: {why}", source.title.trim()));
        return Ok(());
    }
    source.icon = None;
    // Said once the source has actually landed: a source left alone because
    // it was already here has had nothing switched off.
    let switched_off = std::mem::take(&mut source.keep_out_of_inbox)
        && matches!(source.kind, SourceKind::Mail { .. });

    let existing = match news.get_source(source.id) {
        Ok(existing) => Some(existing),
        Err(Error::NotFound { .. }) => None,
        Err(e) => return Err(e),
    };
    if existing.is_none()
        && have.iter().any(|other| other.id != source.id && same_feed(&other.kind, &source.kind))
    {
        report.skipped += 1;
        return Ok(());
    }

    let mut landing = land(existing, || source.clone(), mode);
    let Some(target) = landing.as_mut() else {
        report.landed(&landing);
        return Ok(());
    };
    *target = source;
    news.put_source(target)?;
    have.retain(|other| other.id != target.id);
    have.push(target.clone());
    if switched_off {
        report.problem(
            SOURCES,
            format!(
                "{}: arrives with \"keep out of the inbox\" off; turn it on in the Feed if you \
                 want it",
                target.title.trim()
            ),
        );
    }
    report.landed(&landing);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::news::{HnList, Retention};

    fn web(title: &str, url: &str) -> NewsSource {
        NewsSource::new(SourceKind::Web { url: url.into() }, title)
    }

    #[test]
    fn sources_are_written_in_the_order_the_sidebar_draws_them() {
        let mut tech = web("b", "https://b.example/feed");
        tech.folder = "Tech".into();
        let mut art = web("y", "https://y.example/feed");
        art.folder = "art".into();
        let mut first = web("a", "https://a.example/feed");
        first.folder = "Tech".into();
        let sorted = in_sidebar_order(vec![tech, art, web("z", "https://z.example/feed"), first]);
        let titles: Vec<&str> = sorted.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["z", "y", "a", "b"], "unfiled, then art, then Tech by name");
    }

    #[test]
    fn only_a_web_feed_has_an_outline() {
        let mut blog = web("Blog", "https://blog.example/feed");
        blog.site_url = "https://blog.example".into();
        blog.folder = " Tech ".into();
        assert_eq!(
            outline(&blog),
            Some(Outline {
                title: "Blog".into(),
                xml_url: "https://blog.example/feed".into(),
                html_url: "https://blog.example".into(),
                folder: "Tech".into(),
            })
        );
        let hn =
            NewsSource::new(SourceKind::HackerNews { list: HnList::Front, min_points: 0 }, "HN");
        let mail = NewsSource::new(SourceKind::Mail { sender: "a@b.example".into() }, "Weekly");
        assert_eq!(outline(&hn), None);
        assert_eq!(outline(&mail), None);
    }

    #[test]
    fn a_link_cannot_be_broken_by_what_is_in_it() {
        assert_eq!(
            link("Why [x] matters", "https://x.example/a"),
            "[Why \\[x\\] matters](https://x.example/a)"
        );
        assert_eq!(
            link("Page", "https://x.example/a (1)"),
            "[Page](https://x.example/a%20%281%29)"
        );
        assert_eq!(link("Ask HN: anyone?", ""), "Ask HN: anyone?");
        assert_eq!(link("  ", "https://x.example/"), "[(untitled)](https://x.example/)");
    }

    #[test]
    fn a_summary_is_one_indented_line_that_cannot_open_a_list() {
        let source = web("Blog", "https://blog.example/feed");
        let published = "2026-09-06T12:00:00Z".parse().unwrap();
        let mut story = Story::new(source.id, "k", "Why Rust", published);
        story.url = "https://blog.example/why-rust".into();
        story.summary = "- not a list\n\nbut two\nparagraphs".into();
        story.discussion_url = "https://news.example/item?id=1".into();

        let page = saved_page(&[story], &[source]);
        assert_eq!(
            page,
            "# Saved stories\n\n\
             - [Why Rust](https://blog.example/why-rust) — Blog, 2026-09-06 \
             ([discussion](https://news.example/item?id=1))\n  \
             \\- not a list but two paragraphs\n"
        );
    }

    #[test]
    fn a_source_from_a_file_is_held_to_the_rules_the_app_holds_it_to() {
        assert_eq!(unusable(&web("Blog", "https://blog.example/feed")), None);
        assert!(unusable(&web("", "https://blog.example/feed")).is_some());
        assert!(unusable(&web("Blog", "ftp://blog.example/feed")).is_some());
        let mail = NewsSource::new(SourceKind::Mail { sender: "nobody".into() }, "Weekly");
        assert!(unusable(&mail).is_some());
        let mut keeps = web("Blog", "https://blog.example/feed");
        keeps.retention = Some(Retention { read_days: 0, unread_days: 90 });
        assert!(unusable(&keeps).is_some());
    }

    #[test]
    fn the_same_feed_in_two_spellings_is_one_feed() {
        let web = |url: &str| SourceKind::Web { url: url.into() };
        assert!(same_feed(&web("https://Blog.example/feed/"), &web("https://blog.example/feed")));
        assert!(!same_feed(&web("https://blog.example/Feed"), &web("https://blog.example/feed")));
        let top = |points| SourceKind::HackerNews { list: HnList::Top, min_points: points };
        assert!(same_feed(&top(300), &top(300)));
        assert!(!same_feed(&top(300), &top(100)));
        let mail = |sender: &str| SourceKind::Mail { sender: sender.into() };
        assert!(same_feed(&mail("News@Example.com"), &mail("news@example.com")));
        assert!(!same_feed(&mail("news@example.com"), &top(300)));
    }
}
