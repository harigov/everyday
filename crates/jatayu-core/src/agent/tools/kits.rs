//! Toolkits: which of the assistant's tools a turn is offered.
//!
//! The whole catalogue, with the tools the service declares for itself, is
//! about two hundred tools and fifty thousand tokens of schema, and every
//! model step reads all of it again. Tool choice degrades well before that --
//! the providers' own guides put the line at twenty to fifty -- and the
//! instructions and memories that make the assistant *this* assistant end up
//! a thin slice between manuals. So a turn is offered a small **core** every
//! time, and the rest as **kits** that join the conversation when it needs
//! them. See `docs/plans/prompt-review.md` §4.
//!
//! This module is the table and nothing else: which tool is in which kit, what
//! each kit is called and says it is for, which words and which screens point
//! at it, and [`KitSet`], the set a turn is offered. *Whether* a kit joins a
//! given turn is the service's question (`jatayu_service::agent::kits`),
//! because it reads the conversation's history, may ask a decision model, and
//! has `load_tools` to answer; everything here is pure and tested offline.
//!
//! # What is core
//!
//! What a person's everyday requests need and what finds things: orientation,
//! tasks, the calendar's reads and writes, notes and the journal, memory, and
//! an earlier conversation. [`Tool::kit`] answers `None` for those, and for
//! every tool a surface adds on its own account (the spoken tools, the desk's,
//! `ask_owner`, `update_plan`, `offer_choices`, `load_tools`): [`kit_of`]
//! answers `None` for a name it does not know, so a tool nobody filed is
//! offered rather than lost.
//!
//! # Why a kit's tools are not split further
//!
//! A kit is a unit the model asks for by name, so it has to be a thing a
//! request names -- "mail", "health", "places" -- rather than a verb. Within a
//! kit nothing is held back: `search_mail` without `read_thread` is a trap.

use serde::{Deserialize, Serialize};

use super::{Domain, Tool, find};
use crate::agent::onscreen::{App, OnScreen, Shown};

/// What a turn's core tools -- the catalogue's, plus the six the service adds --
/// may weigh together, in characters of name, description and argument schema
/// as the provider receives them: about fifteen thousand tokens, against
/// the fifty thousand of everything. Tested here for the catalogue's share and
/// in the service for the whole.
pub const CORE_BUDGET_CHARS: usize = 60_000;

/// The name of the tool that loads a kit mid-conversation. Declared by the
/// service (it needs the turn's own state), named here so that the index
/// below can refer to it and a test can check the two agree.
pub const LOAD_TOOLS: &str = "load_tools";

/// A set of tools that joins a conversation together.
///
/// The order of the variants is the order a turn's tools are offered in, kit
/// after kit, after the core: a conversation's tool list is a stable prefix,
/// and a kit joining moves only what comes after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kit {
    /// Email: searching, reading, drafting, filing and sending.
    Mail,
    /// Health: what a watch measured, medicines, labs, meals, records.
    Health,
    /// Money: bank accounts, transactions, budgets.
    Money,
    /// The Feed: followed sites, newsletters, podcasts, Up Next, and the
    /// memory of what was read and heard.
    Feed,
    /// Files and documents, photos, and the reader.
    Files,
    /// The library -- books, films, recipes, drinks -- and the people in it.
    Library,
    /// Habits and measurements, and their targets.
    Trackers,
    /// Areas, goals, and where the time went.
    Goals,
    /// Routines and daily briefs.
    Routines,
    /// Research, watches and longer projects that run on their own.
    Background,
    /// Meeting notes, transcripts, briefs and talking points.
    Meetings,
    /// Time blocks, and deleting events.
    Time,
    /// Places nearby and the way there.
    Places,
    /// Illustrations, generated images and picture search.
    Pictures,
    /// A real browser to drive.
    Browser,
    /// The tools of the services the person has connected.
    Connections,
    /// The assistant's own housekeeping: memories to review or forget, skills,
    /// and the proposals it has prepared.
    Assistant,
}

impl Kit {
    /// Every kit, in the order a turn offers them.
    pub const ALL: [Kit; 17] = [
        Kit::Mail,
        Kit::Health,
        Kit::Money,
        Kit::Feed,
        Kit::Files,
        Kit::Library,
        Kit::Trackers,
        Kit::Goals,
        Kit::Routines,
        Kit::Background,
        Kit::Meetings,
        Kit::Time,
        Kit::Places,
        Kit::Pictures,
        Kit::Browser,
        Kit::Connections,
        Kit::Assistant,
    ];

