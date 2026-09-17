//! Searching the web, and everything that happens to the answer.
//!
//! This is the whole of web search except the socket. It builds the request,
//! it parses the response, it normalises eight wildly different reply formats
//! into one [`SearchResult`], and it maps that result onto an
//! [`Item`](crate::library::Item). What it does not do — cannot do, by
//! construction — is open a connection: [`everyday_core`](crate) has no async
//! runtime and no TLS stack, and the library feature is built so that stays
//! true. The shell hands bytes in; everything else is here, and therefore
//! under test, offline, deterministically.
//!
//! That split is the same one [`crate::ics`] makes for calendar feeds, and it
//! is why "the metadata lookup got the year wrong" is a test to write rather
//! than a network trace to capture.
//!
//! # The shape any code in the app can use
//!
//! ```
//! use everyday_core::websearch::{Fetcher, Request, SearchRequest, Source, WebSearch};
//! # use everyday_core::Result;
//!
//! struct MyFetcher;
//! impl Fetcher for MyFetcher {
//!     fn get(&self, _request: &Request) -> Result<String> {
//!         Ok(r#"{"query":{"pages":{}}}"#.to_string())
//!     }
//! }
//!
//! let web = WebSearch::new(MyFetcher);
//! let hits = web.search(&SearchRequest::new("dune").on(Source::Wikipedia))?;
//! # Ok::<(), everyday_core::Error>(())
//! ```
//!
//! One trait with one method is the entire contract. Anything that can turn
//! a URL into a string — the desktop shell's HTTP client, the CLI, a test
//! stub, a future mobile shell — is a [`Fetcher`], and everything downstream
//! of it is shared. See `everyday_app::websearch` for the real one and
//! `commands::web_search` for the command that exposes it to the interface.
//!
//! # Which sources, and why these
//!
//! Every source here works without an API key, without an account, and
//! without a client id registered to a vendor. That constraint is not
//! incidental: this is an application people build themselves and run
//! offline by default, and a metadata feature that stops working the day a
//! free tier changes is a feature that should not have shipped. The trade is
//! that [`Source::Web`] scrapes an HTML results page, which is the one part
//! of this file that can be broken by somebody else's redesign —
//! [`parse_duckduckgo`] is written to return nothing rather than nonsense
//! when that happens.
//!
//! It is also why the obvious names are missing. The best film database on
//! the internet is TMDB and the best games database is RAWG, and both are a
//! free account and a key in a settings box away; the shelves they would
//! serve are answered here by a catalogue that needs neither — iTunes for
//! films, [`Source::TvMaze`] for television, [`Source::MusicBrainz`] for
//! records, [`Source::Steam`] for games — with Wikipedia behind every one of
//! them for what a catalogue does not carry. A source that needs a key is a
//! source that is off until somebody sets it up, and this feature is meant
//! to work on the first run.
//!
//! # What leaves the machine
//!
//! A query string, to one host, when you press the button. There is no
//! background enrichment, nothing is looked up as you type unless you ask,
//! and no identifier of yours is sent — see [`Source::request`], which builds
//! every URL in this file and adds nothing to them.

use crate::error::{Error, Result};
use crate::library::{ExternalRating, Item, Kind, Link, normalize_rating};
use serde_json::Value;
use std::collections::BTreeMap;

/// Largest response any of this will parse, in bytes.
///
/// Enforced by the fetcher, stated here because it is a property of the
/// search feature rather than of whichever client implements it. Generous
/// for a results page and small enough that a server which never stops
/// sending is disconnected rather than believed.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Largest cover image accepted. A poster is a few hundred kilobytes; ten
/// megabytes is somebody else's problem arriving in your vault.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// How many results a lookup asks for by default. Enough to recognise the
/// right one in a grid, few enough to read at a glance.
pub const DEFAULT_LIMIT: u32 = 8;

// ── Sources ──────────────────────────────────────────────────────────────

/// Where an answer is being looked for.
///
/// A [`Kind`](crate::library::Kind) names one of these by slug, which is why
/// [`from_slug`](Self::from_slug) never fails: a kind is a record in the
/// vault, possibly written by a newer build, and an unrecognised source
/// should degrade to a plain web search rather than make the shelf
/// unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// A general web search. What a recipe or an article gets, and the
    /// fallback for a kind that names nothing better.
    #[default]
    Web,
    /// Wikipedia's search API: a summary, a thumbnail and an address. The
    /// best general-purpose answer for anything with an article — games,
    /// people, buildings.
    Wikipedia,
    /// Open Library. Books, with covers, page counts, publishers, ISBNs and
    /// an aggregate rating.
    OpenLibrary,
    /// The iTunes Search API. Films, television, albums and podcasts, with
    /// artwork at a usable size.
    ///
    /// Renamed explicitly rather than left to `camelCase`, which would spell
    /// it `iTunes` and put the wire form out of step with
    /// [`slug`](Self::slug) — and the slug is what a `Kind` stores.
    #[serde(rename = "itunes")]
    ITunes,
    /// TVmaze. Television, and better at it than a shop is: the network,
    /// the genres, the year it began and a portrait, for shows nobody sells
    /// a season of.
    #[serde(rename = "tvmaze")]
    TvMaze,
    /// MusicBrainz, with covers from the Cover Art Archive. Records, as a
    /// catalogue rather than a shop — the original release year rather than
    /// the year of the remaster somebody is currently selling.
    MusicBrainz,
    /// Steam's store search. Games, with the poster art the store holds at a
    /// predictable address. Personal computers only, which is why
    /// [`SearchRequest::attempts`] still has Wikipedia behind it: a console
    /// exclusive is not in this catalogue and never will be.
    Steam,
    /// OpenStreetMap's geocoder. Restaurants and places, with an address and
    /// often a cuisine and a telephone number.
    Nominatim,
}

impl Source {
    pub const ALL: [Source; 8] = [
        Source::Web,
        Source::Wikipedia,
        Source::OpenLibrary,
        Source::ITunes,
        Source::TvMaze,
        Source::MusicBrainz,
        Source::Steam,
        Source::Nominatim,
    ];

