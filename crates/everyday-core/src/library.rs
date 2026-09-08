//! The library domain: things you are interested in, and what you did with
//! them.
//!
//! This is the fourth domain in the vault. It holds a reading list, a watch
//! list, a shelf of games, the restaurants worth going back to and the
//! recipes worth cooking again — the *metadata* about them, never the things
//! themselves. Nothing here stores a book; it stores that you want to read
//! one, that you started it in March, and what you thought when you finished.
//!
//! ```text
//!   Kind ────── Item ────── LogEntry
//!  (Books)     (Dune)      started 3 Mar, finished 2 Apr, ★★★★½
//! ```
//!
//! # Three decisions worth knowing about
//!
//! **A kind is data, not a variant.** [`Kind`] is a record in the vault with
//! a name, an icon, a set of extra [`fields`](FieldDef) and the verbs it
//! prefers ("reading" rather than "in progress"). The application seeds ten
//! of them — see [`default_kinds`] — and the person using it can edit those
//! or add "Board games", "Wines", "Podcasts I owe someone an opinion on"
//! without a release. A closed Rust enum would have made every new category
//! a schema migration, and there is no end to the list of things a person
//! can be interested in.
//!
//! Contrast [`ItemStatus`], which *is* a closed set, for the same reason
//! [`TaskStatus`](crate::task::TaskStatus) is: "how long do things sit on my
//! wishlist" is a question worth being able to ask across every kind at
//! once, and per-kind statuses would make it unanswerable. What varies per
//! kind is the *label* on a status, not the status.
//!
//! **What happened is a record, not a field.** An [`Item`] does not have a
//! "date watched". It has any number of [`LogEntry`] rows pointing at it,
//! each dated. That is what makes "what did I read last year", "how many
//! times have I been back to that restaurant" and "I re-watched it and liked
//! it less this time" expressible at all — one field would overwrite the
//! previous answer every time. It is the same shape, and the same argument,
//! as [`TimeBlock`](crate::task::TimeBlock) in the task domain.
//!
//! **Ratings are stored out of 100 and shown out of five.** A `u8` in
//! `0..=100` holds every scale anyone might want to import — five stars, ten
//! points, a percentage from a review site — without the app having to pick
//! one and lose precision on the others. [`stars`] and [`from_stars`] are the
//! conversion the interface uses; see them for why halves survive the round
//! trip and thirds do not.
//!
//! # Where the pictures come from
//!
//! A cover is a [`BlobId`](crate::BlobId): the same content-addressed,
//! chunk-encrypted store that holds photographs in a journal entry. It is
//! *downloaded once and kept*, never hot-linked, which is not a caching
//! decision but a privacy one — a `<img src="https://covers…">` in the
//! webview would tell a stranger's server which books are on your shelf
//! every time you opened the app. See [`crate::websearch`] for the fetch,
//! and note that the webview's content security policy does not permit the
//! alternative even if this changed its mind.

use crate::id::{BlobId, ItemId, KindId, LogId};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Kinds ────────────────────────────────────────────────────────────────

/// What a kind calls the three states an item can be in.
///
/// "In progress" is what a database would say. A person says they are
/// *reading* a book, *watching* a series and *playing* a game, and an
/// application that insists on one word for all three reads like a form.
/// Four strings is a cheap price for the interface never having to be
/// generic about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verbs {
    /// The wishlist, in this kind's language: "To read", "To watch".
    pub wishlist: String,
    /// Underway: "Reading", "Watching", "Playing".
    pub active: String,
    /// Finished: "Read", "Watched", "Played".
    pub done: String,
    /// The past participle used in a sentence — "Read on 4 March". Lower
    /// case, because it lands mid-line.
    pub log: String,
}

impl Verbs {
    pub fn new(wishlist: &str, active: &str, done: &str, log: &str) -> Self {
        Self {
            wishlist: wishlist.into(),
            active: active.into(),
            done: done.into(),
            log: log.into(),
        }
    }

    /// The generic set, for a kind somebody added themselves and did not
    /// bother to give words to.
    pub fn generic() -> Self {
        Self::new("To try", "Underway", "Done", "finished")
    }

    pub fn for_status(&self, status: ItemStatus) -> &str {
        match status {
            ItemStatus::Wishlist => &self.wishlist,
            ItemStatus::Active => &self.active,
            ItemStatus::Paused => "Paused",
            ItemStatus::Done => &self.done,
            ItemStatus::Abandoned => "Given up",
        }
    }
}