    /// The name the model uses for it in `load_tools`, and the index.
    pub fn name(self) -> &'static str {
        match self {
            Kit::Mail => "mail",
            Kit::Health => "health",
            Kit::Money => "money",
            Kit::Feed => "feed",
            Kit::Files => "files",
            Kit::Library => "library",
            Kit::Trackers => "trackers",
            Kit::Goals => "goals",
            Kit::Routines => "routines",
            Kit::Background => "background",
            Kit::Meetings => "meetings",
            Kit::Time => "time",
            Kit::Places => "places",
            Kit::Pictures => "pictures",
            Kit::Browser => "browser",
            Kit::Connections => "connections",
            Kit::Assistant => "assistant",
        }
    }

    /// The kit a name from `load_tools` means. Lenient about case and
    /// whitespace, which cost nothing to forgive.
    pub fn parse(name: &str) -> Option<Kit> {
        let name = name.trim().to_lowercase();
        Kit::ALL.into_iter().find(|kit| kit.name() == name)
    }

    /// What it is for, in one line: the index's text, and what `load_tools`
    /// answers with. Starts with a verb-less noun phrase so that it reads
    /// after "name:" and again after "loaded".
    pub fn blurb(self) -> &'static str {
        match self {
            Kit::Mail => "search, read, draft, file and send their email",
            Kit::Health => "activity, sleep, glucose, medications, labs and meals",
            Kit::Money => "bank accounts, transactions, budgets",
            Kit::Feed => {
                "followed sites, newsletters and podcasts, Up Next, and what they read, heard \
                 and watched"
            }
            Kit::Files => "their files, photos and documents (PDF, EPUB, Markdown)",
            Kit::Library => "books, films, recipes, drinks and contacts, with their history",
            Kit::Trackers => "habits and measurements, and their targets",
            Kit::Goals => "areas, goals, and where their time went, by area and on screen",
            Kit::Routines => "routines and daily briefs",
            Kit::Background => "research, watches and longer projects that run on their own",
            Kit::Meetings => "meeting notes, transcripts, briefs and talking points",
            Kit::Time => "time blocks, and deleting calendar events",
            Kit::Places => "places nearby and directions",
            Kit::Pictures => "illustrations, generated images and picture search",
            Kit::Browser => "a real browser, for a site that needs clicking and typing",
            Kit::Connections => "the tools of the other services they have connected",
            Kit::Assistant => {
                "your memories to review or forget, your skills, and the proposals \
                you have prepared"
            }
        }
    }

    /// The position in [`Kit::ALL`]: where its tools come in a turn's list,
    /// after the core's, and the bit [`KitSet`] keeps it in.
    pub fn index(self) -> usize {
        self as usize
    }

    fn bit(self) -> u32 {
        1 << self.index()
    }

    /// The words whose presence in a request points at this kit. Lowercase;
    /// a word also matches its plural; several words in a row match as a
    /// phrase. See [`from_words`] for how they are read.
    ///
    /// Chosen so that a false positive costs a few thousand tokens and a false
    /// negative one `load_tools` step: words that are common and mean
    /// something else as often as not ("steps", "watch", "project", "log")
    /// are left out, because the model asks when it needs to.
    fn words(self) -> &'static [&'static str] {
        match self {
            Kit::Mail => &[
                "mail",
                "email",
                "emailed",
                "emailing",
                "e-mail",
                "inbox",
                "gmail",
                "outlook",
                "icloud",
                "newsletter",
                "unread",
                "unsubscribe",
                "spam",
                "snooze",
                "snoozed",
                "attachment",
                "invitation",
                "rsvp",
                "reply to",
                "replied to",
                "reply all",
                "forward this",
                "forward that",
                "forward it",
                "cc'd",
                "draft a reply",
                "draft an email",
                "draft a message",
                "send an email",
                "send a message to",
                "let them know",
                "let him know",
                "let her know",
                "accept the invite",
                "decline the invite",
            ],
            Kit::Health => &[
                "sleep",
                "slept",
                "glucose",
                "a1c",
                "blood sugar",
                "blood pressure",
                "heart rate",
                "hrv",
                "resting heart",
                "step count",
                "workout",
                "medication",
                "medicine",
                "pill",
                "dose",
                "supplement",
                "lab",
                "lab results",
                "blood test",
                "cholesterol",
                "tsh",
                "calorie",
                "protein",
                "carb",
                "meal",
                "ate",
                "nutrition",
                "diet",
                "doctor",
                "diagnosis",
                "symptom",
                "allergy",
                "allergies",
                "vitals",
                "oxygen",
                "bmi",
                "fitness",
                "exercise",
                "health",
                "weigh",
                "weight",
            ],
            Kit::Money => &[
                "spent",
                "spend",
                "spending",
                "budget",
                "bank",
                "transaction",
                "expense",
                "subscription",
                "income",
                "afford",
                "credit card",
                "debit",
                "refund",
                "money",
                "paycheck",
                "salary",
                "net worth",
            ],
            Kit::Feed => &[
                "podcast",
                "episode",
                "article",
                "hacker news",
                "newsletter",
                "rss",
                "feed",
                "up next",
                "listened",
                "listening",
                "transcript of the episode",
                "unfollow",
                "headlines",
                "what i read",
                "what i listened",
                "what i watched",
            ],
            Kit::Files => &[
                "pdf",
                "file",
                "photo",
                "document",
                "epub",
                "folder",
                "download",
                "screenshot",
                "scan",
                "album",
                "spreadsheet",
                "docx",
            ],
            Kit::Library => &[
                "book",
                "film",
                "movie",
                "recipe",
                "drink",
                "cocktail",
                "wine",
                "shelf",
                "library",
                "contact",
                "birthday",
                "anniversary",
                "keep in touch",
                "catch up with",
                "reading list",
                "watchlist",
                "tv show",
                "novel",
                "album",
            ],
            Kit::Trackers => &[
                "habit",
                "tracker",
                "streak",
                "target",
                "log my",
                "times a week",
                "times a day",
                "track my",
                "start tracking",
                "keep track of",
            ],
            Kit::Goals => &[
                "goal",
                "priorities",
                "screen time",
                "screentime",
                "time by area",
                "where did my time go",
                "where my time went",
                "my areas",
            ],
            Kit::Routines => &[
                "routine",
                "every morning",
                "every evening",
                "every day",
                "every week",
                "every monday",
                "every tuesday",
                "every wednesday",
                "every thursday",
                "every friday",
                "every saturday",
                "every sunday",
                "each morning",
                "each day",
                "each week",
                "brief",
                "briefing",
                "digest",
            ],
            Kit::Background => &[
                "research",
                "look into",
                "investigate",
                "keep an eye",
                "watch for",
                "monitor",
                "in the background",
                "let me know when",
                "tell me when",
                "notify me",
                "alert me",
                "deep dive",
                "compare options",
            ],
            Kit::Meetings => &[
                "meeting",
                "transcript",
                "recording",
                "talking point",
                "agenda",
                "action item",
                "what did we decide",
                "what was discussed",
                "standup",
            ],
            Kit::Time => &[
                "time block",
                "block out",
                "block off",
                "set aside time",
                "cancel",
                "delete the event",
                "delete that event",
                "remove the event",
            ],
            Kit::Places => &[
                "near me",
                "nearby",
                "directions",
                "how far",
                "route",
                "drive to",
                "walk to",
                "commute",
                "restaurant",
                "cafe",
                "coffee shop",
                "pharmacy",
                "gas station",
                "closest",
                "nearest",
                "navigate",
            ],
            Kit::Pictures => &[
                "draw",
                "illustration",
                "diagram",
                "chart",
                "image",
                "picture",
                "infographic",
                "sketch",
                "logo",
                "illustrate",
            ],
            Kit::Browser => {
                &["browser", "browse", "fill out", "fill in the form", "sign up for", "click"]
            }
            Kit::Connections => &["connection", "connected", "mcp"],
            Kit::Assistant => &[
                "skill",
                "memories",
                "what do you remember",
                "what do you know about me",
                "your proposals",
                "the proposals",
                "accept the proposal",
                "decline the proposal",
                "what are you waiting on me for",
                "what have you been up to",
                "forget that",
                "forget this",
                "forget about",
                "forget what",
                "please forget",
            ],
        }
    }

    /// The kit that the app a person has open points at, if it does. Not
    /// every app does: Notes, Todo, the Journal, the Assistant and Settings
    /// are served by the core.
    fn of_app(app: App) -> Option<Kit> {
        match app {
            App::Mail => Some(Kit::Mail),
            App::Feed => Some(Kit::Feed),
            App::Money => Some(Kit::Money),
            App::ScreenTime => Some(Kit::Goals),
            App::Files => Some(Kit::Files),
            App::Health => Some(Kit::Health),
            App::Library => Some(Kit::Library),
            // Where areas, goals and the time by area are drawn.
            App::Overview => Some(Kit::Goals),
            // Time blocks are on the page, and so is the way to delete an event.
            App::Calendar => Some(Kit::Time),
            App::Notes | App::Todo | App::Assistant | App::Journal | App::Settings => None,
        }
    }

    /// The kit a record that is open or narrowing the list points at.
    fn of_shown(shown: Shown) -> Option<Kit> {
        match shown {
            Shown::Account(_) | Shown::Mailbox(_) | Shown::Thread(_) | Shown::Message(_) => {
                Some(Kit::Mail)
            }
            Shown::Draft(_) => Some(Kit::Mail),
            Shown::NewsSource(_) | Shown::Story(_) => Some(Kit::Feed),
            Shown::BankAccount(_) | Shown::Txn(_) => Some(Kit::Money),
            Shown::File(_) | Shown::Album(_) => Some(Kit::Files),
            Shown::Medication(_)
            | Shown::Condition(_)
            | Shown::HealthDocument(_)
            | Shown::LabResult(_)
            | Shown::Workout(_)
            | Shown::Meal(_) => Some(Kit::Health),
            Shown::Shelf(_) | Shown::Item(_) => Some(Kit::Library),
            Shown::Tracker(_) => Some(Kit::Trackers),
            Shown::Area(_) | Shown::Goal(_) => Some(Kit::Goals),
            Shown::Block(_) | Shown::Calendar(_) => Some(Kit::Time),
            Shown::Journal(_)
            | Shown::Entry(_)
            | Shown::Note(_)
            | Shown::Project(_)
            | Shown::Task(_)
            | Shown::Event(_) => None,
        }
    }
}

