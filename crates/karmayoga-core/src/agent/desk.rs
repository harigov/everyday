//! What the assistant may do on the computer somebody is sitting at, decided
//! with no desktop anywhere near it.
//!
//! The actions themselves -- opening a link in their browser, pausing the
//! music, a timer ringing -- belong to the shell that owns the desktop (the
//! `Desk` in `karmayoga_service::desk`, and the app's implementation of it).
//! What is here is every part of them a test can settle offline: which
//! website "search Amazon for" means and the address that search opens,
//! which installed application "open Files" names, and when "a timer for
//! twenty minutes" or "an alarm at seven" rings. See `docs/plans/desk.md`.

use crate::error::{Error, Result};
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

/// Where `{query}` goes in a [`SearchSite`]'s address.
pub const QUERY: &str = "{query}";

/// The most search sites somebody may add of their own.
pub const MAX_SEARCH_SITES: usize = 50;

/// The longest a search site's name may be: something said out loud, "my
/// company wiki", not a sentence.
pub const MAX_SITE_NAME_CHARS: usize = 40;

/// The longest a search site's address may be.
pub const MAX_SITE_URL_CHARS: usize = 2_000;

/// The longest a query may be once encoded into an address. Past this a
/// search engine refuses it anyway, and a model that wants to put a page of
/// somebody's journal into an address is doing something other than
/// searching.
pub const MAX_QUERY_CHARS: usize = 500;

/// A website that can be searched by putting words into its address:
/// "Amazon" and `https://www.amazon.com/s?k={query}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSite {
    /// What somebody calls it: "Amazon", "Goodreads", "the wiki".
    pub name: String,
    /// The address of its results, with [`QUERY`] where the words go.
    pub url: String,
}

/// The sites every assistant can search without being told how, with the
/// other names people use for them. Somebody's own site of the same name
/// replaces one of these -- an Amazon of their own country, say -- see
/// [`all_sites`].
const BUILT_IN: &[(&str, &str, &[&str])] = &[
    ("Google", "https://www.google.com/search?q={query}", &["google search"]),
    (
        "Google Images",
        "https://www.google.com/search?tbm=isch&q={query}",
        &["images", "image search", "pictures"],
    ),
    ("Google Maps", "https://www.google.com/maps/search/{query}", &["maps", "map"]),
    ("YouTube", "https://www.youtube.com/results?search_query={query}", &[]),
    ("Amazon", "https://www.amazon.com/s?k={query}", &[]),
    ("Wikipedia", "https://en.wikipedia.org/w/index.php?search={query}", &["wiki"]),
    ("DuckDuckGo", "https://duckduckgo.com/?q={query}", &["duck duck go", "ddg"]),
    ("GitHub", "https://github.com/search?q={query}", &[]),
    ("IMDb", "https://www.imdb.com/find/?q={query}", &[]),
    ("Goodreads", "https://www.goodreads.com/search?q={query}", &[]),
    ("eBay", "https://www.ebay.com/sch/i.html?_nkw={query}", &[]),
];

/// Every site there is to search: somebody's own first, in their order, then
/// the built-in ones they have not replaced with one of the same name.
pub fn all_sites(own: &[SearchSite]) -> Vec<SearchSite> {
    let mut sites: Vec<SearchSite> = own.to_vec();
    for (name, url, _) in BUILT_IN {
        if !own.iter().any(|site| same_name(&site.name, name)) {
            sites.push(SearchSite { name: (*name).to_string(), url: (*url).to_string() });
        }
    }
    sites
}