/// How one of a kind's extra fields is written.
///
/// Presentation, not validation: everything is stored as a string in
/// [`Item::facts`] whatever this says. It decides which control the detail
/// panel draws and how the value is aligned, and nothing else — a `Number`
/// field holding "circa 1890" is a slightly untidy record, not a broken one,
/// and refusing it would be the interface arguing with somebody about their
/// own shelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldType {
    #[default]
    Text,
    /// A paragraph rather than a line: an address, a note on the wine.
    Multiline,
    Number,
    /// `YYYY-MM-DD`.
    Date,
    /// Rendered as a link, opened in the system browser.
    Url,
}

impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::Multiline => "multiline",
            FieldType::Number => "number",
            FieldType::Date => "date",
            FieldType::Url => "url",
        }
    }
}

/// One extra field a kind adds to its items.
///
/// `key` is what indexes [`Item::facts`] and is stable; `label` is what the
/// interface prints and can be changed freely. Keeping them apart is what
/// lets somebody rename "Author" to "Written by" without every book losing
/// its author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub field_type: FieldType,
    /// Greyed-out example text. Worth more than a tooltip on a field like
    /// "ISBN" that people fill in once a year.
    #[serde(default)]
    pub placeholder: String,
}

impl FieldDef {
    pub fn new(key: &str, label: &str, field_type: FieldType) -> Self {
        Self { key: key.into(), label: label.into(), field_type, placeholder: String::new() }
    }

    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.into();
        self
    }
}

/// A category of thing you keep track of.
///
/// See the module docs for why this is a record rather than an enum. The
/// interface lists these down the sidebar the way it lists journals and
/// projects, and every [`Item`] belongs to exactly one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kind {
    pub id: KindId,
    /// Stable machine name — `book`, `film`, `restaurant`. What the metadata
    /// lookups key on, and what quick capture matches when you type
    /// `Dune #book`. Distinct from `name`, which is free to be renamed.
    pub slug: String,
    /// Plural, because it names a shelf: "Books", "Films".
    pub name: String,
    /// Singular, because it names one thing and the buttons say "Add a
    /// book". Deriving this by chopping an "s" works for books and not for
    /// anything else.
    pub singular: String,
    /// A short emoji or glyph, as a journal and a project both carry.
    pub icon: String,
    /// `#rrggbb`. Tints the shelf and every card on it.
    pub color: String,
    pub verbs: Verbs,
    /// Extra fields, in the order the detail panel draws them.
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    /// Which metadata source enriches this kind, by
    /// [`Source`](crate::websearch::Source) slug.
    ///
    /// A string rather than the enum, deliberately. This record is written
    /// into the vault; an older build reading a kind whose source it has
    /// never heard of should fall back to a plain web search, not fail to
    /// deserialize the whole shelf. See
    /// [`Source::from_slug`](crate::websearch::Source::from_slug), which
    /// treats anything unknown as the general case.
    #[serde(default)]
    pub source: String,
    /// What progress through one of these is counted in: "page", "episode",
    /// "minute". Empty means this kind has no notion of being part-way
    /// through, which is the right answer for a restaurant.
    #[serde(default)]
    pub progress_unit: String,
    /// Manual ordering in the sidebar; ties broken by `name`.
    #[serde(default)]
    pub sort_order: i32,
    /// Seeded by the application rather than added by hand. Only affects
    /// what the interface offers to do about it — a built-in kind can be
    /// hidden but warns before it is deleted, because deleting it takes
    /// everything on the shelf.
    #[serde(default)]
    pub builtin: bool,
    /// Unticking hides the shelf without deleting it, exactly as unticking a
    /// calendar hides it without unsubscribing.
    #[serde(default = "yes")]
    pub visible: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn yes() -> bool {
    true
}