/// A set of kits: what a turn is offered, beyond the core.
///
/// `KitSet::default()` is [`KitSet::core`] -- no kit at all -- and
/// [`KitSet::all`] is every tool there is, which is what a turn that is not
/// routed gets, and what every turn got before kits existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KitSet(u32);

impl KitSet {
    /// The core alone.
    pub const fn core() -> Self {
        KitSet(0)
    }

    /// Every kit: the whole catalogue.
    pub fn all() -> Self {
        Kit::ALL.into_iter().fold(KitSet::core(), KitSet::with)
    }

    /// This set and `kit`.
    #[must_use]
    pub fn with(self, kit: Kit) -> Self {
        KitSet(self.0 | kit.bit())
    }

    /// This set and everything in `other`.
    #[must_use]
    pub fn union(self, other: KitSet) -> Self {
        KitSet(self.0 | other.0)
    }

    /// What is in this set and not in `other`.
    #[must_use]
    pub fn without(self, other: KitSet) -> Self {
        KitSet(self.0 & !other.0)
    }

    pub fn has(self, kit: Kit) -> bool {
        self.0 & kit.bit() != 0
    }

    pub fn is_core(self) -> bool {
        self.0 == 0
    }

    /// Whether every kit is in it.
    pub fn is_all(self) -> bool {
        self == KitSet::all()
    }