/// The site `asked` names among `sites`, or `None`. Forgiving the way a
/// spoken name has to be: any case, "the" in front, "amazon.com" for Amazon,
/// "maps" for Google Maps -- an alias counts only for the built-in site it
/// belongs to, and only while that site is not replaced by one of
/// somebody's own.
pub fn find_site<'a>(sites: &'a [SearchSite], asked: &str) -> Option<&'a SearchSite> {
    let asked = spoken_name(asked);
    if asked.is_empty() {
        return None;
    }
    if let Some(site) = sites.iter().find(|site| spoken_name(&site.name) == asked) {
        return Some(site);
    }
    // "amazon.com", "www.youtube.com": the site whose address is on it.
    if let Some(site) = sites.iter().find(|site| host_of(&site.url).is_some_and(|h| h == asked)) {
        return Some(site);
    }
    let bare = asked.split('.').next().unwrap_or(&asked).to_string();
    if let Some(site) = sites.iter().find(|site| spoken_name(&site.name) == bare) {
        return Some(site);
    }
    BUILT_IN.iter().find_map(|(name, url, aliases)| {
        aliases
            .iter()
            .any(|alias| *alias == asked)
            .then(|| sites.iter().find(|site| same_name(&site.name, name) && site.url == *url))?
    })
}

/// The address that searches `site` for `query`: the words percent-encoded
/// into [`QUERY`], so that they are a search and never a second parameter or
/// a different path.
pub fn search_url(site: &SearchSite, query: &str) -> Result<String> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::Invalid("there is nothing to search for".into()));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(Error::Invalid(format!(
            "a search is at most {MAX_QUERY_CHARS} characters; that is {}",
            query.chars().count()
        )));
    }
    let encoded = encode(query);
    let url = site.url.replace(QUERY, &encoded);
    url::Url::parse(&url)
        .map_err(|e| Error::Invalid(format!("{}'s address does not work: {e}", site.name)))?;
    Ok(url)
}

/// Check somebody's own search sites before they are saved: a name to say,
/// an `http` or `https` address with [`QUERY`] in it, no two of the same
/// name.
pub fn validate_sites(own: &[SearchSite]) -> Result<()> {
    if own.len() > MAX_SEARCH_SITES {
        return Err(Error::Invalid(format!("at most {MAX_SEARCH_SITES} search sites")));
    }
    for (i, site) in own.iter().enumerate() {
        let name = site.name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("a search site needs a name".into()));
        }
        if name.chars().count() > MAX_SITE_NAME_CHARS || name.contains(['\n', '\r']) {
            return Err(Error::Invalid(format!(
                "a search site's name is one line of at most {MAX_SITE_NAME_CHARS} characters"
            )));
        }
        if own[..i].iter().any(|other| same_name(&other.name, name)) {
            return Err(Error::Invalid(format!("there are two search sites called {name}")));
        }
        let url = site.url.trim();
        if url.chars().count() > MAX_SITE_URL_CHARS {
            return Err(Error::Invalid(format!("{name}'s address is too long")));
        }
        if !url.contains(QUERY) {
            return Err(Error::Invalid(format!(
                "{name}'s address needs {QUERY} where the words of a search go"
            )));
        }
        let parsed = url::Url::parse(&url.replace(QUERY, "test"))
            .map_err(|_| Error::Invalid(format!("{name}'s address is not a web address")))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(Error::Invalid(format!(
                "{name}'s address must start with https:// (or http://)"
            )));
        }
    }
    Ok(())
}

/// Every character but the unreserved ones as `%XX` of its UTF-8 bytes --
/// a space included, as `%20` rather than `+`, which is right in a path as
/// well as in a query.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A name as compared: lower-case, single-spaced, without a leading "the"
/// or "my", and without `www.`.
fn spoken_name(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let words: Vec<&str> = lowered.split_whitespace().collect();
    let words = match words.as_slice() {
        ["the" | "my", rest @ ..] if !rest.is_empty() => rest,
        all => all,
    };
    let joined = words.join(" ");
    joined.strip_prefix("www.").map(str::to_string).unwrap_or(joined)
}

fn same_name(a: &str, b: &str) -> bool {
    spoken_name(a) == spoken_name(b)
}

/// The host of a site's address, without `www.`.
fn host_of(template: &str) -> Option<String> {
    let parsed = url::Url::parse(&template.replace(QUERY, "x")).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    Some(host.strip_prefix("www.").map(str::to_string).unwrap_or(host))
}

/// An application installed on this computer, as the desktop lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    /// What launches it: on Linux its desktop file's id, `org.gnome.Nautilus.desktop`.
    pub id: String,
    /// What it is called: "Files".
    pub name: String,
    /// Other words it answers to, from its desktop file: "folder", "explorer".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