impl Kind {
    pub fn new(slug: &str, name: &str, singular: &str) -> Self {
        let now = Timestamp::now();
        Self {
            id: KindId::new(),
            slug: slug.into(),
            name: name.into(),
            singular: singular.into(),
            icon: "\u{1f516}".into(), // bookmark
            color: DEFAULT_KIND_COLORS[0].to_string(),
            verbs: Verbs::generic(),
            fields: Vec::new(),
            source: String::new(),
            progress_unit: String::new(),
            sort_order: 0,
            builtin: false,
            visible: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_icon(mut self, icon: &str) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn with_color(mut self, color: &str) -> Self {
        self.color = color.into();
        self
    }

    pub fn with_verbs(mut self, verbs: Verbs) -> Self {
        self.verbs = verbs;
        self
    }

    pub fn with_fields(mut self, fields: Vec<FieldDef>) -> Self {
        self.fields = fields;
        self
    }

    pub fn with_source(mut self, source: &str) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_progress(mut self, unit: &str) -> Self {
        self.progress_unit = unit.into();
        self
    }

    /// Can you be part-way through one of these?
    pub fn tracks_progress(&self) -> bool {
        !self.progress_unit.trim().is_empty()
    }

    /// The label for one of this kind's extra fields, or the key itself if
    /// the field has since been removed from the kind.
    ///
    /// Facts outlive field definitions on purpose: deleting "ISBN" from the
    /// Books kind must not silently delete the ISBN off four hundred books,
    /// because putting the field back has to bring them with it.
    pub fn field_label<'a>(&'a self, key: &'a str) -> &'a str {
        self.fields.iter().find(|f| f.key == key).map_or(key, |f| f.label.as_str())
    }
}

/// Shelf accents. The journal palette, so one vault has one set of colours.
pub const DEFAULT_KIND_COLORS: &[&str] = crate::model::DEFAULT_JOURNAL_COLORS;

/// The kinds a new vault starts with.
///
/// Ten, covering what the brief asked for, and every one of them editable
/// and deletable afterwards. They are seeded rather than hard-coded so that
/// the first run is useful and the second run is yours: the application has
/// an opinion about where to start and none at all about where you end up.
///
/// `sort_order` is assigned by position here, so the sidebar's initial order
/// is the order of this list.
pub fn default_kinds() -> Vec<Kind> {
    let mut kinds = vec![
        Kind::new("book", "Books", "Book")
            .with_icon("\u{1f4d9}")
            .with_verbs(Verbs::new("To read", "Reading", "Read", "read"))
            .with_source("openLibrary")
            .with_progress("page")
            .with_fields(vec![
                FieldDef::new("author", "Author", FieldType::Text),
                FieldDef::new("pages", "Pages", FieldType::Number),
                FieldDef::new("publisher", "Publisher", FieldType::Text),
                FieldDef::new("isbn", "ISBN", FieldType::Text).with_placeholder("9780441013593"),
            ]),
        Kind::new("film", "Films", "Film")
            .with_icon("\u{1f3ac}")
            .with_verbs(Verbs::new("To watch", "Watching", "Watched", "watched"))
            .with_source("itunes")
            .with_progress("minute")
            .with_fields(vec![
                FieldDef::new("director", "Director", FieldType::Text),
                FieldDef::new("runtime", "Runtime (min)", FieldType::Number),
                FieldDef::new("genre", "Genre", FieldType::Text),
            ]),
        Kind::new("series", "Series", "Series")
            .with_icon("\u{1f4fa}")
            .with_verbs(Verbs::new("To watch", "Watching", "Watched", "watched"))
            .with_source("itunes")
            .with_progress("episode")
            .with_fields(vec![
                FieldDef::new("creator", "Created by", FieldType::Text),
                FieldDef::new("seasons", "Seasons", FieldType::Number),
                FieldDef::new("network", "Network", FieldType::Text),
            ]),
        Kind::new("music", "Music", "Album")
            .with_icon("\u{1f3b5}")
            .with_verbs(Verbs::new("To hear", "Listening", "Heard", "listened to"))
            .with_source("itunes")
            .with_fields(vec![
                FieldDef::new("artist", "Artist", FieldType::Text),
                FieldDef::new("label", "Label", FieldType::Text),
                FieldDef::new("tracks", "Tracks", FieldType::Number),
            ]),
        Kind::new("game", "Games", "Game")
            .with_icon("\u{1f3ae}")
            .with_verbs(Verbs::new("To play", "Playing", "Played", "played"))
            .with_source("wikipedia")
            .with_progress("hour")
            .with_fields(vec![
                FieldDef::new("developer", "Developer", FieldType::Text),
                FieldDef::new("platform", "Platform", FieldType::Text),
            ]),
        Kind::new("article", "Articles", "Article")
            .with_icon("\u{1f4f0}")
            .with_verbs(Verbs::new("To read", "Reading", "Read", "read"))
            .with_source("web")
            .with_fields(vec![
                FieldDef::new("publication", "Publication", FieldType::Text),
                FieldDef::new("author", "Author", FieldType::Text),
                FieldDef::new("url", "Address", FieldType::Url),
            ]),
        Kind::new("podcast", "Podcasts", "Episode")
            .with_icon("\u{1f3a7}")
            .with_verbs(Verbs::new("To hear", "Listening", "Heard", "listened to"))
            .with_source("itunes")
            .with_progress("minute")
            .with_fields(vec![
                FieldDef::new("show", "Show", FieldType::Text),
                FieldDef::new("url", "Address", FieldType::Url),
            ]),
        Kind::new("restaurant", "Restaurants", "Restaurant")
            .with_icon("\u{1f37d}\u{fe0f}")
            .with_verbs(Verbs::new("To try", "Booked", "Been", "ate at"))
            .with_source("nominatim")
            .with_fields(vec![
                FieldDef::new("cuisine", "Cuisine", FieldType::Text),
                FieldDef::new("address", "Address", FieldType::Multiline),
                FieldDef::new("phone", "Telephone", FieldType::Text),
                FieldDef::new("url", "Website", FieldType::Url),
            ]),
        Kind::new("recipe", "Recipes", "Recipe")
            .with_icon("\u{1f957}")
            .with_verbs(Verbs::new("To cook", "Cooking", "Cooked", "cooked"))
            .with_source("web")
            .with_fields(vec![
                FieldDef::new("source", "From", FieldType::Text),
                FieldDef::new("serves", "Serves", FieldType::Number),
                FieldDef::new("time", "Time (min)", FieldType::Number),
                FieldDef::new("url", "Address", FieldType::Url),
            ]),
        Kind::new("place", "Places", "Place")
            .with_icon("\u{1f5fa}\u{fe0f}")
            .with_verbs(Verbs::new("To visit", "Planning", "Visited", "visited"))
            .with_source("nominatim")
            .with_fields(vec![
                FieldDef::new("country", "Country", FieldType::Text),
                FieldDef::new("address", "Address", FieldType::Multiline),
            ]),
    ];
    for (i, kind) in kinds.iter_mut().enumerate() {
        kind.builtin = true;
        kind.sort_order = i as i32;
        kind.color = DEFAULT_KIND_COLORS[i % DEFAULT_KIND_COLORS.len()].to_string();
    }
    kinds
}

// ── Items ────────────────────────────────────────────────────────────────

/// Where an item is in its life.
///
/// A closed set, unlike [`Kind`], and for the reason given in the module
/// docs: this is the axis every cross-kind question is asked along. What a
/// kind gets to decide is the *word* — see [`Verbs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    /// Noted down. The whole point of the app: somewhere to put a
    /// recommendation before it evaporates.
    #[default]
    Wishlist,
    /// Reading it, watching it, playing it, now.
    Active,
    /// Started and set aside. Distinct from [`Abandoned`](Self::Abandoned):
    /// you mean to come back to this one.
    Paused,
    Done,
    /// Given up on. Kept rather than deleted, because "I tried it and it was
    /// not for me" is the answer you want the next time somebody recommends
    /// it, and a deleted row cannot give it.
    Abandoned,
}

impl ItemStatus {
    /// Left to right, as the shelf's filter bar draws them.
    pub const ALL: [ItemStatus; 5] = [
        ItemStatus::Wishlist,
        ItemStatus::Active,
        ItemStatus::Paused,
        ItemStatus::Done,
        ItemStatus::Abandoned,
    ];