    /// The kits in it, in the order a turn offers them.
    pub fn iter(self) -> impl Iterator<Item = Kit> {
        Kit::ALL.into_iter().filter(move |kit| self.has(*kit))
    }

    /// Whether a tool is offered to a turn with this set: the core always,
    /// and a kit's tools when the kit is in it.
    pub fn offers(self, tool: &Tool) -> bool {
        tool.kit().is_none_or(|kit| self.has(kit))
    }

    /// As [`KitSet::offers`], for a tool known by its name -- the ones the
    /// service declares as well as the catalogue's. A name nobody filed is
    /// core, and offered.
    pub fn offers_name(self, name: &str) -> bool {
        kit_of(name).is_none_or(|kit| self.has(kit))
    }
}

impl FromIterator<Kit> for KitSet {
    fn from_iter<I: IntoIterator<Item = Kit>>(kits: I) -> Self {
        kits.into_iter().fold(KitSet::core(), KitSet::with)
    }
}

impl Tool {
    /// The kit this tool belongs to, or `None` for the core, which every turn
    /// is offered.
    pub fn kit(&self) -> Option<Kit> {
        catalogue_kit(self.name, self.domain)
    }
}

/// The tools the service declares for itself, and the kit each belongs to.
/// Only the ones that are not core; the rest -- `web_search`, `read_web_page`,
/// `get_weather`, `update_plan`, `offer_choices`, `ask_owner`, the spoken and
/// desk tools, `load_tools` -- are not here, and so are core.
///
/// The browser's tools are matched by their `browser_` prefix instead, in
/// [`kit_of`]: there are nine, and a tenth should not need a line here.
const SERVICE_TOOLS: &[(&str, Kit)] = &[
    ("search_places", Kit::Places),
    ("get_directions", Kit::Places),
    ("look_up_item", Kit::Library),
    ("search_images", Kit::Pictures),
    ("generate_image", Kit::Pictures),
    ("draw_illustration", Kit::Pictures),
    ("find_connection_tools", Kit::Connections),
    ("use_connection_tool", Kit::Connections),
    // Mail's own summary, wherever the mail tools are.
    ("summarize_thread", Kit::Mail),
    // What the card before a meeting shows.
    ("meeting_brief", Kit::Meetings),
];

/// The kit the tool called `name` belongs to: any tool, the catalogue's or one
/// the service declares. `None` is core, and also what a name nobody knows
/// answers.
pub fn kit_of(name: &str) -> Option<Kit> {
    if let Some((_, kit)) = SERVICE_TOOLS.iter().find(|(tool, _)| *tool == name) {
        return Some(*kit);
    }
    if name.starts_with("browser_") {
        return Some(Kit::Browser);
    }
    find(name).and_then(Tool::kit)
}