/// What [`match_app`] made of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMatch<'a> {
    One(&'a InstalledApp),
    /// More than one fits equally well: these, best first, at most
    /// [`MOST_APPS_OFFERED`].
    Several(Vec<&'a InstalledApp>),
    None,
}

/// The most applications a "which one?" answer lists.
pub const MOST_APPS_OFFERED: usize = 8;

/// The application `asked` names. Tiers rather than a distance, as the
/// command bar ranks its actions: the whole name beats a name that starts
/// with the words, which beats a word inside a name, which beats a keyword.
/// The first tier that has anything decides; one there is the answer, more
/// than one is a question.
pub fn match_app<'a>(apps: &'a [InstalledApp], asked: &str) -> AppMatch<'a> {
    let asked = spoken_name(asked);
    let asked = asked.strip_suffix(" app").unwrap_or(&asked).trim().to_string();
    if asked.is_empty() {
        return AppMatch::None;
    }
    let tiers: [&dyn Fn(&InstalledApp) -> bool; 4] = [
        &|app| spoken_name(&app.name) == asked,
        &|app| spoken_name(&app.name).starts_with(&asked),
        &|app| {
            let name = spoken_name(&app.name);
            name.split(|c: char| !c.is_alphanumeric()).any(|word| word == asked)
                || name.contains(&format!(" {asked}"))
        },
        &|app| app.keywords.iter().any(|k| spoken_name(k) == asked),
    ];
    for fits in tiers {
        let mut found: Vec<&InstalledApp> = Vec::new();
        for app in apps.iter().filter(|app| fits(app)) {
            // Two desktop files for one application -- a Flatpak and a
            // package -- are one answer, not a question.
            if !found.iter().any(|f| same_name(&f.name, &app.name)) {
                found.push(app);
            }
        }
        match found.len() {
            0 => continue,
            1 => return AppMatch::One(found[0]),
            _ => {
                found.truncate(MOST_APPS_OFFERED);
                return AppMatch::Several(found);
            }
        }
    }
    AppMatch::None
}

/// The longest a timer may run: a day. Past that it is a reminder, which
/// outlives the app being closed, and a task's `remind_at` is the place for
/// it.
pub const LONGEST_TIMER: SignedDuration = SignedDuration::from_hours(24);

/// The shortest: a timer for no time at all is a slip.
pub const SHORTEST_TIMER: SignedDuration = SignedDuration::from_secs(1);

/// When a timer set at `now` rings: after `length`, or at the next `at` --
/// a 24-hour `"HH:MM"` in `now`'s zone, today if it is still ahead and
/// tomorrow if not. Exactly one of the two. Refused past
/// [`LONGEST_TIMER`].
pub fn timer_ends(
    now: &Zoned,
    length: Option<SignedDuration>,
    at: Option<&str>,
) -> Result<Timestamp> {
    let ends = match (length, at.map(str::trim).filter(|a| !a.is_empty())) {
        (Some(_), Some(_)) => {
            return Err(Error::Invalid("give a length or a time to ring at, not both".into()));
        }
        (None, None) => {
            return Err(Error::Invalid("give a length or a time to ring at".into()));
        }
        (Some(length), None) => {
            if length < SHORTEST_TIMER {
                return Err(Error::Invalid("a timer has to run for at least a second".into()));
            }
            if length > LONGEST_TIMER {
                return Err(too_long());
            }
            now.timestamp()
                .checked_add(length)
                .map_err(|e| Error::Invalid(format!("that is too far ahead: {e}")))?
        }
        (None, Some(at)) => {
            let time = super::parse_clock(at).ok_or_else(|| {
                Error::Invalid(format!("{at:?} is not a time of day as HH:MM, 24-hour"))
            })?;
            let zone = now.time_zone().clone();
            let today = now
                .date()
                .to_datetime(time)
                .to_zoned(zone.clone())
                .map_err(|e| Error::Invalid(e.to_string()))?;
            let at = if today > *now {
                today
            } else {
                let tomorrow = now.date().tomorrow().map_err(|e| Error::Invalid(e.to_string()))?;
                tomorrow
                    .to_datetime(time)
                    .to_zoned(zone)
                    .map_err(|e| Error::Invalid(e.to_string()))?
            };
            at.timestamp()
        }
    };
    Ok(ends)
}