    /// Is this still something you intend to get to? `Abandoned` is not.
    pub fn is_open(self) -> bool {
        matches!(self, ItemStatus::Wishlist | ItemStatus::Active | ItemStatus::Paused)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Wishlist => "wishlist",
            ItemStatus::Active => "active",
            ItemStatus::Paused => "paused",
            ItemStatus::Done => "done",
            ItemStatus::Abandoned => "abandoned",
        }
    }
}

/// Somebody else's score, as a source gave it.
///
/// Normalised to `0..=100` on the way in so that four stars from one site
/// and 82% from another can sit on the same row and be compared. `count`
/// carries how many people it is an average of, because 9.4 from eleven
/// people is a different fact from 9.4 from eleven thousand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRating {
    /// Who said so: "Open Library", "iTunes".
    pub source: String,
    /// `0..=100`.
    pub score: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default)]
    pub url: String,
}

/// A link kept beside an item: where it was found, where to buy it, where
/// the recipe actually is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// How far through you are.
///
/// `unit` is copied from the [`Kind`] at the moment progress is first
/// recorded rather than read from it live, so that changing a kind from
/// pages to minutes does not silently relabel every book on the shelf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub position: u32,
    /// The finishing line, if it is known. A series you are watching as it
    /// airs has no total, and guessing one would be worse than saying
    /// "episode 6" and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    pub unit: String,
}

impl Progress {
    pub fn new(position: u32, total: Option<u32>, unit: impl Into<String>) -> Self {
        Self { position, total, unit: unit.into() }
    }