/// Which kit a catalogue tool is in, from its name and its domain.
///
/// A match on the domain, with the core's tools picked out by name first: a
/// domain added later lands in a kit of its own or fails to compile, and a
/// tool added to a domain that is mostly a kit joins that kit, which is almost
/// always right -- and when it is not, the size test at the foot of this file
/// says so.
fn catalogue_kit(name: &str, domain: Domain) -> Option<Kit> {
    // The few that are in the core although their domain is in a kit, or the
    // other way about.
    match name {
        // Orientation, and the search of entries and notes (the description
        // says where the rest of what they keep is searched).
        "overview" | "search" => return None,
        // Which a conversation about a calendar needs on the first turn: the
        // events themselves, and the calendars to put one on -- the preamble
        // names `list_calendars` wherever there is more than one place for an
        // event to go. Only deleting is admin.
        "list_events" | "create_event" | "update_event" | "list_calendars" => return None,
        "delete_event" => return Some(Kit::Time),
        // Tasks have time blocks; the blocks are a kit, the tasks are not.
        "list_time_blocks" | "create_time_block" | "update_time_block" | "delete_time_block" => {
            return Some(Kit::Time);
        }
        // A meeting's talking points are meeting work, filed under tasks.
        "add_talking_point" | "list_talking_points" => return Some(Kit::Meetings),
        // What they have used the screen for is a question about where the
        // time went.
        "screen_time" => return Some(Kit::Goals),
        // Memory the assistant needs without being asked, and finding what
        // was said before; reviewing, confirming and forgetting are admin.
        "remember"
        | "update_profile"
        | "read_skill"
        | "search_conversations"
        | "read_conversation" => return None,
        "list_memories" | "confirm_memory" | "forget" => return Some(Kit::Assistant),
        _ => {}
    }
    match domain {
        // Journals, notes and tasks are what every day is made of.
        Domain::Journals | Domain::Notes | Domain::Tasks => None,
        // The remaining calendar tools are listed above; a new one is core
        // until somebody says otherwise.
        Domain::Calendars => None,
        Domain::Library => Some(Kit::Library),
        Domain::Trackers => Some(Kit::Trackers),
        Domain::Purpose => Some(Kit::Goals),
        Domain::Routines => Some(Kit::Routines),
        Domain::Jobs => Some(Kit::Background),
        // Skills, proposals: what is left of the assistant's own domain.
        Domain::Agent => Some(Kit::Assistant),
        Domain::Mail => Some(Kit::Mail),
        Domain::Meetings => Some(Kit::Meetings),
        Domain::News => Some(Kit::Feed),
        Domain::Money => Some(Kit::Money),
        Domain::ScreenTime => Some(Kit::Goals),
        Domain::Files => Some(Kit::Files),
        Domain::Health => Some(Kit::Health),
    }
}

// ---- routing: what points at a kit ----------------------------------------

/// The kits the open app, and the records open in it, point at.
///
/// By reference, as [`OnScreen`] itself is: nothing here reads the vault, and
/// nothing a stranger wrote can reach it.
pub fn from_screen(screen: &OnScreen) -> KitSet {
    let mut kits = KitSet::core();
    if let Some(kit) = Kit::of_app(screen.app) {
        kits = kits.with(kit);
    }
    for shown in screen.open.iter().chain(&screen.within) {
        if let Some(kit) = Kit::of_shown(*shown) {
            kits = kits.with(kit);
        }
    }
    // A document in the reader, and a thing being heard, whatever app is open.
    if screen.reader.is_some() {
        kits = kits.with(Kit::Files);
    }
    if screen.playing.is_some() {
        kits = kits.with(Kit::Feed);
    }
    kits
}

/// The kits a message's own words point at -- see [`Kit::words`].
///
/// Whole words only, lowercased, with the plural of the last word accepted:
/// "email" finds "emails" and not "email-ish", "meal" finds "meals" and not
/// "mealy". A phrase matches words in a row. This is deliberately not a
/// classifier; it is a net with large holes, because `load_tools` and the
/// decision model are behind it.
pub fn from_words(text: &str) -> KitSet {
    let tokens = tokens(text);
    if tokens.is_empty() {
        return KitSet::core();
    }
    Kit::ALL
        .into_iter()
        .filter(|kit| kit.words().iter().any(|phrase| contains_phrase(&tokens, phrase)))
        .collect()
}