    /// The stable name a [`Kind`](crate::library::Kind) stores.
    pub fn slug(self) -> &'static str {
        match self {
            Source::Web => "web",
            Source::Wikipedia => "wikipedia",
            Source::OpenLibrary => "openLibrary",
            Source::ITunes => "itunes",
            Source::TvMaze => "tvmaze",
            Source::MusicBrainz => "musicBrainz",
            Source::Steam => "steam",
            Source::Nominatim => "nominatim",
        }
    }

    /// What the interface calls it, and what is recorded in
    /// [`ExternalRating::source`].
    pub fn label(self) -> &'static str {
        match self {
            Source::Web => "the web",
            Source::Wikipedia => "Wikipedia",
            Source::OpenLibrary => "Open Library",
            Source::ITunes => "iTunes",
            Source::TvMaze => "TVmaze",
            Source::MusicBrainz => "MusicBrainz",
            Source::Steam => "Steam",
            Source::Nominatim => "OpenStreetMap",
        }
    }

    /// Resolve a slug, treating anything unrecognised as a plain web search.
    ///
    /// Deliberately total. See the type's docs: the input comes out of the
    /// vault, and an older build meeting a newer kind must keep working.
    pub fn from_slug(slug: &str) -> Self {
        match slug.trim() {
            "wikipedia" => Source::Wikipedia,
            "openLibrary" | "openlibrary" => Source::OpenLibrary,
            "itunes" | "iTunes" => Source::ITunes,
            "tvmaze" | "tvMaze" => Source::TvMaze,
            "musicBrainz" | "musicbrainz" => Source::MusicBrainz,
            "steam" => Source::Steam,
            "nominatim" => Source::Nominatim,
            _ => Source::Web,
        }
    }

    /// Does this source usually come back with a picture?
    ///
    /// Read by the interface to decide whether a shelf's cards should reserve
    /// room for artwork before anything has been fetched, so the grid does
    /// not reflow under the cursor when the first cover lands.
    pub fn has_images(self) -> bool {
        !matches!(self, Source::Web | Source::Nominatim)
    }

    /// Is this a catalogue of *works* — a thing a shelf collects — rather
    /// than a search of pages or of the ground?
    ///
    /// The one place it matters is [`SearchRequest::attempts`], which puts
    /// Wikipedia behind a catalogue and nothing behind the other two. See
    /// that method for the argument; it is written once here so that adding
    /// a catalogue is one line rather than an edit to a fallback chain
    /// somebody has to remember exists.
    pub fn is_catalogue(self) -> bool {
        matches!(
            self,
            Source::OpenLibrary
                | Source::ITunes
                | Source::TvMaze
                | Source::MusicBrainz
                | Source::Steam
        )
    }

    /// Build the request for `req`. The only place a URL is constructed.
    pub fn request(self, req: &SearchRequest) -> Result<Request> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(Error::Invalid("a search needs something to search for".into()));
        }
        let q = encode(query);
        let limit = req.limit.clamp(1, 50);
        let (url, accept) = match self {
            Source::Web => (
                // The HTML endpoint rather than the JavaScript one: it is the
                // only one that answers a plain GET with the results in it.
                format!("https://html.duckduckgo.com/html/?q={q}"),
                "text/html",
            ),
            Source::Wikipedia => (
                format!(
                    "https://en.wikipedia.org/w/api.php?action=query&format=json&formatversion=2\
                     &generator=search&gsrsearch={q}&gsrlimit={limit}&gsrnamespace=0\
                     &prop=extracts|pageimages|info&exintro=1&explaintext=1&exsentences=3\
                     &piprop=thumbnail&pithumbsize=480&inprop=url&redirects=1"
                ),
                "application/json",
            ),
            Source::OpenLibrary => (
                format!(
                    "https://openlibrary.org/search.json?q={q}&limit={limit}&fields=key,title,\
                     subtitle,author_name,first_publish_year,cover_i,number_of_pages_median,\
                     publisher,isbn,ratings_average,ratings_count,subject,first_sentence"
                ),
                "application/json",
            ),
            Source::ITunes => {
                // The media filter is what stops a search for a film
                // returning the soundtrack album. It is chosen from the
                // kind, which is the only thing that knows what was meant.
                let media = match req.hint.as_str() {
                    "film" => "&media=movie&entity=movie",
                    "series" => "&media=tvShow&entity=tvSeason",
                    "music" => "&media=music&entity=album",
                    "podcast" => "&media=podcast&entity=podcast",
                    "book" => "&media=ebook",
                    _ => "",
                };
                (
                    format!("https://itunes.apple.com/search?term={q}&limit={limit}{media}"),
                    "application/json",
                )
            }
            // No limit parameter: the endpoint decides how many shows match
            // and `search` truncates. Asking for a page of a list that is
            // usually three long would be inventing a parameter.
            Source::TvMaze => {
                (format!("https://api.tvmaze.com/search/shows?q={q}"), "application/json")
            }
            // Release *groups*, not releases: one answer per record rather
            // than one per pressing, which is the difference between eleven
            // editions of the same album and the album.
            Source::MusicBrainz => (
                format!(
                    "https://musicbrainz.org/ws/2/release-group\
                     ?query={q}&fmt=json&limit={limit}"
                ),
                "application/json",
            ),
            // `cc` and `l` are the store's currency and language, and the
            // endpoint answers with prices in them whether or not they are
            // asked for. They are pinned rather than left to the caller's
            // address so that the request says nothing about where the
            // person is, and the price is dropped by the parser regardless.
            Source::Steam => (
                format!("https://store.steampowered.com/api/storesearch/?term={q}&cc=us&l=en"),
                "application/json",
            ),
            Source::Nominatim => (
                format!(
                    "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2\
                     &limit={limit}&addressdetails=1&extratags=1"
                ),
                "application/json",
            ),
        };
        Ok(Request { url, accept, source: self })
    }

    /// Turn a response body into results. Never panics on malformed input.
    pub fn parse(self, body: &str) -> Result<Vec<SearchResult>> {
        match self {
            Source::Web => Ok(parse_duckduckgo(body)),
            Source::Wikipedia => parse_wikipedia(body),
            Source::OpenLibrary => parse_open_library(body),
            Source::ITunes => parse_itunes(body),
            Source::TvMaze => parse_tvmaze(body),
            Source::MusicBrainz => parse_musicbrainz(body),
            Source::Steam => parse_steam(body),
            Source::Nominatim => parse_nominatim(body),
        }
    }

    /// A second request, for the one result somebody actually chose.
    ///
    /// Most sources need none: what their search returns is everything they
    /// have. Steam's does not — its store search answers with a name, an id
    /// and a price, and everything a shelf wants about a game (who made it,
    /// when it came out, what it is) is one request away at a different
    /// address.
    ///
    /// Fetched on *pick* rather than for every hit, which is the whole
    /// reason it is a separate step: eight results would be eight more
    /// requests, for seven games nobody is adding. See
    /// `websearch::apply_and_cover` in the service, which is where the one
    /// request happens and where failing at it costs nothing.
    pub fn detail(self, result: &SearchResult) -> Option<Request> {
        match self {
            Source::Steam => steam_app_id(&result.url).map(|id| Request {
                url: format!("https://store.steampowered.com/api/appdetails?appids={id}&l=en"),
                accept: "application/json",
                source: self,
            }),
            _ => None,
        }
    }

    /// Fold a [`detail`](Self::detail) reply into the result it was for.
    ///
    /// Additive on purpose: it fills what the search left blank and never
    /// contradicts what the search already said. A detail reply that is
    /// unreadable, or about something else entirely, therefore leaves a
    /// usable result rather than a damaged one.
    pub fn merge_detail(self, result: &mut SearchResult, body: &str) -> Result<()> {
        match self {
            Source::Steam => merge_steam_detail(result, body),
            _ => Ok(()),
        }
    }
}

/// A request, ready for anything that can perform a GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    /// What to send as `Accept`. These endpoints content-negotiate and one
    /// of them serves HTML to a client that does not ask.
    pub accept: &'static str,
    pub source: Source,
}

/// What is being looked for, and where.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchRequest {
    pub query: String,
    pub source: Source,
    /// The [`Kind::slug`](crate::library::Kind::slug) this is for, when there
    /// is one. Only [`Source::ITunes`] uses it, and only to pick a media
    /// filter — see [`Source::request`].
    pub hint: String,
    pub limit: u32,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self { query: query.into(), source: Source::Web, hint: String::new(), limit: DEFAULT_LIMIT }
    }

    pub fn on(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    pub fn about(mut self, kind_slug: impl Into<String>) -> Self {
        self.hint = kind_slug.into();
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }

    /// The request for a kind, using whatever source that kind prefers.
    pub fn for_kind(query: impl Into<String>, kind: &Kind) -> Self {
        Self::new(query).on(Source::from_slug(&kind.source)).about(kind.slug.clone())
    }

    /// The requests to try, in order, stopping at the first that answers.
    ///
    /// Up to three: this request, then Wikipedia where that makes sense, and
    /// a plain web search last. The fallback chain is what makes the feature
    /// feel like it works — Open Library does not know about a
    /// self-published pamphlet, and coming back with "no results" when the
    /// web has plenty is the failure people remember.
    ///
    /// # Why Wikipedia sits in the middle
    ///
    /// Because of what the last step returns. A plain web search answers with
    /// *pages*: "Dune (2021) — IMDb", "Dune | Rotten Tomatoes", "Buy Dune on
    /// Blu-ray". Those are websites about the thing, and what the shelf wants
    /// is the thing — a title, the people responsible, a year, a picture and
    /// a sentence. So a search for a film that iTunes happened to miss came
    /// back as a list of shops, and the person adding it had to pick one and
    /// then correct every field by hand.
    ///
    /// Wikipedia answers with the article *about* the work, which parses into
    /// exactly those fields (see [`parse_wikipedia`]), and it has one for very
    /// nearly every film, book, record and game that a catalogue can miss.
    /// Putting it before the web search costs one request in the rare case
    /// both are needed, and turns the common failure from "here are some
    /// shops" into "here is the film".
    ///
    /// It is inserted only after a *catalogue* — see
    /// [`Source::is_catalogue`] — and that limit is deliberate. Nominatim's misses are places, where
    /// what somebody actually wants next is the restaurant's own website and
    /// not an encyclopaedia article about the neighbourhood. A request that
    /// already prefers the web is an article or a recipe, which is a web page
    /// and nothing else. And Wikipedia's own misses have nowhere to go but
    /// the web, which is where they already went.
    ///
    /// It lives here rather than in either caller because there are two
    /// callers: [`WebSearch::lookup`], which is synchronous, and the desktop
    /// shell's async equivalent. Both walk this list, so neither can drift
    /// on the policy — the only difference between them is an `await`.
    pub fn attempts(&self) -> Vec<SearchRequest> {
        let mut out = vec![self.clone()];
        if self.source.is_catalogue() {
            out.push(
                SearchRequest::new(self.hinted_query())
                    .on(Source::Wikipedia)
                    .about(self.hint.clone())
                    .limit(self.limit),
            );
        }
        if self.source != Source::Web {
            out.push(
                SearchRequest::new(self.hinted_query())
                    .on(Source::Web)
                    .about(self.hint.clone())
                    .limit(self.limit),
            );
        }
        out
    }

    /// The query to hand an attempt made as a *fallback*.
    ///
    /// The kind's own word is added to it: "dune" over a films shelf becomes
    /// "dune film". Two sources of ambiguity are removed by that one word —
    /// the novel from the picture, and the record from the tour.
    ///
    /// It goes to both fallbacks and not only to the last one. That was the
    /// first shape and it was half a fix: the whole point of putting
    /// Wikipedia in front of the open web is that it answers with the work,
    /// and searching it for the bare word answers with the *most famous* work
    /// of that name — so "dune" from a films shelf came back as the novel,
    /// which is the exact case the step was added for. Wikipedia's
    /// `generator=search` is full text, so the extra word ranks the film's
    /// article first, and neither source has a structured field to
    /// disambiguate on: the query is all there is.
    ///
    /// Only for a fallback, never for a search somebody asked for on that
    /// source directly: there, what was typed is what was meant.
    fn hinted_query(&self) -> String {
        let hint = self.hint.trim();
        if hint.is_empty() || self.query.to_lowercase().contains(&hint.to_lowercase()) {
            return self.query.clone();
        }
        format!("{} {hint}", self.query)
    }
}

/// One answer, from any source, in one shape.
///
/// Everything is optional because every source omits something. The
/// interface draws what is there and the item takes what it is given; a
/// field that arrives empty is a field that stays as it was.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    /// Author, director, artist — whoever the source names as responsible.
    #[serde(default)]
    pub creator: String,
    /// A blurb, trimmed to something a card can hold.
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i16>,
    /// Where to read more. Always `http`/`https`; see [`http_url`].
    #[serde(default)]
    pub url: String,
    /// The picture, if the source offered one, at the largest size it will
    /// serve without being asked twice.
    #[serde(default)]
    pub image_url: String,
    /// Their score, normalised to `0..=100`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating_count: Option<u32>,
    /// Whose score `rating` is, when it is not the source that answered.
    ///
    /// Steam repeats Metacritic's number, and a score is worth nothing
    /// without the name of whoever gave it: "93 on Steam" would credit the
    /// shop that quoted it. Empty means the obvious thing — the source that
    /// answered is the source of the score.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rating_source: String,
    /// Where that score was published, when it is somebody else's and lives
    /// somewhere other than [`SearchResult::url`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rating_url: String,
    /// Kind-specific values, already keyed to match the field keys the
    /// seeded kinds use — `author`, `pages`, `director`, `cuisine`.
    ///
    /// Written even when empty: the interface walks it with
    /// `Object.entries`, which throws on a missing key. See `Item::external`.
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    /// Which source said so, by [`Source::slug`].
    #[serde(default)]
    pub source: String,
}