fn too_long() -> Error {
    Error::Invalid(
        "a timer runs for at most a day, and only while the app is open; for anything further \
         off, set a reminder on a task"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own(name: &str, url: &str) -> SearchSite {
        SearchSite { name: name.into(), url: url.into() }
    }

    #[test]
    fn a_built_in_site_is_found_by_name_alias_or_address() {
        let sites = all_sites(&[]);
        assert_eq!(find_site(&sites, "amazon").unwrap().name, "Amazon");
        assert_eq!(find_site(&sites, "  The Amazon ").unwrap().name, "Amazon");
        assert_eq!(find_site(&sites, "amazon.com").unwrap().name, "Amazon");
        assert_eq!(find_site(&sites, "www.youtube.com").unwrap().name, "YouTube");
        assert_eq!(find_site(&sites, "maps").unwrap().name, "Google Maps");
        assert_eq!(find_site(&sites, "wiki").unwrap().name, "Wikipedia");
        assert!(find_site(&sites, "nowhere").is_none());
        assert!(find_site(&sites, " ").is_none());
    }

    #[test]
    fn a_site_of_their_own_replaces_the_built_in_one_of_the_same_name() {
        let sites = all_sites(&[own("amazon", "https://www.amazon.in/s?k={query}")]);
        let amazon = find_site(&sites, "Amazon").unwrap();
        assert!(amazon.url.contains("amazon.in"));
        assert_eq!(sites.iter().filter(|s| same_name(&s.name, "Amazon")).count(), 1);
        // Their own comes first, the built-in ones after.
        assert_eq!(sites[0].name, "amazon");
        assert_eq!(sites[1].name, "Google");
    }

    #[test]
    fn an_alias_belongs_to_the_built_in_site_only() {
        let sites = all_sites(&[own("Google Maps", "https://maps.example/?q={query}")]);
        // "maps" was the built-in Google Maps's alias; theirs is found by its
        // own name, not by the alias.
        assert!(find_site(&sites, "maps").is_none());
        assert!(find_site(&sites, "google maps").unwrap().url.contains("maps.example"));
    }

    #[test]
    fn the_query_is_encoded_into_the_address_and_nothing_else() {
        let sites = all_sites(&[]);
        let amazon = find_site(&sites, "amazon").unwrap();
        assert_eq!(
            search_url(amazon, " noise cancelling headphones ").unwrap(),
            "https://www.amazon.com/s?k=noise%20cancelling%20headphones"
        );
        // A second parameter, a fragment or a path cannot be smuggled in.
        assert_eq!(
            search_url(amazon, "a&tag=evil#x/../y").unwrap(),
            "https://www.amazon.com/s?k=a%26tag%3Devil%23x%2F..%2Fy"
        );
        assert_eq!(
            search_url(find_site(&sites, "maps").unwrap(), "café").unwrap(),
            "https://www.google.com/maps/search/caf%C3%A9"
        );
        assert!(search_url(amazon, "  ").is_err());
        assert!(search_url(amazon, &"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }

    #[test]
    fn their_own_sites_are_checked_before_they_are_kept() {
        assert!(validate_sites(&[own("Wiki", "https://wiki.example/search?q={query}")]).is_ok());
        assert!(validate_sites(&[own("", "https://a.example/?q={query}")]).is_err());
        assert!(validate_sites(&[own("No query", "https://a.example/")]).is_err());
        assert!(validate_sites(&[own("Script", "javascript:alert({query})")]).is_err());
        assert!(validate_sites(&[own("File", "file:///etc/{query}")]).is_err());
        assert!(validate_sites(&[own("Two\nlines", "https://a.example/?q={query}")]).is_err());
        assert!(
            validate_sites(&[
                own("Wiki", "https://a.example/?q={query}"),
                own("the wiki", "https://b.example/?q={query}"),
            ])
            .is_err()
        );
    }

    fn app(id: &str, name: &str, keywords: &[&str]) -> InstalledApp {
        InstalledApp {
            id: id.into(),
            name: name.into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    #[test]
    fn an_app_is_found_by_its_name_then_its_words_then_its_keywords() {
        let apps = [
            app("org.gnome.Nautilus.desktop", "Files", &["folder", "explorer"]),
            app("firefox.desktop", "Firefox Web Browser", &["internet"]),
            app("google-chrome.desktop", "Google Chrome", &["browser"]),
            app("org.gnome.Calculator.desktop", "Calculator", &[]),
            app("org.gnome.Calendar.desktop", "Calendar", &[]),
        ];
        assert_eq!(match_app(&apps, "files"), AppMatch::One(&apps[0]));
        assert_eq!(match_app(&apps, "the Files app"), AppMatch::One(&apps[0]));
        assert_eq!(match_app(&apps, "firefox"), AppMatch::One(&apps[1]));
        assert_eq!(match_app(&apps, "chrome"), AppMatch::One(&apps[2]));
        assert_eq!(match_app(&apps, "explorer"), AppMatch::One(&apps[0]));
        assert_eq!(match_app(&apps, "calc"), AppMatch::One(&apps[3]));
        assert_eq!(match_app(&apps, "cal"), AppMatch::Several(vec![&apps[3], &apps[4]]));
        assert_eq!(match_app(&apps, "spreadsheet"), AppMatch::None);
    }

    #[test]
    fn two_desktop_files_for_one_app_are_one_answer() {
        let apps = [
            app("org.mozilla.firefox.desktop", "Firefox", &[]),
            app("firefox_firefox.desktop", "Firefox", &[]),
        ];
        assert_eq!(match_app(&apps, "firefox"), AppMatch::One(&apps[0]));
    }

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn a_timer_rings_after_its_length_or_at_the_next_such_time() {
        let now = at("2026-11-24T14:00:00-08:00[America/Los_Angeles]");
        assert_eq!(
            timer_ends(&now, Some(SignedDuration::from_mins(20)), None).unwrap(),
            at("2026-11-24T14:20:00-08:00[America/Los_Angeles]").timestamp()
        );
        assert_eq!(
            timer_ends(&now, None, Some("15:30")).unwrap(),
            at("2026-11-24T15:30:00-08:00[America/Los_Angeles]").timestamp()
        );
        // Already past today: tomorrow.
        assert_eq!(
            timer_ends(&now, None, Some("07:00")).unwrap(),
            at("2026-11-25T07:00:00-08:00[America/Los_Angeles]").timestamp()
        );
        // Now exactly is past, too: nobody sets an alarm for this minute.
        assert_eq!(
            timer_ends(&now, None, Some("14:00")).unwrap(),
            at("2026-11-25T14:00:00-08:00[America/Los_Angeles]").timestamp()
        );
    }

    #[test]
    fn a_timer_needs_one_of_a_length_or_a_time_and_at_most_a_day() {
        let now = at("2026-11-24T14:00:00-08:00[America/Los_Angeles]");
        let minute = Some(SignedDuration::from_mins(1));
        assert!(timer_ends(&now, None, None).is_err());
        assert!(timer_ends(&now, minute, Some("15:00")).is_err());
        assert!(timer_ends(&now, Some(SignedDuration::ZERO), None).is_err());
        assert!(timer_ends(&now, Some(SignedDuration::from_hours(25)), None).is_err());
        assert!(timer_ends(&now, None, Some("3pm")).is_err());
        assert!(timer_ends(&now, None, Some("25:00")).is_err());
    }

    #[test]
    fn an_alarm_across_a_clock_change_rings_at_the_wall_clock_time() {
        // The night the clocks go back: 07:00 the next morning is nine hours
        // of wall clock and ten of real time away.
        let now = at("2026-10-31T22:00:00-07:00[America/Los_Angeles]");
        assert_eq!(
            timer_ends(&now, None, Some("07:00")).unwrap(),
            at("2026-11-01T07:00:00-08:00[America/Los_Angeles]").timestamp()
        );
    }
}