    /// Fraction done in `0.0..=1.0`, or `None` when there is no total to be
    /// a fraction of.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(total) if total > 0 => Some((self.position as f32 / total as f32).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

/// One thing you are interested in.
///
/// Every field except `title` and `kind_id` is optional in practice, because
/// the fastest way to add something is to type its name and press Enter and
/// let the metadata arrive later — or never.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    pub kind_id: KindId,
    pub title: String,
    /// A subtitle, a tagline, the name of the series a book is book three of.
    #[serde(default)]
    pub subtitle: String,
    /// Author, director, artist, developer, chef. One field rather than one
    /// per kind: it is what the card prints under the title and what the eye
    /// scans a shelf for, and a kind that wants finer distinctions has
    /// [`Kind::fields`] for them.
    #[serde(default)]
    pub creator: String,
    /// Publication or release year. Signed, because history did not start in
    /// 1970 and a place can be older than that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i16>,
    #[serde(default)]
    pub status: ItemStatus,
    /// Yours, `0..=100`. See [`stars`] for the conversion the interface uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external: Vec<ExternalRating>,
    /// The cover, downloaded into the vault's blob store. See the module
    /// docs for why it is not a URL the webview loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<BlobId>,
    /// Where the cover came from, kept so it can be fetched again if the
    /// blob is ever collected — and so the interface can say "no picture
    /// yet" rather than "no picture" while one is on its way.
    #[serde(default)]
    pub cover_url: String,
    /// The blurb, as a metadata source wrote it. Never edited by the app;
    /// what *you* think goes in `notes`, and keeping the two apart is what
    /// lets a re-fetch replace one without touching the other.
    #[serde(default)]
    pub summary: String,
    /// Your notes. Plain text, for the reason a task's are: it keeps capture
    /// quick and keeps the shelf out of the attachment garbage collector.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The kind's extra fields, keyed by [`FieldDef::key`].
    ///
    /// A `BTreeMap` rather than a `HashMap` so the serialised form is stable:
    /// two saves of an unchanged item produce identical bytes, which is what
    /// keeps a conflict check honest and a backup diff readable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Starred. The same affordance a journal entry has, meaning the same
    /// thing: this one, out of all of them.
    #[serde(default)]
    pub favourite: bool,
    /// When you started and finished, as *dates you chose*.
    ///
    /// Denormalised from the log on purpose. The log is the truth and a
    /// re-read adds to it; these two are the summary the card prints and the
    /// shelf sorts on, and computing them from an unbounded list of entries
    /// on every draw is what a "started" column exists to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_on: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_on: Option<Date>,
    /// Which metadata source filled this in, by
    /// [`Source`](crate::websearch::Source) slug. Empty for something typed
    /// in by hand, which is most of them.
    #[serde(default)]
    pub source: String,
    /// Manual ordering within a shelf; ties broken by the query's sort.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Item {
    pub fn new(kind_id: KindId, title: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: ItemId::new(),
            kind_id,
            title: title.into(),
            subtitle: String::new(),
            creator: String::new(),
            year: None,
            status: ItemStatus::Wishlist,
            rating: None,
            external: Vec::new(),
            cover: None,
            cover_url: String::new(),
            summary: String::new(),
            notes: String::new(),
            tags: Vec::new(),
            facts: BTreeMap::new(),
            links: Vec::new(),
            progress: None,
            favourite: false,
            started_on: None,
            finished_on: None,
            source: String::new(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Move to `status`, keeping the two denormalised dates honest.
    ///
    /// Only ever *fills in* a blank date; it never overwrites one you set.
    /// Marking a book done on the day you happen to be tidying the shelf
    /// should not rewrite the day you actually finished it.
    pub fn set_status(&mut self, status: ItemStatus, today: Date) {
        self.status = status;
        match status {
            ItemStatus::Active if self.started_on.is_none() => self.started_on = Some(today),
            ItemStatus::Done => {
                self.started_on.get_or_insert(today);
                self.finished_on.get_or_insert(today);
            }
            // Back to the wishlist is a decision that this was never started,
            // so the dates go with it -- otherwise an item bounced through
            // "done" by a misclick keeps a finish date it never earned.
            ItemStatus::Wishlist => {
                self.started_on = None;
                self.finished_on = None;
            }
            _ => {}
        }
        self.updated_at = Timestamp::now();
    }

    /// The best guess at a headline byline: the creator, then the year.
    pub fn byline(&self) -> String {
        match (self.creator.trim(), self.year) {
            ("", None) => String::new(),
            ("", Some(y)) => y.to_string(),
            (c, None) => c.to_string(),
            (c, Some(y)) => format!("{c} \u{00b7} {y}"),
        }
    }

    /// Everything worth matching a text filter against.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(128);
        for part in [
            self.title.as_str(),
            self.subtitle.as_str(),
            self.creator.as_str(),
            self.summary.as_str(),
            self.notes.as_str(),
        ] {
            out.push_str(part);
            out.push('\n');
        }
        for tag in &self.tags {
            out.push_str(tag);
            out.push('\n');
        }
        // Facts are searchable too: "the Thai place on Mill Road" is a
        // question about an address, and an address is a fact.
        for value in self.facts.values() {
            out.push_str(value);
            out.push('\n');
        }
        out
    }
}

// ── The log ──────────────────────────────────────────────────────────────

/// What sort of thing happened.
///
/// The set is short on purpose. A log is meant to be written in passing —
/// one tap on a card — and an app that first asks you to classify the event
/// is an app whose log stays empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogEvent {
    Started,
    /// Got somewhere: page 143, episode 4. Carries
    /// [`LogEntry::position`].
    Progress,
    /// Finished it. This is the row "what did I watch in March" counts.
    #[default]
    Finished,
    /// Went back to it — a re-read, a second visit, the album you have had
    /// on all week. Distinct from a second `Finished` so that "how many
    /// times" and "when did I first" are both answerable.
    Revisited,
    /// A thought, on a day, attached to nothing else.
    Note,
    /// Set aside or given up. Kept in the log because *when* you stopped is
    /// as much a fact as when you started.
    Stopped,
}