/// `text` as lowercase words: runs of letters, digits and the apostrophes and
/// hyphens inside a word.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        let c = if c == '\u{2019}' { '\'' } else { c };
        if c.is_alphanumeric() || ((c == '\'' || c == '-') && !word.is_empty()) {
            word.extend(c.to_lowercase());
        } else if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    // A word that ends in a hyphen or an apostrophe -- "ex-", "dogs'" -- is
    // the word without it.
    for word in &mut out {
        while word.ends_with(['-', '\'']) {
            word.pop();
        }
    }
    out.retain(|w| !w.is_empty());
    out
}

/// Whether the words of `phrase`, in a row, are in `tokens`; the last may be
/// plural.
fn contains_phrase(tokens: &[String], phrase: &str) -> bool {
    let words: Vec<&str> = phrase.split(' ').collect();
    let Some((last, init)) = words.split_last() else { return false };
    if tokens.len() < words.len() {
        return false;
    }
    tokens.windows(words.len()).any(|window| {
        let (end, start) = window.split_last().expect("a window is never empty");
        start.iter().zip(init).all(|(token, word)| token == word)
            && (end == last || *end == format!("{last}s") || *end == format!("{last}es"))
    })
}

// ---- the index -------------------------------------------------------------

/// What the system prompt says about the kits not yet loaded: one line each,
/// and the instruction to load before refusing. Empty when there are none.
///
/// Worded for a model that has only the core and no memory of what loading
/// does, the way the skills index is: the point is that "I can't do that" is
/// the wrong answer while a kit that could is listed.
pub fn index(not_loaded: &[Kit]) -> String {
    if not_loaded.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "\n\n## More tools\nYou start with the everyday tools. These sets load with \
         {LOAD_TOOLS} when a request needs them; load the set before you say you cannot do \
         something, and load everything a request needs in one call:"
    );
    for kit in not_loaded {
        text.push_str("\n- ");
        text.push_str(kit.name());
        text.push_str(": ");
        text.push_str(kit.blurb());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::catalog;

    /// The size a tool costs every step it is offered: its name, its
    /// description and its argument schema, as the provider receives them.
    fn weight(tool: &Tool) -> usize {
        tool.name.len() + tool.description.len() + tool.parameters().to_string().len()
    }

    fn weigh<'a>(tools: impl IntoIterator<Item = &'a Tool>) -> usize {
        tools.into_iter().map(weight).sum()
    }

    #[test]
    fn every_catalogue_tool_maps_to_the_core_or_to_one_kit() {
        // `kit()` is total by construction -- an `Option` and a match -- so the
        // test is about the answers: that the names in the exception list are
        // real tools, and every kit has something in it.
        let mut sizes = std::collections::BTreeMap::new();
        for tool in catalog() {
            let kit = tool.kit();
            assert_eq!(kit_of(tool.name), kit, "{} answers the same either way", tool.name);
            *sizes.entry(kit).or_insert(0usize) += 1;
        }
        for kit in Kit::ALL {
            // Places, Pictures, Browser and Connections are the service's.
            let service_only =
                matches!(kit, Kit::Places | Kit::Pictures | Kit::Browser | Kit::Connections);
            assert!(
                service_only || sizes.contains_key(&Some(kit)),
                "{} has no catalogue tool in it",
                kit.name()
            );
        }
        assert!(sizes.contains_key(&None), "the core is not empty");
        for (name, kit) in SERVICE_TOOLS {
            assert!(
                find(name).is_none(),
                "{name} is in the catalogue; its kit comes from the catalogue then, not the list"
            );
            assert_eq!(kit_of(name), Some(*kit));
        }
    }

    #[test]
    fn the_exceptions_name_tools_that_exist() {
        for name in [
            "overview",
            "search",
            "list_events",
            "create_event",
            "update_event",
            "list_calendars",
            "delete_event",
            "list_time_blocks",
            "create_time_block",
            "update_time_block",
            "delete_time_block",
            "add_talking_point",
            "list_talking_points",
            "screen_time",
            "remember",
            "update_profile",
            "read_skill",
            "search_conversations",
            "read_conversation",
            "list_memories",
            "confirm_memory",
            "forget",
        ] {
            assert!(find(name).is_some(), "{name} is not a tool: a rename left the table behind");
        }
    }

    #[test]
    fn a_name_nobody_filed_is_core_and_the_service_ones_are_where_they_belong() {
        for core in [
            LOAD_TOOLS,
            "web_search",
            "read_web_page",
            "get_weather",
            "update_plan",
            "offer_choices",
            "ask_owner",
            "end_voice_mode",
            "type_text",
            "no_such_tool",
        ] {
            assert_eq!(kit_of(core), None, "{core}");
            assert!(KitSet::core().offers_name(core), "{core}");
        }
        assert_eq!(kit_of("browser_click"), Some(Kit::Browser));
        assert_eq!(kit_of("search_places"), Some(Kit::Places));
        assert_eq!(kit_of("summarize_thread"), Some(Kit::Mail));
        assert!(!KitSet::core().offers_name("search_mail"));
        assert!(KitSet::core().with(Kit::Mail).offers_name("search_mail"));
        assert!(KitSet::core().with(Kit::Mail).offers_name("summarize_thread"));
    }

    #[test]
    fn a_kit_set_is_a_set() {
        let none = KitSet::core();
        assert!(none.is_core() && !none.is_all());
        assert_eq!(KitSet::default(), none);
        let mail = none.with(Kit::Mail).with(Kit::Mail);
        assert!(mail.has(Kit::Mail) && !mail.has(Kit::Health));
        let both = mail.union(none.with(Kit::Health));
        assert_eq!(both.iter().collect::<Vec<_>>(), vec![Kit::Mail, Kit::Health]);
        assert_eq!(both.without(mail).iter().collect::<Vec<_>>(), vec![Kit::Health]);
        let all = KitSet::all();
        assert!(all.is_all() && Kit::ALL.into_iter().all(|k| all.has(k)));
        assert_eq!(all.iter().collect::<Vec<_>>(), Kit::ALL.to_vec());
        assert_eq!(Kit::ALL.into_iter().collect::<KitSet>(), all);
        // Iteration is in the fixed order, whatever order they joined in.
        let late = none.with(Kit::Assistant).with(Kit::Mail).with(Kit::Money);
        assert_eq!(late.iter().collect::<Vec<_>>(), vec![Kit::Mail, Kit::Money, Kit::Assistant]);
    }

    #[test]
    fn all_offers_every_catalogue_tool_and_core_only_the_core() {
        for tool in catalog() {
            assert!(KitSet::all().offers(tool), "{}", tool.name);
            assert_eq!(KitSet::core().offers(tool), tool.kit().is_none(), "{}", tool.name);
        }
    }

    #[test]
    fn the_list_of_kits_is_in_the_order_of_their_positions() {
        for (at, kit) in Kit::ALL.into_iter().enumerate() {
            assert_eq!(kit.index(), at, "{}", kit.name());
        }
        assert!(Kit::ALL.len() <= 32, "a KitSet is a u32");
    }

    #[test]
    fn kits_are_named_and_parsed_by_the_same_words() {
        for kit in Kit::ALL {
            assert_eq!(Kit::parse(kit.name()), Some(kit));
            assert_eq!(Kit::parse(&format!("  {}  ", kit.name().to_uppercase())), Some(kit));
            assert!(!kit.blurb().is_empty() && !kit.blurb().contains('\n'));
            assert!(!kit.words().is_empty(), "{} has nothing that points at it", kit.name());
            // The serde name is the same word.
            assert_eq!(serde_json::to_value(kit).unwrap(), serde_json::json!(kit.name()));
        }
        assert_eq!(Kit::parse("tasks"), None);
        assert_eq!(Kit::parse(""), None);
    }

    #[test]
    fn the_words_are_written_the_way_they_are_read() {
        for kit in Kit::ALL {
            for phrase in kit.words() {
                assert_eq!(
                    *phrase,
                    phrase.to_lowercase(),
                    "{phrase} must be lowercase: tokens are"
                );
                assert!(
                    !phrase.starts_with(' ') && !phrase.ends_with(' ') && !phrase.contains("  ")
                );
                // Run through the tokeniser it comes out as itself, or the
                // phrase could never match.
                assert_eq!(tokens(phrase).join(" "), *phrase, "{phrase}");
            }
        }
    }

    #[test]
    fn words_point_at_kits() {
        let kits = |text: &str| from_words(text).iter().collect::<Vec<_>>();
        assert_eq!(kits("Reply to Anna's email about Tuesday"), vec![Kit::Mail]);
        assert_eq!(kits("Any emails from the bank?"), vec![Kit::Mail, Kit::Money]);
        assert_eq!(kits("How did I sleep this week?"), vec![Kit::Health]);
        assert_eq!(kits("what's my A1C trend"), vec![Kit::Health]);
        assert_eq!(kits("I ate two eggs for breakfast"), vec![Kit::Health]);
        assert_eq!(kits("How much did I spend on food last month?"), vec![Kit::Money]);
        assert_eq!(kits("What was in that podcast episode?"), vec![Kit::Feed]);
        assert_eq!(kits("Open the PDF I saved"), vec![Kit::Files]);
        assert_eq!(kits("keep an eye on the price of the flights"), vec![Kit::Background]);
        assert_eq!(kits("Is there a good restaurant nearby?"), vec![Kit::Places]);
        assert_eq!(kits("Every Monday morning, send me a digest"), vec![Kit::Routines]);
        assert_eq!(kits("Draw a diagram of the plan"), vec![Kit::Pictures]);
        assert_eq!(kits("block out Friday afternoon"), vec![Kit::Time]);
        assert_eq!(kits("cancel my three o'clock"), vec![Kit::Time]);
        assert_eq!(kits("let them know I'm running late"), vec![Kit::Mail]);
        assert_eq!(kits("What did we decide in the meeting?"), vec![Kit::Meetings]);
        // Common words that mean something else as often as not.
        assert_eq!(kits("don't forget to call Mom"), vec![]);
        assert_eq!(kits("Listen, can you help me with something?"), vec![]);
        assert_eq!(kits("remind me in 5 minutes"), vec![]);
        assert_eq!(kits("a daily walk in the area"), vec![]);
        assert_eq!(kits("read this website for me"), vec![]);
        assert_eq!(kits("forget that I like olives"), vec![Kit::Assistant]);
        // Nothing the core does not do.
        assert_eq!(kits("add milk to my list"), vec![]);
        assert_eq!(kits("what is on my calendar tomorrow"), vec![]);
        assert_eq!(kits(""), vec![]);
        assert_eq!(kits("   ..."), vec![]);
    }

    #[test]
    fn a_word_is_a_whole_word_with_an_optional_plural() {
        let kits = |text: &str| from_words(text).iter().collect::<Vec<_>>();
        // Not inside another word.
        assert_eq!(kits("a hotmail-ish nailed it"), vec![]);
        assert_eq!(kits("the mealy potatoes"), vec![]);
        assert_eq!(kits("a bookmark"), vec![]);
        assert_eq!(kits("the filet was fine"), vec![]);
        // The plural of the last word, and not of a word before it.
        assert_eq!(kits("my meals"), vec![Kit::Health]);
        assert_eq!(kits("two lab results"), vec![Kit::Health]);
        assert_eq!(kits("books"), vec![Kit::Library]);
        // A phrase in order, in a row.
        assert_eq!(kits("what is up with next week"), vec![]);
        assert_eq!(kits("add it to up next"), vec![Kit::Feed]);
        assert_eq!(kits("blood sugar was high"), vec![Kit::Health]);
        assert_eq!(kits("sugar in my blood"), vec![]);
        // Case, punctuation and a curly apostrophe.
        assert_eq!(kits("EMAIL!"), vec![Kit::Mail]);
        assert_eq!(kits("cc\u{2019}d me"), vec![Kit::Mail]);
        assert_eq!(kits("(inbox)"), vec![Kit::Mail]);
    }

    #[test]
    fn the_index_lists_what_is_not_loaded_and_says_to_load_before_refusing() {
        assert_eq!(index(&[]), "");
        let text = index(&[Kit::Mail, Kit::Places]);
        assert!(text.contains("## More tools"), "{text}");
        assert!(text.contains(LOAD_TOOLS) && text.contains("before you say you cannot"), "{text}");
        assert!(text.contains("\n- mail: search, read, draft, file and send their email"));
        assert!(text.contains("\n- places: places nearby and directions"));
        assert!(!text.contains("health"));
        // All of them, one line each.
        let everything = index(&Kit::ALL);
        assert_eq!(everything.matches("\n- ").count(), Kit::ALL.len());
    }

    /// The numbers the design is about, and the reason a tool added to a
    /// domain that is core is a decision: what every step of every turn
    /// carries, before a kit joins.
    ///
    /// Printed as well as asserted -- `--nocapture` shows what each kit
    /// costs -- because "is this tool worth being in the core" is a question
    /// about a number.
    #[test]
    fn the_core_stays_small() {
        let core: Vec<&Tool> = catalog().iter().filter(|t| t.kit().is_none()).collect();
        let core_chars = weigh(core.iter().copied());
        eprintln!("core: {} tools, {core_chars} characters", core.len());
        for tool in &core {
            eprintln!("    {:>6} {}", weight(tool), tool.name);
        }
        for kit in Kit::ALL {
            let tools: Vec<&Tool> = catalog().iter().filter(|t| t.kit() == Some(kit)).collect();
            eprintln!(
                "{}: {} tools, {} characters",
                kit.name(),
                tools.len(),
                weigh(tools.iter().copied())
            );
        }
        // The service adds `web_search`, `read_web_page`, `get_weather`,
        // `update_plan`, `offer_choices` and `load_tools` to this on a
        // routed turn: six small tools, measured in its own tests against the
        // same budget.
        assert!(
            core_chars < CORE_BUDGET_CHARS,
            "the core's tools are {core_chars} characters of schema, and every step of every \
             turn carries them: {} tools. Put something in a kit.",
            core.len()
        );
        assert!(core.len() <= 40, "{} tools in the core", core.len());
    }
}