impl SearchResult {
    /// The one line under the title, for a row in the results list.
    pub fn byline(&self) -> String {
        match (self.creator.trim(), self.year) {
            ("", None) => String::new(),
            ("", Some(y)) => y.to_string(),
            (c, None) => c.to_string(),
            (c, Some(y)) => format!("{c} \u{00b7} {y}"),
        }
    }
}

// ── The facade ───────────────────────────────────────────────────────────

/// Anything that can perform a GET and hand back the body as text.
///
/// The single point at which this crate touches something it cannot do
/// itself. Implementations are responsible for the timeout, the redirect
/// policy and the size cap ([`MAX_RESPONSE_BYTES`]); everything about *what*
/// to fetch and what it means is on this side.
pub trait Fetcher: Send + Sync {
    fn get(&self, request: &Request) -> Result<String>;
}

/// Web search, for any code that has a [`Fetcher`].
///
/// Deliberately tiny. The value is not in this struct, it is in everything
/// below [`Source`] — but having one named thing to reach for is what stops
/// the next feature that wants a lookup from building its own URL by hand.
pub struct WebSearch<F> {
    fetcher: F,
}

impl<F: Fetcher> WebSearch<F> {
    pub fn new(fetcher: F) -> Self {
        Self { fetcher }
    }

    /// Run one search and return what came back, best first.
    pub fn search(&self, req: &SearchRequest) -> Result<Vec<SearchResult>> {
        let request = req.source.request(req)?;
        let body = self.fetcher.get(&request)?;
        let mut out = req.source.parse(&body)?;
        out.truncate(req.limit.clamp(1, 50) as usize);
        Ok(out)
    }

    /// Look something up on behalf of a kind, falling back to a plain web
    /// search when the preferred source has nothing.
    ///
    /// The fallback is what makes the feature feel like it works: Open
    /// Library does not know about a self-published pamphlet, and coming
    /// back with "no results" when the web has plenty is the failure people
    /// remember.
    pub fn lookup(&self, query: &str, kind: &Kind) -> Result<Vec<SearchResult>> {
        if !kind.looks_things_up() {
            return Ok(Vec::new());
        }
        let mut last = None;
        for attempt in SearchRequest::for_kind(query, kind).attempts() {
            match self.search(&attempt) {
                Ok(hits) if !hits.is_empty() => return Ok(hits),
                Ok(_) => {}
                // A source that is down must not stop the fallback running;
                // but if *every* attempt fails, "no results" would be a lie.
                Err(e) => last = Some(e),
            }
        }
        match last {
            Some(e) => Err(e),
            None => Ok(Vec::new()),
        }
    }
}

// ── Applying a result ────────────────────────────────────────────────────

/// Copy what `result` knows onto `item`, without overwriting anything the
/// person wrote themselves.
///
/// The rule, and it is the whole of the design here: **metadata fills gaps,
/// it never argues**. A title you typed survives a lookup that disagrees; a
/// note you wrote is never touched; a rating you gave is yours and the
/// source's goes beside it in [`Item::external`] rather than over it. The
/// only field replaced outright is [`Item::summary`], which is explicitly
/// the source's own words — see its doc comment.
///
/// `overwrite` is the "yes, replace what is there" the interface offers on
/// an explicit re-fetch, when somebody has looked at the two versions and
/// chosen. Even then it leaves `notes`, `rating`, `status` and the log
/// alone: those are not metadata, they are the point of the app.
pub fn apply(result: &SearchResult, kind: &Kind, item: &mut Item, overwrite: bool) {
    let fill = |field: &mut String, value: &str| {
        if !value.trim().is_empty() && (overwrite || field.trim().is_empty()) {
            *field = value.trim().to_string();
        }
    };

    fill(&mut item.title, &result.title);
    fill(&mut item.subtitle, &result.subtitle);
    fill(&mut item.creator, &result.creator);
    fill(&mut item.cover_url, &result.image_url);
    // The blurb belongs to whoever wrote it, so a re-fetch replaces it. What
    // *you* think lives in `notes`, which nothing here can reach.
    if !result.summary.trim().is_empty() {
        item.summary = result.summary.trim().to_string();
    }
    if result.year.is_some() && (overwrite || item.year.is_none()) {
        item.year = result.year;
    }

    // Only the facts this kind actually has a field for. A film's runtime
    // arriving on a shelf with no runtime field would be invisible in the
    // interface and undeletable from it, which is worse than losing it.
    for (key, value) in &result.facts {
        if value.trim().is_empty() || !kind.fields.iter().any(|f| &f.key == key) {
            continue;
        }
        match item.facts.get(key) {
            Some(existing) if !existing.trim().is_empty() && !overwrite => {}
            _ => {
                item.facts.insert(key.clone(), value.trim().to_string());
            }
        }
    }

    if let Some(score) = result.rating {
        // Whoever gave the score, which is not always whoever answered.
        let label = match result.rating_source.trim() {
            "" => Source::from_slug(&result.source).label().to_string(),
            named => named.to_string(),
        };
        let url = match result.rating_url.trim() {
            "" => result.url.clone(),
            published => published.to_string(),
        };
        let rating =
            ExternalRating { source: label.clone(), score, count: result.rating_count, url };
        match item.external.iter_mut().find(|r| r.source == label) {
            Some(existing) => *existing = rating,
            None => item.external.push(rating),
        }
    }

    if !result.url.trim().is_empty() {
        let label = Source::from_slug(&result.source).label().to_string();
        if !item.links.iter().any(|l| l.url == result.url) {
            item.links.push(Link { label, url: result.url.clone() });
        }
    }

    if item.source.is_empty() || overwrite {
        item.source = result.source.clone();
    }
    item.updated_at = jiff::Timestamp::now();
}

// ── Parsers ──────────────────────────────────────────────────────────────

/// Pull results out of DuckDuckGo's HTML endpoint.
///
/// Hand-rolled and deliberately forgiving. This is the one parser here that
/// reads a page meant for a browser rather than a documented reply, so it
/// assumes nothing about the surrounding markup: it looks for the two
/// classes that carry a result and skips anything it cannot make sense of.
/// A redesign at the other end therefore produces an empty list — which the
/// interface says plainly — rather than a shelf full of navigation links.
pub fn parse_duckduckgo(html: &str) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    // Each result opens with an anchor carrying `result__a`. Walking the
    // occurrences of that class rather than a DOM keeps this to a page of
    // code; the cost -- that a class rename breaks it -- is unavoidable
    // either way, and breaking means returning nothing.
    while let Some(offset) = html[cursor..].find(RESULT_LINK) {
        let at = cursor + offset;
        let (before, after) = (&html[cursor..at], &html[at..]);
        cursor = at + RESULT_LINK.len();

        let Some(title) = anchor_text(after) else { continue };
        if title.trim().is_empty() {
            continue;
        }
        // The href may sit either side of the class within the same opening
        // tag, and both orders occur. Look forward to the `>` first, then
        // backwards no further than the `<a` that opened this tag -- ranging
        // over all of `before` would pick up the *previous* result's link.
        let url = after
            .split_once('>')
            .and_then(|(attrs, _)| last_href(attrs))
            .or_else(|| before.rfind("<a").and_then(|from| last_href(&before[from..])))
            .map(|href| unwrap_redirect(&href))
            .and_then(|url| http_url(&url).ok())
            .unwrap_or_default();

        let summary = after
            .split_once("class=\"result__snippet\"")
            .and_then(|(_, rest)| anchor_text(rest))
            .unwrap_or_default();

        out.push(SearchResult {
            title: decode_entities(&title),
            summary: decode_entities(&summary),
            url,
            source: Source::Web.slug().to_string(),
            ..Default::default()
        });
    }
    out
}

/// The class every result's title link carries.
const RESULT_LINK: &str = "class=\"result__a\"";

/// The words inside the element whose opening tag `after` is part-way
/// through, with any markup between them removed.
///
/// Reads to the closing `</a>` rather than to the next `<`, because a
/// result's title arrives with the matched words already wrapped in `<b>` --
/// stopping at the first tag would return the empty run before "Dune" and
/// throw the title away.
fn anchor_text(after: &str) -> Option<String> {
    let start = after.find('>')? + 1;
    let end = after[start..].find("</a>")? + start;
    Some(strip_tags(&after[start..end]))
}

/// The last `href="…"` in a fragment.
fn last_href(fragment: &str) -> Option<String> {
    let at = fragment.rfind("href=\"")? + 6;
    let end = fragment[at..].find('"')? + at;
    Some(fragment[at..end].to_string())
}

/// DuckDuckGo wraps every outbound link in `/l/?uddg=<encoded>`. Unwrap it,
/// so what is stored is the address of the page rather than a redirector's.
fn unwrap_redirect(href: &str) -> String {
    let Some(at) = href.find("uddg=") else {
        // Protocol-relative links are what the page actually emits.
        return href
            .strip_prefix("//")
            .map_or_else(|| href.to_string(), |r| format!("https://{r}"));
    };
    let tail = &href[at + 5..];
    let encoded = tail.split(['&', '"']).next().unwrap_or(tail);
    decode_percent(encoded)
}