impl LogEvent {
    pub const ALL: [LogEvent; 6] = [
        LogEvent::Started,
        LogEvent::Progress,
        LogEvent::Finished,
        LogEvent::Revisited,
        LogEvent::Note,
        LogEvent::Stopped,
    ];

    /// Does this row mean "I got to the end of it"? Both of the ones that
    /// do, so a count of them is a count of times through.
    pub fn is_completion(self) -> bool {
        matches!(self, LogEvent::Finished | LogEvent::Revisited)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LogEvent::Started => "started",
            LogEvent::Progress => "progress",
            LogEvent::Finished => "finished",
            LogEvent::Revisited => "revisited",
            LogEvent::Note => "note",
            LogEvent::Stopped => "stopped",
        }
    }
}

/// One occasion on which you did something about an item.
///
/// See the module docs: this exists so that "what did I read when" has an
/// answer that survives reading the thing twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: LogId,
    pub item_id: ItemId,
    #[serde(default)]
    pub event: LogEvent,
    /// The day it happened, in `tz`. A civil date rather than an instant:
    /// "I finished it on Tuesday" is what a person knows, and pinning that
    /// to a clock time nobody recorded would be inventing precision.
    pub date: Date,
    /// IANA time zone the date was decided in, so a log written in Tokyo
    /// still reads as the day it was when it was written.
    pub tz: String,
    #[serde(default)]
    pub note: String,
    /// What you thought *at the time*. The item carries your current
    /// verdict; this carries the one you had on the day, and a re-read that
    /// changes your mind leaves both on the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// Where you got to, for a [`LogEvent::Progress`] row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    /// How long you spent, if you care to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl LogEntry {
    pub fn new(item_id: ItemId, event: LogEvent, date: Date, tz: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: LogId::new(),
            item_id,
            event,
            date,
            tz: tz.into(),
            note: String::new(),
            rating: None,
            position: None,
            minutes: None,
            created_at: now,
            updated_at: now,
        }
    }
}

// ── Ratings ──────────────────────────────────────────────────────────────

/// Turn a stored `0..=100` score into stars out of five.
///
/// Rounded to the nearest half, which is the granularity the interface
/// offers — a five-star control with half steps has ten positions, and ten
/// positions is about as fine as anyone's opinion of a film actually is.
pub fn stars(score: u8) -> f32 {
    let raw = f32::from(score.min(100)) / 20.0;
    (raw * 2.0).round() / 2.0
}

/// The inverse: a half-star position back to the stored scale.
///
/// Exact for every half step, which is what makes the round trip stable —
/// set four and a half stars, reload, and the control is still on four and a
/// half rather than having drifted a pixel.
pub fn from_stars(stars: f32) -> u8 {
    (stars.clamp(0.0, 5.0) * 20.0).round() as u8
}

/// Normalise somebody else's scale onto `0..=100`.
///
/// `max` is what their scale tops out at: 5 for stars, 10 for a points
/// score, 100 for a percentage. A zero or negative `max` is a source we
/// misread, and the honest answer to that is no rating at all rather than a
/// number divided by nothing.
pub fn normalize_rating(score: f64, max: f64) -> Option<u8> {
    if !score.is_finite() || !max.is_finite() || max <= 0.0 || score < 0.0 {
        return None;
    }
    Some(((score / max) * 100.0).round().clamp(0.0, 100.0) as u8)
}

// ── Counts ───────────────────────────────────────────────────────────────

/// What is on one shelf. For the line under its name in the sidebar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindCount {
    pub kind_id: KindId,
    pub items: u64,
    /// Wishlist, active and paused together: everything still ahead of you.
    pub open: u64,
    pub active: u64,
}