fn parse_wikipedia(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let pages = root.pointer("/query/pages").and_then(Value::as_array);
    let Some(pages) = pages else { return Ok(Vec::new()) };
    let mut out: Vec<(i64, SearchResult)> = Vec::new();
    for page in pages {
        let title = string_at(page, "title");
        if title.is_empty() {
            continue;
        }
        // `index` is the search rank. JSON object order is not, so without
        // this the best match arrives wherever the map happened to put it.
        let rank = page.get("index").and_then(Value::as_i64).unwrap_or(i64::MAX);
        out.push((
            rank,
            SearchResult {
                title,
                summary: trim_summary(&string_at(page, "extract")),
                url: http_url(&string_at(page, "fullurl")).unwrap_or_default(),
                image_url: page
                    .pointer("/thumbnail/source")
                    .and_then(Value::as_str)
                    .and_then(|u| http_url(u).ok())
                    .unwrap_or_default(),
                source: Source::Wikipedia.slug().to_string(),
                ..Default::default()
            },
        ));
    }
    out.sort_by_key(|(rank, _)| *rank);
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

fn parse_open_library(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(docs) = root.get("docs").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for doc in docs {
        let title = string_at(doc, "title");
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        let author = first_string(doc, "author_name");
        if !author.is_empty() {
            facts.insert("author".to_string(), author.clone());
        }
        if let Some(pages) = doc.get("number_of_pages_median").and_then(Value::as_i64) {
            facts.insert("pages".to_string(), pages.to_string());
        }
        let publisher = first_string(doc, "publisher");
        if !publisher.is_empty() {
            facts.insert("publisher".to_string(), publisher);
        }
        let isbn = first_string(doc, "isbn");
        if !isbn.is_empty() {
            facts.insert("isbn".to_string(), isbn);
        }
        // Open Library rates out of five.
        let rating = doc
            .get("ratings_average")
            .and_then(Value::as_f64)
            .and_then(|score| normalize_rating(score, 5.0));
        let key = string_at(doc, "key");
        out.push(SearchResult {
            title,
            subtitle: string_at(doc, "subtitle"),
            creator: author,
            summary: trim_summary(&first_string(doc, "first_sentence")),
            year: doc.get("first_publish_year").and_then(Value::as_i64).and_then(to_year),
            url: if key.is_empty() {
                String::new()
            } else {
                format!("https://openlibrary.org{key}")
            },
            image_url: doc
                .get("cover_i")
                .and_then(Value::as_i64)
                .map(|id| format!("https://covers.openlibrary.org/b/id/{id}-L.jpg"))
                .unwrap_or_default(),
            rating,
            rating_count: doc
                .get("ratings_count")
                .and_then(Value::as_i64)
                .and_then(|n| u32::try_from(n).ok()),
            facts,
            source: Source::OpenLibrary.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

fn parse_itunes(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(results) = root.get("results").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for r in results {
        // A film is a `trackName`, an album a `collectionName`; asking for
        // both in that order is what makes one parser serve four media.
        let title = match string_at(r, "trackName") {
            t if !t.is_empty() => t,
            _ => string_at(r, "collectionName"),
        };
        if title.is_empty() {
            continue;
        }
        let creator = string_at(r, "artistName");
        let mut facts = BTreeMap::new();
        let genre = string_at(r, "primaryGenreName");
        if !genre.is_empty() {
            facts.insert("genre".to_string(), genre);
        }
        // The same number under three names, because a kind's field key is
        // the kind's business and a runtime is a runtime.
        if let Some(ms) = r.get("trackTimeMillis").and_then(Value::as_i64) {
            let minutes = (ms / 60_000).max(0).to_string();
            facts.insert("runtime".to_string(), minutes);
        }
        if let Some(n) = r.get("trackCount").and_then(Value::as_i64) {
            facts.insert("tracks".to_string(), n.to_string());
        }
        if !creator.is_empty() {
            // Whichever of these the kind has a field for wins; the rest are
            // dropped by `apply`, which only keeps facts a kind can show.
            for key in ["director", "artist", "show"] {
                facts.insert(key.to_string(), creator.clone());
            }
        }
        let description = match string_at(r, "longDescription") {
            d if !d.is_empty() => d,
            _ => string_at(r, "description"),
        };
        out.push(SearchResult {
            title,
            subtitle: string_at(r, "collectionName"),
            creator,
            summary: trim_summary(&description),
            year: year_from_iso(&string_at(r, "releaseDate")),
            url: http_url(&string_at(r, "trackViewUrl"))
                .or_else(|_| http_url(&string_at(r, "collectionViewUrl")))
                .unwrap_or_default(),
            // The artwork is served at 100px unless the size in the path is
            // rewritten. Apple serves whatever is asked for from the same
            // address, and 100px is unusable on any display made this decade.
            image_url: http_url(&string_at(r, "artworkUrl100").replace("100x100", "600x600"))
                .unwrap_or_default(),
            facts,
            source: Source::ITunes.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Pull shows out of TVmaze's search reply.
///
/// The endpoint answers with a list of `{score, show}` already ranked, so
/// the order is kept as given. Everything a television shelf asks for is in
/// the one reply — the network, the genres, the year it began, a portrait
/// and the site's own rating — which is the whole reason this source is
/// here rather than a shop's television section.
fn parse_tvmaze(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(hits) = root.as_array() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for hit in hits {
        // A search answers with the show wrapped in a score; the endpoints
        // that return a show on its own do not. Accept both, so that a
        // single-show reply is not silently no results.
        let show = hit.get("show").unwrap_or(hit);
        let title = string_at(show, "name");
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        // Broadcast or streamed: one field either way, because "where it is
        // on" is one question however the industry files it.
        let network = match show.pointer("/network/name").and_then(Value::as_str) {
            Some(name) => name.to_string(),
            None => show.pointer("/webChannel/name").and_then(Value::as_str).unwrap_or("").into(),
        };
        if !network.is_empty() {
            facts.insert("network".to_string(), network.clone());
        }
        let genres: Vec<&str> = show
            .get("genres")
            .and_then(Value::as_array)
            .map(|g| g.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !genres.is_empty() {
            facts.insert("genre".to_string(), genres.join(", "));
        }
        if let Some(minutes) = show
            .get("averageRuntime")
            .and_then(Value::as_i64)
            .or_else(|| show.get("runtime").and_then(Value::as_i64))
        {
            facts.insert("runtime".to_string(), minutes.to_string());
        }
        out.push(SearchResult {
            title,
            subtitle: network,
            // TVmaze names no creator in a search reply, and guessing one
            // from the network would be worse than leaving it for the person
            // who knows.
            creator: String::new(),
            // The summary is a fragment of HTML, always. Tags out, entities
            // decoded, then cut — in that order, or a `&amp;` lands mid-cut.
            summary: trim_summary(&decode_entities(&strip_tags(&string_at(show, "summary")))),
            year: year_from_iso(&string_at(show, "premiered")),
            url: http_url(&string_at(show, "url")).unwrap_or_default(),
            image_url: show
                .pointer("/image/original")
                .or_else(|| show.pointer("/image/medium"))
                .and_then(Value::as_str)
                .and_then(|u| http_url(u).ok())
                .unwrap_or_default(),
            // TVmaze rates out of ten, and says nothing about how many
            // people voted.
            rating: show
                .pointer("/rating/average")
                .and_then(Value::as_f64)
                .and_then(|score| normalize_rating(score, 10.0)),
            rating_count: None,
            facts,
            source: Source::TvMaze.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Pull records out of MusicBrainz's release-group search.
///
/// # Where the picture comes from
///
/// Not from this reply: MusicBrainz holds the facts and the Cover Art
/// Archive holds the sleeves, and a search says nothing about whether one
/// exists. The address is *derived* from the record's id, which the archive
/// serves or answers 404 to, and a 404 here is a card with no artwork rather
/// than a failed lookup — see `apply_and_cover` in the service, which treats
/// the picture as best-effort for exactly this reason. The alternative was a
/// second request per result, eight of them per search, to a rate-limited
/// host, to find out something the card can live without.
fn parse_musicbrainz(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(groups) = root.get("release-groups").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<(i64, SearchResult)> = Vec::new();
    for group in groups {
        let title = string_at(group, "title");
        let id = string_at(group, "id");
        if title.is_empty() || id.is_empty() {
            continue;
        }
        // A credit is a list — "Simon & Garfunkel" is one artist, but a
        // duet is two joined by a phrase the data carries. Joining on it is
        // what turns three objects back into the line printed on the sleeve.
        let artist = group
            .get("artist-credit")
            .and_then(Value::as_array)
            .map(|credits| {
                credits.iter().fold(String::new(), |mut line, credit| {
                    line.push_str(&string_at(credit, "name"));
                    line.push_str(credit.get("joinphrase").and_then(Value::as_str).unwrap_or(""));
                    line
                })
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut facts = BTreeMap::new();
        if !artist.is_empty() {
            facts.insert("artist".to_string(), artist.clone());
        }
        // "Album", "EP", "Single", and the secondary types that say a record
        // is a compilation or a soundtrack rather than a record proper.
        let mut descriptors: Vec<String> = Vec::new();
        let primary = string_at(group, "primary-type");
        if !primary.is_empty() {
            descriptors.push(primary);
        }
        if let Some(secondary) = group.get("secondary-types").and_then(Value::as_array) {
            descriptors.extend(secondary.iter().filter_map(Value::as_str).map(str::to_string));
        }
        out.push((
            // Lucene's relevance, which the endpoint sorts by and which a
            // parser has no business reordering. Kept explicitly so that it
            // survives anything this function does to the list.
            group.get("score").and_then(Value::as_i64).unwrap_or(0),
            SearchResult {
                title,
                subtitle: descriptors.join(", "),
                creator: artist,
                summary: string_at(group, "disambiguation"),
                year: year_from_iso(&string_at(group, "first-release-date")),
                url: format!("https://musicbrainz.org/release-group/{id}"),
                image_url: format!("https://coverartarchive.org/release-group/{id}/front-500"),
                facts,
                source: Source::MusicBrainz.slug().to_string(),
                ..Default::default()
            },
        ));
    }
    out.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

/// Pull games out of Steam's store search.
///
/// Thin, as store replies go: a name, an id, a picture the size of a
/// postage stamp and what it costs today. The id is the useful part — the
/// store serves each game's poster at a fixed address built from it, which
/// is how a card gets artwork worth looking at out of a reply that contains
/// a 231-pixel thumbnail.
///
/// # Why the score is dropped
///
/// The reply carries a `metascore`, and it is Metacritic's, not Steam's.
/// [`apply`] labels an [`ExternalRating`] with the source that answered, so
/// keeping it would put "94 on Steam" beside a game — a number attributed to
/// the shop that repeated it rather than the people who gave it. A rating
/// with the wrong name on it is worse than no rating.
fn parse_steam(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(items) = root.get("items").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        let title = string_at(item, "name");
        let Some(id) = item.get("id").and_then(Value::as_i64) else { continue };
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        // What it runs on, which is the only sense in which a store that
        // sells nothing else can answer "platform".
        let platforms: Vec<&str> = [("windows", "Windows"), ("mac", "macOS"), ("linux", "Linux")]
            .iter()
            .filter(|(key, _)| {
                item.pointer(&format!("/platforms/{key}")).and_then(Value::as_bool).unwrap_or(false)
            })
            .map(|(_, name)| *name)
            .collect();
        if !platforms.is_empty() {
            facts.insert("platform".to_string(), platforms.join(", "));
        }
        out.push(SearchResult {
            title,
            url: format!("https://store.steampowered.com/app/{id}/"),
            // The tall library artwork rather than the wide banner: a shelf
            // draws covers, and `header.jpg` in a poster's frame is a letter
            // box with a logo in it.
            image_url: format!(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/{id}/library_600x900.jpg"
            ),
            facts,
            source: Source::Steam.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Fill a Steam result in from the store's page for that one game.
///
/// The reply is keyed by the app id — `{"1145360":{"success":true,"data":…}}`
/// — and `success` is `false` for a game that is not sold in the store's
/// region, which is a blank rather than an error: the search result is still
/// perfectly good, it just stays as thin as it arrived.
fn merge_steam_detail(result: &mut SearchResult, body: &str) -> Result<()> {
    let root: Value = serde_json::from_str(body)?;
    let data = root
        .as_object()
        .and_then(|by_id| by_id.values().next())
        .filter(|entry| entry.get("success").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|entry| entry.get("data"));
    let Some(data) = data else { return Ok(()) };

    if result.summary.trim().is_empty() {
        let blurb = string_at(data, "short_description");
        result.summary = trim_summary(&decode_entities(&strip_tags(&blurb)));
    }
    if result.year.is_none() {
        // "Sep 17, 2020", "2020", "Q3 2026" — a store's own prose, in a
        // format that has changed before. The year is the only part of it
        // this app has a field for.
        result.year = year_from_text(&string_at(data.get("release_date").unwrap_or(data), "date"));
    }
    for (key, field) in [("developer", "developers"), ("publisher", "publishers")] {
        let who = first_string(data, field);
        if !who.is_empty() {
            result.facts.entry(key.to_string()).or_insert(who);
        }
    }
    // Steam files a game under several genres and calls each one a
    // description; the shelf has one line for them.
    let genres: Vec<String> = data
        .get("genres")
        .and_then(Value::as_array)
        .map(|all| all.iter().map(|g| string_at(g, "description")).collect())
        .unwrap_or_default();
    let genres: Vec<String> = genres.into_iter().filter(|g| !g.is_empty()).collect();
    if !genres.is_empty() {
        result.facts.entry("genre".to_string()).or_insert_with(|| genres.join(", "));
    }
    if result.creator.trim().is_empty() {
        result.creator = first_string(data, "developers");
    }
    // Metacritic's, out of a hundred, and credited to them — see
    // `SearchResult::rating_source`.
    if let Some(score) = data.pointer("/metacritic/score").and_then(Value::as_f64) {
        if let Some(rating) = normalize_rating(score, 100.0) {
            result.rating = Some(rating);
            result.rating_source = "Metacritic".to_string();
            result.rating_url = data
                .pointer("/metacritic/url")
                .and_then(Value::as_str)
                .and_then(|u| http_url(u).ok())
                .unwrap_or_default();
        }
    }
    Ok(())
}

/// The app id inside a store address this file built, and nothing else.
///
/// Deliberately strict about the prefix: the id is about to be interpolated
/// into a request, and the only addresses that may produce one are the ones
/// [`parse_steam`] wrote.
fn steam_app_id(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://store.steampowered.com/app/")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

fn parse_nominatim(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(places) = root.as_array() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for place in places {
        let display = string_at(place, "display_name");
        if display.is_empty() {
            continue;
        }
        // `display_name` is a full comma-separated address whose first part
        // is the name of the thing. Splitting it is what turns "Som Saa, 43,
        // Commercial Street, Spitalfields, …" into a title and an address.
        let (name, rest) = display.split_once(',').unwrap_or((display.as_str(), ""));
        let mut facts = BTreeMap::new();
        facts.insert("address".to_string(), rest.trim().to_string());
        for (tag, key) in [("cuisine", "cuisine"), ("phone", "phone"), ("website", "url")] {
            let value = place.pointer(&format!("/extratags/{tag}")).and_then(Value::as_str);
            if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                facts.insert(key.to_string(), value.to_string());
            }
        }
        if let Some(country) = place.pointer("/address/country").and_then(Value::as_str) {
            facts.insert("country".to_string(), country.to_string());
        }
        if let Some(kind) = osm_place_type(place) {
            facts.insert("type".to_string(), kind);
        }
        let osm_url = match (place.get("osm_type").and_then(Value::as_str), place.get("osm_id")) {
            (Some(kind), Some(id)) => format!("https://www.openstreetmap.org/{kind}/{id}"),
            _ => String::new(),
        };
        out.push(SearchResult {
            title: name.trim().to_string(),
            subtitle: rest.trim().to_string(),
            summary: display,
            url: osm_url,
            facts,
            source: Source::Nominatim.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// OpenStreetMap's own word for what a place is -- `museum`, `playground`,
/// `theme_park` -- tidied into what a Places shelf's type field holds.
///
/// Only for the categories that describe somewhere a person goes. The rest
/// describe the map instead: a city is `boundary/administrative`, a street
/// address `building/house`, a road `highway/primary`. Filling the field
/// with "Administrative" would be wrong on that card, and the detail panel
/// would then offer it on every other place on the shelf.
fn osm_place_type(place: &Value) -> Option<String> {
    const VISITABLE: &[&str] = &["tourism", "leisure", "historic", "natural", "amenity"];
    // `category` in the `jsonv2` format this asks for, `class` in `json`.
    let category = place.get("category").or_else(|| place.get("class")).and_then(Value::as_str)?;
    let osm_type = place.get("type").and_then(Value::as_str)?.trim();
    // "yes" means "one of these, of no stated sort", which is no type.
    if !VISITABLE.contains(&category) || osm_type.is_empty() || osm_type == "yes" {
        return None;
    }
    let words = osm_type.replace('_', " ");
    let mut chars = words.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

// ── Small helpers ────────────────────────────────────────────────────────

/// Refuse anything that is not an ordinary web address.
///
/// The bug this exists to prevent is the same one
/// [`calendar::normalize_feed_url`](crate::calendar::normalize_feed_url)
/// guards, arriving by a different door: a `file:///etc/passwd` in a search
/// result's `href`, followed downstream by a fetcher that reads it. The two
/// are kept separate rather than shared because a calendar URL is *pasted by
/// the user* and deserves the courtesy of `webcal://` rewriting and a bare
/// hostname; this one comes off the internet and gets no such benefit of the
/// doubt.
pub fn http_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(trimmed.to_string())
    } else {
        Err(Error::Invalid(format!("{trimmed:?} is not a web address")))
    }
}

/// Percent-encode for a query string. Unreserved characters survive;
/// everything else, spaces and ampersands very much included, does not.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The inverse, tolerant of malformed input: a stray `%` is a `%`.
fn decode_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The five entities that actually appear in a search result's title.
fn decode_entities(s: &str) -> String {
    let mut out = s
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ");
    // Ampersand last, or `&amp;lt;` decodes to `<` in two passes.
    out = out.replace("&amp;", "&");
    out.trim().to_string()
}

/// Remove any markup inside a run of text, leaving the words.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// A blurb, cut to something a card can hold without becoming an essay.
fn trim_summary(text: &str) -> String {
    const MAX: usize = 480;
    let text = text.trim();
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    // Cut on a word boundary and say so, rather than stopping mid-syllable.
    let cut: String = text.chars().take(MAX).collect();
    let end = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}\u{2026}", cut[..end].trim_end_matches([',', ';', ':']))
}

fn string_at(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_string()
}

/// The first element of a string array, or the string itself if the source
/// gave one rather than a list. Both shapes occur across these APIs.
fn first_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::Array(items)) => {
            items.iter().find_map(Value::as_str).unwrap_or_default().trim().to_string()
        }
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// A year, if it is one a person could plausibly have meant.
fn to_year(n: i64) -> Option<i16> {
    i16::try_from(n).ok().filter(|y| (-4000..=4000).contains(y))
}

fn year_from_iso(date: &str) -> Option<i16> {
    date.get(..4)?.parse::<i64>().ok().and_then(to_year)
}

/// The year inside a line of prose — "Sep 17, 2020", "Q3 2026", "2020".
///
/// The first run of exactly four digits, which is the year in every shape a
/// store has used so far and is not a day, a month or a price. A number
/// longer than four digits is not a year at all, so a run of five is
/// skipped rather than truncated.
fn year_from_text(text: &str) -> Option<i16> {
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let digits: String = rest[start..].chars().take_while(char::is_ascii_digit).collect();
        if digits.len() == 4 {
            if let Some(year) = digits.parse::<i64>().ok().and_then(to_year) {
                return Some(year);
            }
        }
        rest = &rest[start + digits.len()..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{ItemStatus, default_kinds};

    fn kind(slug: &str) -> Kind {
        default_kinds().into_iter().find(|k| k.slug == slug).expect("a seeded kind")
    }

    struct Canned(&'static str);
    impl Fetcher for Canned {
        fn get(&self, _request: &Request) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    struct Empty;
    impl Fetcher for Empty {
        fn get(&self, request: &Request) -> Result<String> {
            // Answer the fallback and nothing else, so a test can tell that
            // the fallback is what ran.
            Ok(match request.source {
                Source::Web => DDG.to_string(),
                _ => "{\"docs\":[]}".to_string(),
            })
        }
    }

    const DDG: &str = r#"
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fdune&amp;rut=x">
          <b>Dune</b> &amp; its sequels
        </a>
        <a class="result__snippet">A desert planet, and the empire that wants it.</a>
      </div>
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Ftwo">Two</a>
        <a class="result__snippet">Second.</a>
      </div>"#;

    #[test]
    fn a_query_is_encoded_rather_than_interpolated() {
        // The bug this guards: a title with an ampersand in it silently
        // becoming a second query parameter.
        let req = SearchRequest::new("Fear & Loathing in Las Vegas");
        let built = Source::Web.request(&req).unwrap();
        assert!(built.url.contains("Fear%20%26%20Loathing"), "got {}", built.url);
        assert!(!built.url.contains("&Loathing"));
        assert_eq!(encode("a/b?c=d#e"), "a%2Fb%3Fc%3Dd%23e");
        // ...and the unreserved set is left legible.
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn an_empty_query_is_refused_before_a_request_is_built() {
        assert!(Source::Web.request(&SearchRequest::new("   ")).is_err());
    }

    #[test]
    fn the_itunes_media_filter_comes_from_the_kind() {
        // Without it, searching for the film Dune returns the soundtrack.
        let film = Source::ITunes.request(&SearchRequest::new("dune").about("film")).unwrap();
        assert!(film.url.contains("media=movie"), "got {}", film.url);
        let album = Source::ITunes.request(&SearchRequest::new("dune").about("music")).unwrap();
        assert!(album.url.contains("entity=album"), "got {}", album.url);
        // A kind with no opinion gets no filter rather than a wrong one.
        let plain = Source::ITunes.request(&SearchRequest::new("dune")).unwrap();
        assert!(!plain.url.contains("media="));
    }

    #[test]
    fn an_unknown_source_slug_degrades_to_a_web_search() {
        // A kind written by a newer build must not make the shelf unreadable.
        assert_eq!(Source::from_slug("letterboxd"), Source::Web);
        assert_eq!(Source::from_slug(""), Source::Web);
        for s in Source::ALL {
            assert_eq!(Source::from_slug(s.slug()), s, "{} did not round trip", s.slug());
        }
    }

    #[test]
    fn duckduckgo_results_are_unwrapped_from_the_redirect() {
        let hits = parse_duckduckgo(DDG);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Dune & its sequels", "entities should be decoded");
        assert_eq!(hits[0].url, "https://example.org/dune", "the redirect was not unwrapped");
        assert!(hits[0].summary.starts_with("A desert planet"));
        assert_eq!(hits[1].url, "https://example.org/two");
    }

    #[test]
    fn a_redesigned_results_page_yields_nothing_rather_than_rubbish() {
        // The failure mode that matters for a scraper: it must not return a
        // shelf full of navigation links when the markup changes.
        assert!(
            parse_duckduckgo("<html><body><a href=\"/about\">About</a></body></html>").is_empty()
        );
        assert!(parse_duckduckgo("").is_empty());
        assert!(parse_duckduckgo("class=\"result__a\"").is_empty());
    }

    #[test]
    fn open_library_gives_a_book_its_cover_and_its_numbers() {
        let body = r#"{"docs":[{
            "key":"/works/OL893415W","title":"Dune","subtitle":"Book one",
            "author_name":["Frank Herbert"],"first_publish_year":1965,
            "cover_i":8188463,"number_of_pages_median":412,
            "publisher":["Chilton Books"],"isbn":["9780441013593"],
            "ratings_average":4.2,"ratings_count":1204,
            "first_sentence":["A beginning is the time for taking the most delicate care."]
        }]}"#;
        let hits = parse_open_library(body).unwrap();
        let hit = &hits[0];
        assert_eq!(hit.title, "Dune");
        assert_eq!(hit.creator, "Frank Herbert");
        assert_eq!(hit.year, Some(1965));
        assert_eq!(hit.url, "https://openlibrary.org/works/OL893415W");
        assert_eq!(hit.image_url, "https://covers.openlibrary.org/b/id/8188463-L.jpg");
        // 4.2 out of five is 84 out of a hundred.
        assert_eq!(hit.rating, Some(84));
        assert_eq!(hit.rating_count, Some(1204));
        assert_eq!(hit.facts.get("pages").map(String::as_str), Some("412"));
        assert_eq!(hit.facts.get("isbn").map(String::as_str), Some("9780441013593"));
        assert!(hit.summary.starts_with("A beginning"));
    }

    #[test]
    fn itunes_artwork_is_asked_for_at_a_usable_size() {
        let body = r#"{"results":[{
            "trackName":"Dune","artistName":"Denis Villeneuve",
            "artworkUrl100":"https://is1.example.com/img/100x100bb.jpg",
            "releaseDate":"2021-10-22T07:00:00Z","primaryGenreName":"Sci-Fi",
            "trackTimeMillis":9360000,"trackViewUrl":"https://itunes.example/dune",
            "longDescription":"A desert planet."
        }]}"#;
        let hit = &parse_itunes(body).unwrap()[0];
        assert_eq!(hit.image_url, "https://is1.example.com/img/600x600bb.jpg");
        assert_eq!(hit.year, Some(2021));
        assert_eq!(hit.facts.get("runtime").map(String::as_str), Some("156"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Sci-Fi"));
        assert_eq!(hit.creator, "Denis Villeneuve");
    }

    #[test]
    fn itunes_falls_back_to_the_collection_name_for_an_album() {
        let body = r#"{"results":[{
            "collectionName":"Kind of Blue","artistName":"Miles Davis",
            "trackCount":5,"releaseDate":"1959-08-17T07:00:00Z",
            "collectionViewUrl":"https://itunes.example/kob"
        }]}"#;
        let hit = &parse_itunes(body).unwrap()[0];
        assert_eq!(hit.title, "Kind of Blue");
        assert_eq!(hit.facts.get("tracks").map(String::as_str), Some("5"));
        assert_eq!(hit.url, "https://itunes.example/kob");
    }

    #[test]
    fn wikipedia_results_keep_the_search_ranking() {
        // The bug this guards: JSON object order is not search order, so the
        // best match arrives wherever the map put it.
        let body = r#"{"query":{"pages":[
            {"index":2,"title":"Dune (novel)","extract":"A 1965 novel.",
             "fullurl":"https://en.wikipedia.org/wiki/Dune_(novel)",
             "thumbnail":{"source":"https://upload.example/dune.jpg"}},
            {"index":1,"title":"Dune","extract":"A sand dune.",
             "fullurl":"https://en.wikipedia.org/wiki/Dune"}
        ]}}"#;
        let hits = parse_wikipedia(body).unwrap();
        assert_eq!(hits[0].title, "Dune", "search rank was not honoured");
        assert_eq!(hits[1].image_url, "https://upload.example/dune.jpg");
    }

    #[test]
    fn tvmaze_answers_with_the_show_rather_than_a_season_for_sale() {
        // Shaped as the endpoint answers: the show inside a relevance score,
        // a streamer rather than a network, and a blurb that is HTML.
        let body = r#"[{"score":0.89,"show":{
            "id":44933,"url":"https://www.tvmaze.com/shows/44933/severance",
            "name":"Severance","genres":["Drama","Science-Fiction"],
            "averageRuntime":49,"premiered":"2022-02-18","rating":{"average":7.6},
            "network":null,"webChannel":{"id":310,"name":"Apple TV"},
            "image":{"medium":"https://static.tvmaze.com/m.jpg",
                     "original":"https://static.tvmaze.com/o.jpg"},
            "summary":"<p>Mark Scout leads a team at Lumon &amp; co.</p>"
        }}]"#;
        let hits = parse_tvmaze(body).unwrap();
        let hit = &hits[0];
        assert_eq!(hit.title, "Severance");
        assert_eq!(hit.year, Some(2022));
        // The streamer stands in for the network, because "where it is on"
        // is one question however the industry files it.
        assert_eq!(hit.facts.get("network").map(String::as_str), Some("Apple TV"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Drama, Science-Fiction"));
        assert_eq!(hit.facts.get("runtime").map(String::as_str), Some("49"));
        // 7.6 out of ten, on the same hundred as everybody else's score.
        assert_eq!(hit.rating, Some(76));
        assert_eq!(hit.image_url, "https://static.tvmaze.com/o.jpg");
        // Markup out and entities decoded, in that order.
        assert_eq!(hit.summary, "Mark Scout leads a team at Lumon & co.");
    }

    #[test]
    fn a_musicbrainz_cover_is_derived_from_the_records_id() {
        // The search reply says nothing about artwork; the address is built
        // from the id and the archive answers it or does not.
        let body = r#"{"release-groups":[
            {"id":"6f25f9fb","score":87,"title":"KID A MNESIA",
             "primary-type":"Album","secondary-types":["Compilation"],
             "first-release-date":"2021-11-05",
             "artist-credit":[{"name":"Radiohead"}]},
            {"id":"e75c0549","score":100,"title":"Kid A","primary-type":"Album",
             "first-release-date":"2000-08-03",
             "artist-credit":[{"name":"Radiohead"}]}
        ]}"#;
        let hits = parse_musicbrainz(body).unwrap();
        // Relevance decides the order, not the order the reply happened to
        // arrive in.
        assert_eq!(hits[0].title, "Kid A");
        assert_eq!(hits[0].creator, "Radiohead");
        // The year the record came out, not the year of a remaster somebody
        // is selling -- which is the whole reason this source is here.
        assert_eq!(hits[0].year, Some(2000));
        assert_eq!(hits[0].url, "https://musicbrainz.org/release-group/e75c0549");
        assert_eq!(
            hits[0].image_url,
            "https://coverartarchive.org/release-group/e75c0549/front-500"
        );
        assert_eq!(hits[0].facts.get("artist").map(String::as_str), Some("Radiohead"));
        assert_eq!(hits[1].subtitle, "Album, Compilation");
    }

    #[test]
    fn a_split_artist_credit_is_joined_back_into_one_line() {
        let body = r#"{"release-groups":[{"id":"x","title":"Watch the Throne",
            "artist-credit":[{"name":"Jay-Z","joinphrase":" & "},{"name":"Kanye West"}]}]}"#;
        let hits = parse_musicbrainz(body).unwrap();
        assert_eq!(hits[0].creator, "Jay-Z & Kanye West");
    }

    #[test]
    fn steam_builds_the_poster_from_the_app_id() {
        let body = r#"{"total":2,"items":[
            {"type":"app","name":"Hades","id":1145360,
             "price":{"currency":"USD","final":2499},"metascore":"93",
             "tiny_image":"https://shared.akamai.steamstatic.com/capsule_231x87.jpg",
             "platforms":{"windows":true,"mac":true,"linux":false}},
            {"type":"app","name":"No id here"}
        ]}"#;
        let hits = parse_steam(body).unwrap();
        // The second has no id, so there is no address to build and no
        // result to show; it is dropped rather than half-drawn.
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.title, "Hades");
        assert_eq!(hit.url, "https://store.steampowered.com/app/1145360/");
        // Not the 231-pixel thumbnail the reply carries.
        assert_eq!(
            hit.image_url,
            "https://cdn.cloudflare.steamstatic.com/steam/apps/1145360/library_600x900.jpg"
        );
        assert_eq!(hit.facts.get("platform").map(String::as_str), Some("Windows, macOS"));
        // Metacritic's number, arriving through a shop. Keeping it would put
        // Steam's name on somebody else's score.
        assert_eq!(hit.rating, None);
    }

    #[test]
    fn a_chosen_steam_result_is_filled_in_from_the_games_own_page() {
        let mut hit = parse_steam(
            r#"{"items":[{"type":"app","name":"Hades","id":1145360,
                "platforms":{"windows":true,"mac":false,"linux":false}}]}"#,
        )
        .unwrap()
        .remove(0);
        // Nothing worth putting on a card yet, which is the reason the
        // second request exists.
        assert_eq!(hit.year, None);
        assert!(hit.summary.is_empty());

        let request = Source::Steam.detail(&hit).expect("a store page to ask for");
        assert_eq!(
            request.url,
            "https://store.steampowered.com/api/appdetails?appids=1145360&l=en"
        );

        let body = r#"{"1145360":{"success":true,"data":{
            "name":"Hades","type":"game","developers":["Supergiant Games"],
            "publishers":["Supergiant Games"],
            "release_date":{"coming_soon":false,"date":"Sep 17, 2020"},
            "metacritic":{"score":93,"url":"https://www.metacritic.com/game/pc/hades"},
            "short_description":"Defy the god of the dead &amp; hack out of the Underworld.",
            "genres":[{"id":"1","description":"Action"},{"id":"23","description":"Indie"}]
        }}}"#;
        Source::Steam.merge_detail(&mut hit, body).unwrap();
        assert_eq!(hit.year, Some(2020));
        assert_eq!(hit.creator, "Supergiant Games");
        assert_eq!(hit.facts.get("developer").map(String::as_str), Some("Supergiant Games"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Action, Indie"));
        assert_eq!(hit.summary, "Defy the god of the dead & hack out of the Underworld.");
        // The platform the search found is not overwritten by the page.
        assert_eq!(hit.facts.get("platform").map(String::as_str), Some("Windows"));

        // The score is Metacritic's, and says so.
        assert_eq!(hit.rating, Some(93));
        assert_eq!(hit.rating_source, "Metacritic");
        let mut item = Item::new(kind("game").id, "Hades");
        apply(&hit, &kind("game"), &mut item, false);
        let theirs = &item.external[0];
        assert_eq!(theirs.source, "Metacritic", "a score keeps the name of whoever gave it");
        assert_eq!(theirs.url, "https://www.metacritic.com/game/pc/hades");
        // The link to the thing itself is still the shop's, because that is
        // where the game is.
        assert_eq!(item.links[0].label, "Steam");
    }

    #[test]
    fn a_detail_reply_about_nothing_leaves_the_result_alone() {
        let mut hit =
            parse_steam(r#"{"items":[{"name":"Hades","id":1145360}]}"#).unwrap().remove(0);
        let before = hit.clone();
        // Not sold here: `success` is false and there is no `data` at all.
        Source::Steam.merge_detail(&mut hit, r#"{"1145360":{"success":false}}"#).unwrap();
        assert_eq!(hit, before);
        // And a reply that is not JSON is an error, never a panic and never
        // a half-filled card.
        assert!(Source::Steam.merge_detail(&mut hit, "<html>nope").is_err());
        assert_eq!(hit, before);
    }

    #[test]
    fn a_source_with_nothing_more_to_say_asks_for_nothing() {
        for source in Source::ALL.iter().filter(|s| **s != Source::Steam) {
            let hit = SearchResult {
                url: "https://store.steampowered.com/app/1145360/".into(),
                ..Default::default()
            };
            assert!(source.detail(&hit).is_none(), "{source:?} invented a second request");
        }
        // Nor for a result whose address this file did not write: the id is
        // about to go into a URL.
        for url in [
            "https://store.steampowered.com.evil.example/app/1/",
            "http://store.steampowered.com/app/1/",
            "https://store.steampowered.com/app/",
        ] {
            let hit = SearchResult { url: url.into(), ..Default::default() };
            assert!(Source::Steam.detail(&hit).is_none(), "{url} produced a request");
        }
    }

    #[test]
    fn every_seeded_shelf_names_a_source_that_exists() {
        // Except the one that names no source at all, which is checked by
        // `a_shelf_that_looks_nothing_up_sends_nothing`.
        for kind in crate::library::default_kinds().into_iter().filter(|k| k.looks_things_up()) {
            let source = Source::from_slug(&kind.source);
            assert_eq!(
                source.slug(),
                kind.source,
                "the {} shelf names {:?}, which degrades to a plain web search",
                kind.slug,
                kind.source,
            );
        }
        // The three shelves the specialised catalogues were added for.
        assert_eq!(Source::from_slug(&kind("series").source), Source::TvMaze);
        assert_eq!(Source::from_slug(&kind("music").source), Source::MusicBrainz);
        assert_eq!(Source::from_slug(&kind("game").source), Source::Steam);
    }

    #[test]
    fn a_console_game_falls_through_steam_to_wikipedia() {
        // Steam sells games for one machine. The shelf holds games for any
        // of them, so the catalogue's miss has to land somewhere that knows
        // what a Zelda is.
        let games = SearchRequest::for_kind("breath of the wild", &kind("game"));
        assert_eq!(
            games.attempts().iter().map(|a| a.source).collect::<Vec<_>>(),
            vec![Source::Steam, Source::Wikipedia, Source::Web],
        );
        for source in Source::ALL {
            assert_eq!(
                source.is_catalogue(),
                SearchRequest::new("x")
                    .on(source)
                    .attempts()
                    .iter()
                    .any(|a| a.source == Source::Wikipedia && source != Source::Wikipedia),
                "{source:?} disagrees with its own fallback chain",
            );
        }
    }

    #[test]
    fn nominatim_splits_a_display_name_into_a_name_and_an_address() {
        let body = r#"[{
            "display_name":"Som Saa, 43A, Commercial Street, Spitalfields, London, E1 6BD",
            "osm_type":"way","osm_id":123,
            "extratags":{"cuisine":"thai","phone":"020 7324 7790","website":"https://somsaa.example"},
            "address":{"country":"United Kingdom"}
        }]"#;
        let hit = &parse_nominatim(body).unwrap()[0];
        assert_eq!(hit.title, "Som Saa");
        assert_eq!(hit.facts.get("cuisine").map(String::as_str), Some("thai"));
        assert!(hit.facts["address"].starts_with("43A, Commercial Street"));
        assert_eq!(hit.facts.get("country").map(String::as_str), Some("United Kingdom"));
        assert_eq!(hit.url, "https://www.openstreetmap.org/way/123");
    }

    #[test]
    fn nominatim_says_what_sort_of_place_it_is() {
        let body = r#"[
            {"display_name":"Adventure Playground, Mill Road","category":"leisure","type":"playground"},
            {"display_name":"Some Hall, High Street","category":"historic","type":"yes"},
            {"display_name":"Alton Towers, Staffordshire","class":"tourism","type":"theme_park"},
            {"display_name":"Paris, Île-de-France","category":"boundary","type":"administrative"},
            {"display_name":"12, Mill Road, Cambridge","category":"building","type":"house"},
            {"display_name":"Mill Road, Cambridge","category":"highway","type":"primary"},
            {"display_name":"No category, anywhere","type":"museum"}
        ]"#;
        let hits = parse_nominatim(body).unwrap();
        let types: Vec<Option<&str>> =
            hits.iter().map(|hit| hit.facts.get("type").map(String::as_str)).collect();
        // "yes" is "one of these, of no stated sort", which is no type; and a
        // city, a house or a road is a place on the map, not a sort of outing.
        assert_eq!(types, [Some("Playground"), None, Some("Theme park"), None, None, None, None],);
    }

    #[test]
    fn a_shelf_that_looks_nothing_up_sends_nothing() {
        struct Refuse;
        impl Fetcher for Refuse {
            fn get(&self, request: &Request) -> Result<String> {
                panic!("a people shelf asked {} about somebody", request.url)
            }
        }
        let contacts =
            crate::library::default_kinds().into_iter().find(|k| k.slug == "contact").unwrap();
        assert!(WebSearch::new(Refuse).lookup("Ada Lovelace", &contacts).unwrap().is_empty());
    }

    #[test]
    fn malformed_json_is_an_error_and_never_a_panic() {
        for source in [Source::Wikipedia, Source::OpenLibrary, Source::ITunes, Source::Nominatim] {
            assert!(source.parse("not json at all").is_err(), "{source:?}");
            // Well-formed JSON of the wrong shape is empty, not an error:
            // "no results" is a normal answer.
            assert!(source.parse("{}").unwrap().is_empty(), "{source:?}");
        }
    }

    #[test]
    fn only_web_addresses_survive_a_result() {
        // The bug this exists to prevent: a `file://` href in a result being
        // handed to a fetcher and read off disk.
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "", "  "] {
            assert!(http_url(bad).is_err(), "{bad} should be refused");
        }
        assert_eq!(http_url(" https://example.org/x ").unwrap(), "https://example.org/x");
    }

    #[test]
    fn applying_metadata_fills_gaps_and_never_argues() {
        let books = kind("book");
        let mut item = Item::new(books.id, "dune");
        item.notes = "Lent to Ana.".into();
        item.rating = Some(90);
        item.status = ItemStatus::Active;
        item.facts.insert("author".into(), "F. Herbert".into());

        let hit = SearchResult {
            title: "Dune".into(),
            creator: "Frank Herbert".into(),
            year: Some(1965),
            summary: "A desert planet.".into(),
            rating: Some(84),
            rating_count: Some(1204),
            url: "https://openlibrary.org/works/OL893415W".into(),
            image_url: "https://covers.example/1.jpg".into(),
            facts: BTreeMap::from([
                ("author".into(), "Frank Herbert".into()),
                ("pages".into(), "412".into()),
                // A fact this kind has no field for is dropped rather than
                // stored somewhere the interface cannot show or delete it.
                ("runtime".into(), "156".into()),
            ]),
            source: "openLibrary".into(),
            ..Default::default()
        };
        apply(&hit, &books, &mut item, false);

        assert_eq!(item.title, "dune", "a title you typed must survive a lookup");
        assert_eq!(item.notes, "Lent to Ana.", "your notes are not metadata");
        assert_eq!(item.rating, Some(90), "your rating is yours");
        assert_eq!(item.status, ItemStatus::Active);
        assert_eq!(item.facts["author"], "F. Herbert", "a fact you wrote must survive");
        assert_eq!(item.facts.get("pages").map(String::as_str), Some("412"), "a gap is filled");
        assert!(!item.facts.contains_key("runtime"), "a book has no runtime field");
        assert_eq!(item.creator, "Frank Herbert");
        assert_eq!(item.year, Some(1965));
        assert_eq!(item.summary, "A desert planet.");
        assert_eq!(item.external.len(), 1);
        assert_eq!(item.external[0].score, 84);
        assert_eq!(item.external[0].source, "Open Library");
        assert_eq!(item.links.len(), 1);
    }

    #[test]
    fn a_second_lookup_replaces_rather_than_stacking() {
        let books = kind("book");
        let mut item = Item::new(books.id, "Dune");
        let mut hit = SearchResult {
            title: "Dune".into(),
            rating: Some(80),
            url: "https://openlibrary.org/works/OL1W".into(),
            source: "openLibrary".into(),
            ..Default::default()
        };
        apply(&hit, &books, &mut item, false);
        hit.rating = Some(84);
        apply(&hit, &books, &mut item, false);
        assert_eq!(item.external.len(), 1, "one source, one rating row");
        assert_eq!(item.external[0].score, 84);
        assert_eq!(item.links.len(), 1, "the same address was linked twice");
    }

    #[test]
    fn an_explicit_refetch_may_overwrite_metadata_but_never_your_opinion() {
        let books = kind("book");
        let mut item = Item::new(books.id, "dune");
        item.creator = "F. Herbert".into();
        item.notes = "Lent to Ana.".into();
        item.rating = Some(90);
        let hit = SearchResult {
            title: "Dune".into(),
            creator: "Frank Herbert".into(),
            source: "openLibrary".into(),
            ..Default::default()
        };
        apply(&hit, &books, &mut item, true);
        assert_eq!(item.title, "Dune");
        assert_eq!(item.creator, "Frank Herbert");
        assert_eq!(item.notes, "Lent to Ana.", "notes are never metadata");
        assert_eq!(item.rating, Some(90), "your rating is never metadata");
    }

    #[test]
    fn the_facade_runs_a_search_end_to_end() {
        let web = WebSearch::new(Canned(DDG));
        let hits = web.search(&SearchRequest::new("dune").on(Source::Web)).unwrap();
        assert_eq!(hits.len(), 2);
        // And honours the limit even when the source over-delivers.
        let one = web.search(&SearchRequest::new("dune").on(Source::Web).limit(1)).unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn a_lookup_falls_back_to_the_web_when_its_own_source_is_empty() {
        // Open Library does not know about a self-published pamphlet, and
        // "no results" when the web has plenty is the failure people
        // remember.
        let hits = WebSearch::new(Empty).lookup("a pamphlet", &kind("book")).unwrap();
        assert_eq!(hits.len(), 2, "the web fallback did not run");
        assert_eq!(hits[0].source, "web");
    }

    #[test]
    fn a_long_blurb_is_cut_on_a_word_and_marked() {
        let long = "word ".repeat(200);
        let cut = trim_summary(&long);
        assert!(cut.chars().count() < 500);
        assert!(cut.ends_with('\u{2026}'));
        assert!(!cut.contains("wor\u{2026}"), "cut mid-word: {cut}");
        // Something short is left exactly alone.
        assert_eq!(trim_summary("  A desert planet.  "), "A desert planet.");
    }

    #[test]
    fn entities_decode_in_the_right_order() {
        // `&amp;lt;` is a literal "&lt;", not a "<". Decoding the ampersand
        // first would turn one into the other.
        assert_eq!(decode_entities("a &amp;lt; b"), "a &lt; b");
        assert_eq!(decode_entities("Fear &amp; Loathing"), "Fear & Loathing");
        assert_eq!(decode_entities("it&#39;s"), "it's");
    }

    #[test]
    fn percent_decoding_survives_malformed_input() {
        assert_eq!(decode_percent("https%3A%2F%2Fx.org"), "https://x.org");
        assert_eq!(decode_percent("100%"), "100%");
        assert_eq!(decode_percent("a%zz"), "a%zz");
        assert_eq!(decode_percent("a+b"), "a b");
    }

    #[test]
    fn a_catalogue_falls_back_through_wikipedia_before_the_open_web() {
        // The point of the middle step: a catalogue miss should still answer
        // with the *work* rather than with a list of shops selling it.
        let books = SearchRequest::for_kind("dune", &kind("book"));
        let attempts = books.attempts();
        assert_eq!(
            attempts.iter().map(|a| a.source).collect::<Vec<_>>(),
            vec![Source::OpenLibrary, Source::Wikipedia, Source::Web],
        );
        for attempt in &attempts {
            assert_eq!(attempt.limit, books.limit, "the limit must carry down the chain");
            assert_eq!(attempt.hint, "book", "so a later source can still tell what was meant");
        }

        // Wikipedia is *not* inserted where an encyclopaedia is the wrong
        // answer to the question. A place that OpenStreetMap does not know
        // wants the restaurant's website, not an article about the street.
        let place = SearchRequest::for_kind("bar termini", &kind("restaurant"));
        assert_eq!(
            place.attempts().iter().map(|a| a.source).collect::<Vec<_>>(),
            vec![Source::Nominatim, Source::Web],
        );

        // Nor after Wikipedia itself, which would be the same request twice.
        let game = SearchRequest::new("outer wilds").on(Source::Wikipedia).about("game");
        assert_eq!(
            game.attempts().iter().map(|a| a.source).collect::<Vec<_>>(),
            vec![Source::Wikipedia, Source::Web],
        );

        // A request that is already a web search has nothing to fall back to.
        assert_eq!(SearchRequest::new("x").attempts().len(), 1);
    }

    #[test]
    fn every_fallback_says_what_kind_of_thing_it_is_looking_for() {
        // "dune" over a films shelf is a novel, a record and a tour as well.
        // Neither fallback has a structured field to disambiguate on, so the
        // one word goes into both queries -- including Wikipedia's, whose
        // whole reason for being in the chain is to answer with the right
        // work rather than with the most famous one of that name.
        let films = SearchRequest::for_kind("dune", &kind("film"));
        let attempts = films.attempts();
        assert_eq!(attempts[0].query, "dune", "the shelf's own source is asked what was typed");
        for fallback in &attempts[1..] {
            assert_eq!(
                fallback.query, "dune film",
                "{:?} was asked the bare word",
                fallback.source
            );
        }

        // Not twice, when the person already typed it.
        let typed = SearchRequest::for_kind("Dune the Film", &kind("film"));
        assert!(typed.attempts().iter().all(|a| a.query == "Dune the Film"));

        // And never for a search asked for on a source explicitly: there,
        // what was typed is what was meant.
        let plain = SearchRequest::new("dune film").about("film");
        assert_eq!(plain.attempts().len(), 1);
        assert_eq!(plain.attempts()[0].query, "dune film");
    }

    #[test]
    fn a_search_request_round_trips_through_json() {
        let req = SearchRequest::new("dune").on(Source::OpenLibrary).about("book").limit(5);
        let round: SearchRequest =
            serde_json::from_slice(&serde_json::to_vec(&req).unwrap()).unwrap();
        assert_eq!(round, req);
        for s in Source::ALL {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.slug()));
        }
    }
}