/// Counts for the library sidebar and the year-in-review strip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    pub kinds: u64,
    pub items: u64,
    pub wishlist: u64,
    pub active: u64,
    pub done: u64,
    /// Log rows meaning "got to the end of it" dated in the current year.
    pub finished_this_year: u64,
    /// Items rated by you, and the mean of those ratings on the stored
    /// `0..=100` scale. Omitted from the mean, not counted as zero, when you
    /// have not rated anything.
    pub rated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_rating: Option<u8>,
    pub by_kind: Vec<KindCount>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn the_seeded_kinds_are_distinct_and_complete() {
        let kinds = default_kinds();
        let slugs: std::collections::BTreeSet<&str> =
            kinds.iter().map(|k| k.slug.as_str()).collect();
        assert_eq!(slugs.len(), kinds.len(), "two seeded kinds share a slug");
        // The list the brief asked for, by name.
        for want in ["book", "film", "music", "game", "restaurant", "recipe", "place", "article"] {
            assert!(slugs.contains(want), "no seeded kind for {want}");
        }
        assert!(kinds.iter().all(|k| k.builtin), "seeded kinds should say so");
        assert!(kinds.iter().all(|k| !k.singular.is_empty() && !k.name.is_empty()));
        // Sidebar order is the order of the list, not whatever the ids sort
        // to -- ids are minted in a loop and would order by microsecond.
        assert!(kinds.windows(2).all(|w| w[0].sort_order < w[1].sort_order));
    }

    #[test]
    fn a_kind_speaks_its_own_language() {
        let books = default_kinds().into_iter().find(|k| k.slug == "book").unwrap();
        assert_eq!(books.verbs.for_status(ItemStatus::Active), "Reading");
        assert_eq!(books.verbs.for_status(ItemStatus::Done), "Read");
        let games = default_kinds().into_iter().find(|k| k.slug == "game").unwrap();
        assert_eq!(games.verbs.for_status(ItemStatus::Active), "Playing");
    }

    #[test]
    fn a_fact_outlives_the_field_that_named_it() {
        // The bug this guards: deleting a field from a kind silently
        // deleting the value off every item, so putting the field back
        // brings nothing with it.
        let mut books = Kind::new("book", "Books", "Book").with_fields(vec![FieldDef::new(
            "isbn",
            "ISBN",
            FieldType::Text,
        )]);
        assert_eq!(books.field_label("isbn"), "ISBN");
        books.fields.clear();
        assert_eq!(books.field_label("isbn"), "isbn", "an orphaned fact still has a label");
    }

    #[test]
    fn progress_is_a_fraction_only_when_there_is_an_end_in_sight() {
        let half = Progress::new(200, Some(400), "page");
        assert_eq!(half.fraction(), Some(0.5));
        // A series airing weekly has no total, and inventing one would be
        // worse than drawing no bar.
        assert_eq!(Progress::new(6, None, "episode").fraction(), None);
        // Nor does a total of zero divide.
        assert_eq!(Progress::new(1, Some(0), "page").fraction(), None);
        // Past the end reads as done, not as 130%.
        assert_eq!(Progress::new(520, Some(400), "page").fraction(), Some(1.0));
    }

    #[test]
    fn stars_round_trip_through_the_stored_scale() {
        for tenth in 0..=10 {
            let want = tenth as f32 / 2.0;
            assert_eq!(stars(from_stars(want)), want, "{want} stars did not survive");
        }
        assert_eq!(stars(100), 5.0);
        assert_eq!(stars(0), 0.0);
        // Somebody else's 82% lands on the nearest half we can draw.
        assert_eq!(stars(82), 4.0);
        assert_eq!(stars(85), 4.5);
    }

    #[test]
    fn foreign_scales_normalise_onto_one_hundred() {
        assert_eq!(normalize_rating(4.0, 5.0), Some(80));
        assert_eq!(normalize_rating(9.4, 10.0), Some(94));
        assert_eq!(normalize_rating(82.0, 100.0), Some(82));
        // A source we misread gives no rating rather than a nonsense one.
        assert_eq!(normalize_rating(4.0, 0.0), None);
        assert_eq!(normalize_rating(f64::NAN, 5.0), None);
        assert_eq!(normalize_rating(-1.0, 5.0), None);
        // And one that overshoots its own scale is clamped, not believed.
        assert_eq!(normalize_rating(11.0, 10.0), Some(100));
    }

    #[test]
    fn finishing_something_dates_it_without_overwriting_what_you_said() {
        let kind = KindId::new();
        let mut item = Item::new(kind, "Dune");
        item.set_status(ItemStatus::Active, date(2026, 3, 3));
        assert_eq!(item.started_on, Some(date(2026, 3, 3)));

        // Tidying the shelf a month later must not rewrite the start.
        item.set_status(ItemStatus::Done, date(2026, 4, 2));
        assert_eq!(item.started_on, Some(date(2026, 3, 3)), "the start was overwritten");
        assert_eq!(item.finished_on, Some(date(2026, 4, 2)));

        // ...but putting it back on the wishlist means it was never started.
        item.set_status(ItemStatus::Wishlist, date(2026, 4, 3));
        assert_eq!(item.started_on, None);
        assert_eq!(item.finished_on, None);
    }

    #[test]
    fn a_byline_degrades_gracefully() {
        let kind = KindId::new();
        let mut item = Item::new(kind, "Dune");
        assert_eq!(item.byline(), "");
        item.year = Some(1965);
        assert_eq!(item.byline(), "1965");
        item.creator = "Frank Herbert".into();
        assert_eq!(item.byline(), "Frank Herbert \u{00b7} 1965");
        item.year = None;
        assert_eq!(item.byline(), "Frank Herbert");
    }

    #[test]
    fn searchable_text_reaches_the_kind_specific_facts() {
        // "the Thai place on Mill Road" is a question about an address.
        let mut item = Item::new(KindId::new(), "Som Saa");
        item.facts.insert("address".into(), "14 Mill Road, Cambridge".into());
        item.tags = vec!["dinner".into()];
        let text = item.searchable_text().to_lowercase();
        assert!(text.contains("mill road"));
        assert!(text.contains("dinner"));
    }

    #[test]
    fn wire_names_match_the_serde_representation() {
        for s in ItemStatus::ALL {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
        for e in LogEvent::ALL {
            assert_eq!(serde_json::to_string(&e).unwrap(), format!("\"{}\"", e.as_str()));
        }
        for f in [
            FieldType::Text,
            FieldType::Multiline,
            FieldType::Number,
            FieldType::Date,
            FieldType::Url,
        ] {
            assert_eq!(serde_json::to_string(&f).unwrap(), format!("\"{}\"", f.as_str()));
        }
    }

    #[test]
    fn only_the_two_completion_events_count_as_a_time_through() {
        assert!(LogEvent::Finished.is_completion());
        assert!(LogEvent::Revisited.is_completion());
        for e in [LogEvent::Started, LogEvent::Progress, LogEvent::Note, LogEvent::Stopped] {
            assert!(!e.is_completion(), "{e:?} should not count as a time through");
        }
    }

    #[test]
    fn an_item_survives_a_json_round_trip_with_every_field_set() {
        let kind = KindId::new();
        let mut item = Item::new(kind, "Dune");
        item.subtitle = "Book one".into();
        item.creator = "Frank Herbert".into();
        item.year = Some(1965);
        item.status = ItemStatus::Done;
        item.rating = Some(90);
        item.external = vec![ExternalRating {
            source: "Open Library".into(),
            score: 84,
            count: Some(1204),
            url: "https://openlibrary.org/works/OL893415W".into(),
        }];
        item.cover = Some(BlobId::of(b"a cover"));
        item.cover_url = "https://covers.openlibrary.org/b/id/1.jpg".into();
        item.summary = "A desert planet.".into();
        item.notes = "Better than I remembered.".into();
        item.tags = vec!["sci-fi".into()];
        item.facts.insert("isbn".into(), "9780441013593".into());
        item.links = vec![Link { label: "Open Library".into(), url: "https://ol.org".into() }];
        item.progress = Some(Progress::new(412, Some(412), "page"));
        item.favourite = true;
        item.started_on = Some(date(2026, 3, 3));
        item.finished_on = Some(date(2026, 4, 2));
        item.source = "openLibrary".into();

        let round: Item = serde_json::from_slice(&serde_json::to_vec(&item).unwrap()).unwrap();
        assert_eq!(round, item);
    }

    #[test]
    fn a_kind_survives_a_json_round_trip() {
        for kind in default_kinds() {
            let round: Kind = serde_json::from_slice(&serde_json::to_vec(&kind).unwrap()).unwrap();
            assert_eq!(round, kind);
        }
    }

    #[test]
    fn an_items_serialised_form_is_stable_across_saves() {
        // Facts are a BTreeMap so that two saves of an unchanged item make
        // identical bytes. With a HashMap this passes about one run in a
        // thousand, which is the worst way for it to fail.
        let mut item = Item::new(KindId::new(), "Som Saa");
        for (k, v) in [("cuisine", "Thai"), ("phone", "01223"), ("address", "Mill Road")] {
            item.facts.insert(k.into(), v.into());
        }
        let once = serde_json::to_vec(&item).unwrap();
        for _ in 0..16 {
            assert_eq!(serde_json::to_vec(&item).unwrap(), once);
        }
    }
}
