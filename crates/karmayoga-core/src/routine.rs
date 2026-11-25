//! Standing work: what the assistant does without being asked.
//!
//! A [`Routine`] is a trigger, an instruction in the person's own words, and
//! a switch. "Every weekday at seven, look at what is due and what is on the
//! calendar, and leave me a note." The scheduler in `karmayoga-service` reads
//! them, decides which are due, and runs them one at a time through the same
//! turn the chat rail uses.
//!
//! # Why the arithmetic is here
//!
//! Deciding whether a routine is due is the part that goes wrong, and it goes
//! wrong in ways that only show up at a boundary: a weekday schedule across a
//! weekend, an hour that happens twice when the clocks go back, a laptop that
//! was shut for a week. So it is a pure function of a trigger, a clock and a
//! zone, with the clock passed in -- and there is a table of cases at the
//! foot of this file rather than a scheduler somebody has to run in October
//! to find out about.
//!
//! # The rule about missing one
//!
//! A morning brief that was missed by six hours is not a morning brief. A
//! weekly review missed by a day is still a weekly review. So a routine
//! carries a *grace*, and a slot older than that is [`Due::Missed`] -- which
//! the scheduler records as a skipped run with a reason, rather than either
//! running six hours late or saying nothing at all. A laptop shut for a week
//! shows one honest row per morning instead of seven briefs at once.

use crate::brief::{Brief, BriefKind, BriefSpec};
use crate::id::{AccountId, AreaId, ChatLinkId, RoutineId, RoutineRunId};
use crate::timestamped::Timestamped;
use crate::{ConversationId, Error, Result};
use jiff::civil::{Time, Weekday as CivilWeekday, time};
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

/// Longest a routine's instructions may be.
///
/// The same order as the assistant's own standing instructions, and finite
/// for the same reason: this string is sent on every run of every day.
pub const MAX_INSTRUCTIONS_BYTES: usize = 8_000;

/// Longest a routine's name may be.
pub const MAX_NAME_BYTES: usize = 200;

/// The most steps a brief takes unless somebody says otherwise: one to write,
/// and a few reads before it.
pub const BRIEF_STEPS: u32 = 8;

/// What a routine writes, as the whole of its last message, when it looked and
/// found nothing worth the person's attention: a sweep over what is waiting on
/// them that finds none of it, say. Such a run is kept in the log -- with that
/// as its summary, so "why did I hear nothing" has an answer -- and is neither
/// delivered nor announced nor counted as unread.
///
/// Matched by [`is_nothing_to_report`], not by `==`, because a model gives it
/// a different case or a full stop.
pub const NOTHING_TO_REPORT: &str = "Nothing to report.";

/// Whether `text` is [`NOTHING_TO_REPORT`] and nothing else, whatever its case
/// and closing punctuation.
pub fn is_nothing_to_report(text: &str) -> bool {
    let said = text.trim().trim_matches(|c: char| matches!(c, '.' | '!' | '*' | '_' | '"'));
    said.trim().eq_ignore_ascii_case("nothing to report")
}

/// How long after its moment a routine will still run, when nothing else is
/// said. An hour: long enough to survive a laptop opened after breakfast,
/// short enough that a morning brief is still a morning brief.
pub const DEFAULT_GRACE_MINUTES: u32 = 60;

/// A day of the week, in the spelling the wire uses.
///
/// Ours rather than [`jiff::civil::Weekday`]'s so that the stored spelling is
/// this application's decision and cannot change under us with a dependency
/// bump. The conversion is the only place the two meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];

    /// Monday to Friday, which is what "weekdays" means to a person and is
    /// the commonest schedule anybody will ask for.
    pub const WEEKDAYS: [Weekday; 5] =
        [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];

    pub fn as_str(self) -> &'static str {
        match self {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        }
    }

    /// Parse a day. Accepts the short form, the full name, and any casing,
    /// because this is also what a model will be typing into `create_routine`.
    pub fn parse(s: &str) -> Option<Weekday> {
        let s = s.trim().to_lowercase();
        Weekday::ALL.iter().copied().find(|d| {
            let short = d.as_str();
            s == short || s.starts_with(short) && d.full().starts_with(&s)
        })
    }

    pub fn full(self) -> &'static str {
        match self {
            Weekday::Mon => "monday",
            Weekday::Tue => "tuesday",
            Weekday::Wed => "wednesday",
            Weekday::Thu => "thursday",
            Weekday::Fri => "friday",
            Weekday::Sat => "saturday",
            Weekday::Sun => "sunday",
        }
    }

    fn from_civil(day: CivilWeekday) -> Weekday {
        match day {
            CivilWeekday::Monday => Weekday::Mon,
            CivilWeekday::Tuesday => Weekday::Tue,
            CivilWeekday::Wednesday => Weekday::Wed,
            CivilWeekday::Thursday => Weekday::Thu,
            CivilWeekday::Friday => Weekday::Fri,
            CivilWeekday::Saturday => Weekday::Sat,
            CivilWeekday::Sunday => Weekday::Sun,
        }
    }
}

/// What sets a routine going.
///
/// An enum rather than a bag of optional fields, for the reason
/// [`crate::Purpose`] is one: a clock time with weekdays and a lead time
/// before a meeting have nothing in common but the word "when", and a routine
/// has exactly one of them.
// Both lines matter and they do different things: `rename_all` names the
// variants, `rename_all_fields` names the fields inside them. Without the
// second, `lead_minutes` went out in snake_case while every other record on
// the wire is camelCase -- so a routine that runs before a meeting could not
// be saved from the interface at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Trigger {
    /// A time of day, on the given days. An empty list means every day.
    Schedule {
        at: Time,
        #[serde(default)]
        days: Vec<Weekday>,
        /// Only on this day of the month, `1..=28`, as well as on `days`.
        /// `None` is every day `days` allows. Added for the monthly dream;
        /// capped at 28 so that every month has the day.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        day_of_month: Option<u8>,
    },
    /// Before an event on the calendar starts.
    ///
    /// Evaluated as a *query* on each tick rather than as a clock slot: the
    /// events it is about arrive from somebody else's server, so there is no
    /// moment to compute in advance. See [`Trigger::is_clock`].
    BeforeEvent {
        lead_minutes: u32,
        /// Only events on a calendar filed under this area. `None` is any.
        /// `roleId` in a routine saved before areas were renamed from roles.
        #[serde(default, skip_serializing_if = "Option::is_none", alias = "roleId")]
        area_id: Option<AreaId>,
    },
    /// Before a task falls due. A query, like `BeforeEvent`.
    TaskDue { lead_days: u32 },
    /// Never on its own. Run now, and nothing else.
    Manual,
}

impl Trigger {
    /// Whether this trigger is a moment on the clock.
    ///
    /// The two query triggers are answered by looking at what is in the vault
    /// on each tick, so [`Routine::is_due`] has nothing to say about them.
    pub fn is_clock(&self) -> bool {
        matches!(self, Trigger::Schedule { .. })
    }

    /// The trigger in words, for a list that has to say when something runs.
    ///
    /// Here rather than in the interface because the assistant has to say it
    /// too, and two spellings of "weekdays at 07:00" would eventually
    /// disagree.
    pub fn describe(&self) -> String {
        match self {
            Trigger::Schedule { at, days, day_of_month } => {
                let when = format!("{:02}:{:02}", at.hour(), at.minute());
                // The day of the month wins over `days`: the two are not
                // meant to be combined, and a monthly dream sets `days` to
                // empty anyway. See the module doc's table of cases.
                if let Some(dom) = day_of_month {
                    return format!("Monthly on the {} at {when}", ordinal(*dom));
                }
                if days.is_empty() {
                    return format!("Every day at {when}");
                }
                if days.len() == 7 {
                    return format!("Every day at {when}");
                }
                if days.len() == 5 && Weekday::WEEKDAYS.iter().all(|d| days.contains(d)) {
                    return format!("Weekdays at {when}");
                }
                if days.len() == 2 && days.contains(&Weekday::Sat) && days.contains(&Weekday::Sun) {
                    return format!("Weekends at {when}");
                }
                let names: Vec<&str> =
                    Weekday::ALL.iter().filter(|d| days.contains(d)).map(|d| d.as_str()).collect();
                format!("{} at {when}", capitalise(&names.join(", ")))
            }
            Trigger::BeforeEvent { lead_minutes, .. } => {
                format!("{lead_minutes} minutes before a meeting")
            }
            Trigger::TaskDue { lead_days } => match lead_days {
                0 => "When a task falls due".into(),
                1 => "The day before a task is due".into(),
                n => format!("{n} days before a task is due"),
            },
            Trigger::Manual => "Only when you ask".into(),
        }
    }
}

/// "1st", "2nd", "3rd", "4th" -- the ordinal suffix a day of the month is
/// read with. The teens are all "th", including the ones whose last digit
/// would otherwise say "st" or "nd" or "rd".
fn ordinal(n: u8) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Whether a routine wants running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Its moment has come and is still within its grace. `slot` is the
    /// moment, which the run records so two ticks cannot both claim it.
    Now { slot: Timestamp },
    /// Its moment came and went while nothing was running. Recorded as a
    /// skipped run rather than run late or passed over in silence.
    Missed { slot: Timestamp },
    /// Nothing to do.
    Later,
}

/// Whose routine this is.
///
/// A dream is a routine the application owns: it is made when dreaming is
/// switched on, it cannot be deleted by hand, and its instructions field is
/// the person's paragraph *added to* an app-owned prompt rather than the
/// prompt itself. See `docs/plans/dreaming.md`.
///
/// A brief is the other routine the application owns: one per
/// [`BriefKind`], made when it is first switched on, and its instructions are
/// likewise the person's paragraph added to the app's own prompt. What it
/// covers and how long it is ride on the kind itself -- see [`BriefSpec`] and
/// `docs/plans/briefs.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RoutineKind {
    #[default]
    Custom,
    Dream {
        scope: DreamScope,
    },
    Brief(BriefSpec),
}

impl RoutineKind {
    pub fn is_dream(&self) -> bool {
        matches!(self, RoutineKind::Dream { .. })
    }

    pub fn dream_scope(&self) -> Option<DreamScope> {
        match self {
            RoutineKind::Dream { scope } => Some(*scope),
            _ => None,
        }
    }

    pub fn brief(&self) -> Option<&BriefSpec> {
        match self {
            RoutineKind::Brief(spec) => Some(spec),
            _ => None,
        }
    }

    pub fn brief_kind(&self) -> Option<BriefKind> {
        self.brief().map(|spec| spec.brief)
    }

    /// Whether the application owns this routine: a dream or a brief. Such a
    /// routine is made only by the application, never deleted by hand, and
    /// its instructions may be empty.
    pub fn is_owned(&self) -> bool {
        !matches!(self, RoutineKind::Custom)
    }

    /// Whether `other` is the same routine as far as ownership goes: both a
    /// person's, the same dream, or the same brief -- whatever that brief
    /// covers, which is the person's to change.
    pub fn same_owner(&self, other: &RoutineKind) -> bool {
        match (self, other) {
            (RoutineKind::Custom, RoutineKind::Custom) => true,
            (RoutineKind::Dream { scope: a }, RoutineKind::Dream { scope: b }) => a == b,
            (RoutineKind::Brief(a), RoutineKind::Brief(b)) => a.brief == b.brief,
            _ => false,
        }
    }
}

/// How far back a dream reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum DreamScope {
    /// Yesterday, from the records. The default because it is the smallest
    /// unit dreaming comes in -- a `Digest` with nothing else set describes
    /// one night, not a week or a month.
    #[default]
    Day,
    /// The last seven days, from the nightly dreams' outputs.
    Week,
    /// The last month, from the weekly dreams' outputs.
    Month,
}

impl DreamScope {
    pub const ALL: [DreamScope; 3] = [DreamScope::Day, DreamScope::Week, DreamScope::Month];

    pub fn as_str(self) -> &'static str {
        match self {
            DreamScope::Day => "day",
            DreamScope::Week => "week",
            DreamScope::Month => "month",
        }
    }
}

/// Where a run's result goes besides the Assistant app, and whether the run
/// may reach its person while it works.
///
/// Every run is kept in the app whatever this says; this adds places. The
/// default is none, which is every routine saved before this existed.
///
/// The email goes to the sending account's own address, always. It is the
/// person's own report reaching them, not a message to anybody else, and a
/// routine the assistant set up must not be able to point it anywhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct Delivery {
    /// The chat link whose private room with its owner gets the result. A
    /// reply there carries on the run's own conversation. See
    /// `docs/plans/chat.md`'s "Routines in the chat".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatLinkId>,
    /// Put questions to the owner in that chat while the run works: approvals
    /// for what would delete or send something, and a free question when it
    /// needs a fact or a decision only they have. Read only when `chat` is
    /// set. Off, a run has nobody to ask and refuses or parks as it always
    /// has.
    pub ask: bool,
    /// Call the owner on that chat link with the result instead of writing
    /// it there: the assistant says it, and stays on the line for whatever
    /// they ask about it, in the run's own conversation (which the app keeps,
    /// as it keeps every run). Written in the chat only if they do not pick
    /// up, or the call cannot be made. Read only when `chat` is set.
    pub call: bool,
    /// Email the result from this account to its own address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<AccountId>,
}

impl Delivery {
    /// Whether this sends the result anywhere beyond the app.
    pub fn is_empty(&self) -> bool {
        self.chat.is_none() && self.email.is_none()
    }

    /// Whether a run may ask its person something while it works.
    pub fn asks(&self) -> bool {
        self.ask && self.chat.is_some()
    }

    /// Whether the result is delivered by calling its person.
    pub fn calls(&self) -> bool {
        self.call && self.chat.is_some()
    }
}

/// Where a run's result was sent, or why it could not be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Delivered {
    pub to: DeliveredTo,
    pub ok: bool,
    /// Where it went ("Matrix", "me@example.com"), or why it did not.
    pub detail: String,
}

/// One of the places [`Delivery`] can send a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DeliveredTo {
    Chat,
    Email,
}

/// Standing work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: RoutineId,
    pub name: String,
    /// What to do, in the person's own words. This is the prompt.
    pub instructions: String,
    pub trigger: Trigger,
    /// Minutes past its moment that it will still run. See the module note.
    #[serde(default = "default_grace")]
    pub grace_minutes: u32,
    pub enabled: bool,
    /// Written by a person, or owned by the application. A person's routine
    /// is the default, and everything saved before this field existed.
    #[serde(default)]
    pub kind: RoutineKind,
    /// Where the result goes besides the app, and whether a run may ask.
    #[serde(default)]
    pub delivery: Delivery,
    /// Which of the three models a run is asked on -- see
    /// [`crate::llm::ModelTier`]. `None` is the medium one, the assistant's
    /// own, which is what every routine ran on before this could be chosen.
    /// Large without a large model set up runs on the medium one, as research
    /// does; small without a small one is skipped, with the reason, rather
    /// than quietly run on a model nobody chose for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::llm::ModelTier>,
    /// The most steps a run may take -- model turns, each of which may call
    /// tools. `None` is the assistant's own ceiling, from its settings
    /// ([`crate::agent::AgentSettings::max_steps`]). A routine that reads a
    /// week of mail needs more than one that writes a line, and nothing else
    /// about it should have to change for that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn default_grace() -> u32 {
    DEFAULT_GRACE_MINUTES
}

impl Routine {
    pub fn new(name: impl Into<String>, instructions: impl Into<String>, trigger: Trigger) -> Self {
        let now = Timestamp::now();
        Self {
            id: RoutineId::new(),
            name: name.into(),
            instructions: instructions.into(),
            trigger,
            grace_minutes: DEFAULT_GRACE_MINUTES,
            enabled: true,
            kind: RoutineKind::Custom,
            delivery: Delivery::default(),
            model: None,
            max_steps: None,
            last_run_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// The application-owned routine for one dream scope, as first created
    /// when dreaming is switched on. See `Vault::set_dreaming`.
    ///
    /// Instructions start empty -- that box is the person's paragraph,
    /// appended to `crate::dream::instructions` rather than replacing it --
    /// and the routine starts disabled, because `set_dreaming` is what turns
    /// it on, in the same call that makes it.
    pub fn dream(scope: DreamScope) -> Self {
        let (name, trigger, grace_minutes) = match scope {
            DreamScope::Day => (
                "Nightly dream",
                Trigger::Schedule { at: time(3, 0, 0, 0), days: Vec::new(), day_of_month: None },
                20 * 60,
            ),
            DreamScope::Week => (
                "Weekly dream",
                Trigger::Schedule {
                    at: time(3, 30, 0, 0),
                    days: vec![Weekday::Sun],
                    day_of_month: None,
                },
                44 * 60,
            ),
            DreamScope::Month => (
                "Monthly dream",
                Trigger::Schedule { at: time(4, 0, 0, 0), days: Vec::new(), day_of_month: Some(1) },
                6 * 24 * 60,
            ),
        };
        let mut routine = Routine::new(name, "", trigger);
        routine.grace_minutes = grace_minutes;
        routine.kind = RoutineKind::Dream { scope };
        routine.enabled = false;
        // The week and the month are where the inferring is done -- what
        // holds across seven nightly accounts, what a month of weekly ones
        // shows -- and each runs once. The large model is worth its cost
        // there, and answers with the medium one when none is set up.
        if scope != DreamScope::Day {
            routine.model = Some(crate::llm::ModelTier::Large);
        }
        routine
    }

    /// The application-owned routine for one brief, as first created when it
    /// is switched on. See `Vault::set_brief`.
    ///
    /// At the kind's own time on its own days (every day, or Sundays for the
    /// weekly brief), covering the kind's default
    /// sections, at a standard length -- the daily podcast covers none, and
    /// has a quarter of an hour of the whole Feed instead; instructions empty
    /// (the person's paragraph), eight steps at most -- a brief reads a few
    /// things and writes once -- and disabled until `set_brief` turns it on.
    pub fn brief(kind: BriefKind) -> Self {
        let trigger = Trigger::Schedule {
            at: kind.default_time(),
            days: kind.default_days(),
            day_of_month: None,
        };
        let mut routine = Routine::new(kind.name(), "", trigger);
        routine.grace_minutes = kind.default_grace_minutes();
        routine.kind = RoutineKind::Brief(BriefSpec::new(kind));
        routine.max_steps = Some(BRIEF_STEPS);
        routine.enabled = false;
        // The weekly brief runs once a week, matters more, and has the most
        // to bring together -- last week against their goals, and the week
        // ahead. The large model is worth it there, and answers with the
        // medium one when none is set up.
        if kind == BriefKind::Weekly {
            routine.model = Some(crate::llm::ModelTier::Large);
        }
        routine
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a routine needs a name".into()));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(Error::Invalid(format!(
                "a routine's name must be under {MAX_NAME_BYTES} bytes"
            )));
        }
        // A person's routine is a prompt and needs one. A dream's or a
        // brief's is the app's own prompt with the person's paragraph merely
        // appended -- see `crate::dream::instructions` and
        // `crate::brief::instructions` -- so an empty box beside it is not an
        // empty routine.
        if matches!(self.kind, RoutineKind::Custom) && self.instructions.trim().is_empty() {
            return Err(Error::Invalid(
                "a routine needs instructions: what should it do when it runs?".into(),
            ));
        }
        if self.instructions.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(Error::Invalid(format!(
                "instructions are {} bytes; the limit is {MAX_INSTRUCTIONS_BYTES}",
                self.instructions.len()
            )));
        }
        // A dream's work is proposals, decided in the app; there is no result
        // to hand anybody and nobody it should be asking at three in the
        // morning.
        if self.kind.is_dream() && (!self.delivery.is_empty() || self.delivery.ask) {
            return Err(Error::Invalid("a dream's results stay in the Assistant app".into()));
        }
        if let Some(spec) = self.kind.brief() {
            spec.validate()?;
            // A brief tells; it has nothing to ask, and every question would
            // hold up the morning's brief until it was answered.
            if self.delivery.ask {
                return Err(Error::Invalid(
                    "a brief does not ask questions while it works; turn asking off".into(),
                ));
            }
            if !self.trigger.is_clock() && self.trigger != Trigger::Manual {
                return Err(Error::Invalid(
                    "a brief runs at a time of day, or only when asked".into(),
                ));
            }
            // A weekly brief takes the morning brief's place on its days, so
            // "every day" would quietly end the morning brief.
            if spec.brief == BriefKind::Weekly
                && let Trigger::Schedule { days, day_of_month, .. } = &self.trigger
                && day_of_month.is_none()
                && (days.is_empty() || days.len() == Weekday::ALL.len())
            {
                return Err(Error::Invalid(
                    "a weekly brief runs on chosen days -- Sunday, say -- not every day; for \
                     every day, use the morning brief"
                        .into(),
                ));
            }
        }
        if let Trigger::Schedule { day_of_month: Some(dom), .. } = &self.trigger
            && !(1..=28).contains(dom)
        {
            return Err(Error::Invalid(
                "a monthly schedule's day must be between 1 and 28, so that every month has it"
                    .into(),
            ));
        }
        if let Some(steps) = self.max_steps
            && !(1..=crate::agent::MAX_STEPS_LIMIT).contains(&steps)
        {
            return Err(Error::Invalid(format!(
                "a routine's steps must be between 1 and {}",
                crate::agent::MAX_STEPS_LIMIT
            )));
        }
        Ok(())
    }

    /// The moment this routine last wanted running at or before `now`.
    ///
    /// `None` for a trigger that is not on the clock, and for a schedule
    /// whose days do not include the last seven -- which cannot happen, since
    /// an empty list means every day, but is expressed rather than assumed.
    pub fn slot_at_or_before(&self, now: &Zoned) -> Option<Timestamp> {
        let Trigger::Schedule { at, days, day_of_month } = &self.trigger else { return None };
        // Walk back a week a day at a time. Cheap, and it is the only way to
        // be right across a zone whose offset changed in the middle: each
        // candidate is built in the local calendar and converted, so a slot
        // is the wall-clock time it says it is.
        //
        // A monthly routine's grace can run to six days, which is why this
        // walks back further than a weekly one ever needs to: the search has
        // to still be looking on the last day its grace allows.
        let back_days = if day_of_month.is_some() { 35 } else { 7 };
        for back in 0..=back_days {
            let day = now.date().checked_sub(jiff::Span::new().days(back)).ok()?;
            if let Some(dom) = day_of_month
                && day.day() != *dom as i8
            {
                continue;
            }
            if !days.is_empty() && !days.contains(&Weekday::from_civil(day.weekday())) {
                continue;
            }
            // An hour that does not exist -- the spring-forward gap -- is
            // taken as the first instant that does, which is what a person
            // means by "at half past two" on that one morning.
            let Ok(candidate) = day.to_datetime(*at).to_zoned(now.time_zone().clone()) else {
                continue;
            };
            if candidate.timestamp() <= now.timestamp() {
                return Some(candidate.timestamp());
            }
        }
        None
    }

    /// Whether this routine wants running now.
    ///
    /// `since` is the floor: the last run, else the moment the routine was
    /// made. Using `created_at` as the floor is what stops a routine set up
    /// at nine in the morning reporting that it missed its seven o'clock --
    /// a slot that existed before the routine did is not one it missed.
    pub fn is_due(&self, now: &Zoned) -> Due {
        if !self.enabled || !self.trigger.is_clock() {
            return Due::Later;
        }
        let Some(slot) = self.slot_at_or_before(now) else { return Due::Later };
        let since = self.last_run_at.unwrap_or(self.created_at);
        if slot <= since {
            return Due::Later;
        }
        let late = now.timestamp().as_second() - slot.as_second();
        if late <= i64::from(self.grace_minutes) * 60 {
            Due::Now { slot }
        } else {
            Due::Missed { slot }
        }
    }

    /// Whether this routine's schedule has a slot on `day` at all -- switched
    /// on, on the clock, and the day one of its days. What lets the weekly
    /// brief take the morning brief's place on its day.
    pub fn runs_on(&self, day: jiff::civil::Date) -> bool {
        let Trigger::Schedule { days, day_of_month, .. } = &self.trigger else { return false };
        self.enabled
            && day_of_month.is_none_or(|dom| day.day() == dom as i8)
            && (days.is_empty() || days.contains(&Weekday::from_civil(day.weekday())))
    }

    /// The next moment this routine will want running, strictly after `now`.
    ///
    /// For a list that says when something runs next. `None` for a trigger
    /// that is not on the clock, or a routine that is switched off.
    pub fn next_due(&self, now: &Zoned) -> Option<Timestamp> {
        if !self.enabled {
            return None;
        }
        let Trigger::Schedule { at, days, day_of_month } = &self.trigger else { return None };
        let ahead_days = if day_of_month.is_some() { 35 } else { 7 };
        for ahead in 0..=ahead_days {
            let day = now.date().checked_add(jiff::Span::new().days(ahead)).ok()?;
            if let Some(dom) = day_of_month
                && day.day() != *dom as i8
            {
                continue;
            }
            if !days.is_empty() && !days.contains(&Weekday::from_civil(day.weekday())) {
                continue;
            }
            let Ok(candidate) = day.to_datetime(*at).to_zoned(now.time_zone().clone()) else {
                continue;
            };
            if candidate.timestamp() > now.timestamp() {
                return Some(candidate.timestamp());
            }
        }
        None
    }
}

impl Timestamped for Routine {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// How a run ended.
///
/// A closed set, like [`crate::GoalStatus`], so a run cannot be in a state
/// the interface has no word for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Asked for, and not started yet.
    ///
    /// Its own state rather than a corner of `Running`, because the two look
    /// identical in the vault and want opposite things from the scheduler: a
    /// queued run is work to pick up, and a running one whose process is gone
    /// is a row to close. Guessing between them from a timestamp would be a
    /// guess that eventually eats somebody's "run now".
    #[default]
    Queued,
    /// Started and has not finished. A row in this state that no live process
    /// claims is one a dead process left; see the scheduler's sweep.
    Running,
    Done,
    /// The model refused, the endpoint was unreachable, the turn timed out.
    Failed,
    /// Never started, and why is in `reason`: its moment was missed, the
    /// assistant is switched off, the vault was locked.
    Skipped,
}

impl Outcome {
    pub const ALL: [Outcome; 5] =
        [Outcome::Queued, Outcome::Running, Outcome::Done, Outcome::Failed, Outcome::Skipped];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Queued => "queued",
            Outcome::Running => "running",
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
        }
    }

    /// Whether this run is over, one way or another.
    pub fn is_finished(self) -> bool {
        !matches!(self, Outcome::Queued | Outcome::Running)
    }
}

/// One run of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RoutineRun {
    pub id: RoutineRunId,
    pub routine_id: RoutineId,
    /// What this routine was called when it ran, so a log row still reads
    /// after the routine is renamed or deleted.
    #[serde(default)]
    pub routine_name: String,
    /// The scheduled moment this run is for, so two ticks cannot both claim
    /// one. Absent for a run somebody asked for by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<Timestamp>,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    pub outcome: Outcome,
    /// Why it failed or was skipped. Empty when it simply worked.
    #[serde(default)]
    pub reason: String,
    /// What it was about: the meeting, the task. Only the query triggers set
    /// one, and it is also how a second run for the same meeting is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The transcript. Absent for a run that never reached the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// The model's last message: what it has to say for itself.
    #[serde(default)]
    pub summary: String,
    /// Whether anybody has looked at it. What the count on the app bar is.
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub steps: u32,
    /// Where the result was sent besides the app, one entry per place the
    /// routine's [`Delivery`] named, each saying whether it got there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered: Vec<Delivered>,
    /// What a brief's run wrote, taken apart: the headline, the sections, the
    /// spoken brief and the digest it was written from. Only a brief's run
    /// that finished has one. See [`crate::brief`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Box<Brief>>,
}

impl RoutineRun {
    pub fn new(routine: &Routine, slot: Option<Timestamp>) -> Self {
        Self {
            id: RoutineRunId::new(),
            routine_id: routine.id,
            routine_name: routine.name.clone(),
            slot,
            started_at: Timestamp::now(),
            finished_at: None,
            outcome: Outcome::Queued,
            reason: String::new(),
            subject: None,
            conversation_id: None,
            summary: String::new(),
            seen: false,
            steps: 0,
            delivered: Vec::new(),
            brief: None,
        }
    }

    /// A run that never started, and why.
    ///
    /// Recorded rather than logged, because the person's question the next
    /// morning is "why did I not get my brief", and a log line on a machine
    /// under a desk is not an answer.
    pub fn skipped(routine: &Routine, slot: Option<Timestamp>, reason: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            outcome: Outcome::Skipped,
            reason: reason.into(),
            finished_at: Some(now),
            // Nothing to look at, so nothing to mark as looked at. A skipped
            // run must not put a number on the app bar.
            seen: true,
            ..Self::new(routine, slot)
        }
    }

    /// Mark this run as under way, now.
    ///
    /// `started_at` is re-stamped because a queued run may have waited: the
    /// log should say when the work happened rather than when it was asked
    /// for.
    pub fn begin(&mut self) {
        self.outcome = Outcome::Running;
        self.started_at = Timestamp::now();
    }

    pub fn finish(&mut self, outcome: Outcome, summary: impl Into<String>) {
        self.outcome = outcome;
        self.summary = summary.into();
        self.finished_at = Some(Timestamp::now());
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.reason = reason.into();
        self.finish(Outcome::Failed, String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    #[test]
    fn a_before_event_trigger_saved_with_a_role_still_names_its_area() {
        // How a routine saved before areas were renamed from roles spelled it.
        let id = AreaId::new();
        let old = serde_json::json!({ "type": "beforeEvent", "leadMinutes": 30, "roleId": id });
        let trigger: Trigger = serde_json::from_value(old).expect("the old spelling reads");
        assert_eq!(trigger, Trigger::BeforeEvent { lead_minutes: 30, area_id: Some(id) });
        let json = serde_json::to_value(&trigger).expect("a trigger serialises");
        assert_eq!(json["areaId"], id.to_string());
    }

    /// A routine that has never run, made a week before the tests look at it.
    fn morning(days: &[Weekday]) -> Routine {
        let mut r = Routine::new(
            "Morning brief",
            "Say what is due today.",
            Trigger::Schedule { at: time(7, 0, 0, 0), days: days.to_vec(), day_of_month: None },
        );
        r.created_at = date(2026, 9, 1).at(0, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
        r
    }

    fn at(y: i16, m: i8, d: i8, h: i8, min: i8, zone: &str) -> Zoned {
        date(y, m, d).at(h, min, 0, 0).in_tz(zone).expect("a real local time")
    }

    #[test]
    fn a_routine_is_due_at_its_hour_and_not_before() {
        let mut r = morning(&[]);
        // The steady state: it ran yesterday, as it does every day.
        r.last_run_at = Some(at(2026, 9, 7, 7, 0, "UTC").timestamp());
        assert_eq!(r.is_due(&at(2026, 9, 8, 6, 59, "UTC")), Due::Later, "not yet");
        assert!(matches!(r.is_due(&at(2026, 9, 8, 7, 0, "UTC")), Due::Now { .. }), "on the hour");
        assert!(matches!(r.is_due(&at(2026, 9, 8, 7, 59, "UTC")), Due::Now { .. }), "within grace");
    }

    #[test]
    fn a_slot_missed_by_more_than_its_grace_is_missed_rather_than_run_late() {
        let r = morning(&[]);
        // A morning brief at half past two in the afternoon is not a morning
        // brief. The scheduler writes a skipped row saying so.
        assert!(matches!(r.is_due(&at(2026, 9, 8, 14, 30, "UTC")), Due::Missed { .. }));
    }

    #[test]
    fn a_routine_that_has_already_run_for_its_slot_is_not_due_again() {
        let mut r = morning(&[]);
        let Due::Now { slot } = r.is_due(&at(2026, 9, 8, 7, 5, "UTC")) else {
            panic!("should be due");
        };
        r.last_run_at = Some(slot);
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 30, "UTC")), Due::Later, "one run per slot");
        // And the next morning it is due again.
        assert!(matches!(r.is_due(&at(2026, 9, 9, 7, 5, "UTC")), Due::Now { .. }));
    }

    #[test]
    fn a_weekday_routine_sleeps_through_the_weekend() {
        // 12 September 2026 is a Saturday.
        let mut r = morning(&Weekday::WEEKDAYS);
        r.last_run_at = Some(at(2026, 9, 11, 7, 0, "UTC").timestamp());
        assert_eq!(
            r.is_due(&at(2026, 9, 12, 7, 5, "UTC")),
            Due::Later,
            "Saturday is not a weekday"
        );
        assert_eq!(r.is_due(&at(2026, 9, 13, 7, 5, "UTC")), Due::Later, "nor is Sunday");
        assert!(matches!(r.is_due(&at(2026, 9, 14, 7, 5, "UTC")), Due::Now { .. }), "Monday is");
    }

    #[test]
    fn a_slot_that_existed_before_the_routine_did_is_not_one_it_missed() {
        // Set up at nine in the morning, for seven in the morning. This
        // morning's seven o'clock is not a slot it failed to run.
        let mut r = morning(&[]);
        r.created_at = at(2026, 9, 8, 9, 0, "UTC").timestamp();
        assert_eq!(r.is_due(&at(2026, 9, 8, 9, 1, "UTC")), Due::Later);
        assert!(
            matches!(r.is_due(&at(2026, 9, 9, 7, 5, "UTC")), Due::Now { .. }),
            "and tomorrow's is"
        );
    }

    #[test]
    fn a_switched_off_routine_is_never_due() {
        let mut r = morning(&[]);
        r.enabled = false;
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 5, "UTC")), Due::Later);
        assert_eq!(r.next_due(&at(2026, 9, 8, 7, 5, "UTC")), None);
    }

    #[test]
    fn a_manual_routine_is_never_due_on_the_clock() {
        let r = Routine::new("On demand", "Do the thing.", Trigger::Manual);
        assert_eq!(r.is_due(&at(2026, 9, 8, 7, 5, "UTC")), Due::Later);
        assert!(!r.trigger.is_clock());
    }

    #[test]
    fn the_hour_is_the_persons_hour_and_it_survives_the_clocks_going_back() {
        // 1 November 2026, America/Los_Angeles: 01:00 happens twice. A
        // routine set for 01:30 must fire once, at a real instant, and be
        // reckoned in the local calendar rather than by adding 86 400
        // seconds to yesterday.
        let mut r = morning(&[]);
        r.trigger = Trigger::Schedule { at: time(1, 30, 0, 0), days: vec![], day_of_month: None };
        r.created_at = at(2026, 10, 30, 0, 0, "America/Los_Angeles").timestamp();

        let after = at(2026, 11, 1, 4, 0, "America/Los_Angeles");
        let (Due::Now { slot } | Due::Missed { slot }) = r.is_due(&after) else {
            panic!("a slot exists on the long day")
        };
        let local = slot.to_zoned(after.time_zone().clone());
        assert_eq!(local.hour(), 1, "the wall clock said half past one: got {local}");
        assert_eq!(local.minute(), 30);
    }

    #[test]
    fn the_hour_survives_the_clocks_going_forward_over_the_gap() {
        // 8 March 2026, America/Los_Angeles: 02:00 to 03:00 does not exist. A
        // routine set for 02:30 still has to have a moment that day, and it
        // is the first instant that does exist.
        let mut r = morning(&[]);
        r.trigger = Trigger::Schedule { at: time(2, 30, 0, 0), days: vec![], day_of_month: None };
        r.created_at = at(2026, 3, 7, 0, 0, "America/Los_Angeles").timestamp();

        let after = at(2026, 3, 8, 9, 0, "America/Los_Angeles");
        let slot = r.slot_at_or_before(&after).expect("the gap must not swallow the day");
        let local = slot.to_zoned(after.time_zone().clone());
        assert_eq!(local.date(), date(2026, 3, 8), "it is still that morning: got {local}");
    }

    #[test]
    fn next_due_is_the_following_slot_and_never_this_one() {
        let r = morning(&Weekday::WEEKDAYS);
        // Friday 11 September, after seven: the next is Monday.
        let next = r.next_due(&at(2026, 9, 11, 8, 0, "UTC")).unwrap();
        assert_eq!(next.to_zoned(jiff::tz::TimeZone::UTC).date(), date(2026, 9, 14));
        // Friday before seven: the next is today.
        let next = r.next_due(&at(2026, 9, 11, 6, 0, "UTC")).unwrap();
        assert_eq!(next.to_zoned(jiff::tz::TimeZone::UTC).date(), date(2026, 9, 11));
    }

    #[test]
    fn a_trigger_says_when_it_runs_in_words() {
        let seven = time(7, 0, 0, 0);
        let cases = [
            (
                Trigger::Schedule { at: seven, days: vec![], day_of_month: None },
                "Every day at 07:00",
            ),
            (
                Trigger::Schedule {
                    at: seven,
                    days: Weekday::WEEKDAYS.to_vec(),
                    day_of_month: None,
                },
                "Weekdays at 07:00",
            ),
            (
                Trigger::Schedule {
                    at: seven,
                    days: vec![Weekday::Sat, Weekday::Sun],
                    day_of_month: None,
                },
                "Weekends at 07:00",
            ),
            (
                Trigger::Schedule { at: seven, days: vec![Weekday::Wed], day_of_month: None },
                "Wed at 07:00",
            ),
            (Trigger::TaskDue { lead_days: 1 }, "The day before a task is due"),
            (Trigger::Manual, "Only when you ask"),
        ];
        for (trigger, said) in cases {
            assert_eq!(trigger.describe(), said);
        }
    }

    #[test]
    fn a_day_is_parsed_however_it_was_typed() {
        for (typed, want) in [
            ("mon", Weekday::Mon),
            ("Monday", Weekday::Mon),
            (" TUE ", Weekday::Tue),
            ("saturday", Weekday::Sat),
        ] {
            assert_eq!(Weekday::parse(typed), Some(want), "{typed}");
        }
        assert_eq!(Weekday::parse("someday"), None);
    }

    #[test]
    fn a_routine_needs_a_name_and_something_to_do() {
        let mut r = Routine::new("Brief", "Say what is due.", Trigger::Manual);
        r.validate().expect("a usable routine");
        r.name = "  ".into();
        assert!(r.validate().is_err());
        r.name = "Brief".into();
        r.instructions = String::new();
        assert!(r.validate().is_err(), "a routine with no instructions is an empty prompt");
    }

    /// A monthly routine's day of the month.
    fn monthly(dom: u8, grace_minutes: u32) -> Routine {
        let mut r = Routine::new(
            "Monthly dream",
            "Look back over the month.",
            Trigger::Schedule { at: time(4, 0, 0, 0), days: vec![], day_of_month: Some(dom) },
        );
        r.grace_minutes = grace_minutes;
        r.created_at = date(2026, 8, 1).at(0, 0, 0, 0).in_tz("UTC").unwrap().timestamp();
        r
    }

    #[test]
    fn a_monthly_routine_on_the_first_is_due_on_the_first_and_not_the_second() {
        let r = monthly(1, 60);
        assert!(
            matches!(r.is_due(&at(2026, 9, 1, 4, 5, "UTC")), Due::Now { .. }),
            "the first, within grace"
        );
        let mut ran = r.clone();
        let Due::Now { slot } = ran.is_due(&at(2026, 9, 1, 4, 5, "UTC")) else {
            panic!("should be due")
        };
        ran.last_run_at = Some(slot);
        assert_eq!(
            ran.is_due(&at(2026, 9, 2, 4, 5, "UTC")),
            Due::Later,
            "the second is not the first"
        );
    }

    #[test]
    fn a_monthly_routine_crosses_the_month_boundary() {
        let mut r = monthly(1, 60);
        // Last ran on 1 August; 1 September is a fresh slot even though it
        // is only one calendar day after the routine's own creation month.
        r.last_run_at = Some(at(2026, 8, 1, 4, 0, "UTC").timestamp());
        assert!(matches!(r.is_due(&at(2026, 9, 1, 4, 5, "UTC")), Due::Now { .. }));
    }

    #[test]
    fn a_monthly_routines_grace_can_span_several_days() {
        // The monthly dream's own grace: six days. Checked on the fourth,
        // three days late, it is still the first's slot and still due.
        let r = monthly(1, 6 * 24 * 60);
        assert!(
            matches!(r.is_due(&at(2026, 9, 4, 9, 0, "UTC")), Due::Now { .. }),
            "three days late is within a six-day grace"
        );
        // Eight days late is past it.
        assert!(matches!(r.is_due(&at(2026, 9, 9, 4, 5, "UTC")), Due::Missed { .. }));
    }

    #[test]
    fn a_monthly_schedule_refuses_a_day_outside_the_first_28() {
        let mut r = monthly(1, 60);
        r.validate().expect("day 1 is fine");
        r.trigger =
            Trigger::Schedule { at: time(4, 0, 0, 0), days: vec![], day_of_month: Some(29) };
        assert!(r.validate().is_err(), "the 29th does not exist in every month");
        r.trigger = Trigger::Schedule { at: time(4, 0, 0, 0), days: vec![], day_of_month: Some(0) };
        assert!(r.validate().is_err());
    }

    #[test]
    fn a_trigger_says_monthly_with_the_right_ordinal() {
        let cases = [(1u8, "1st"), (2, "2nd"), (3, "3rd"), (4, "4th"), (11, "11th"), (21, "21st")];
        for (dom, want) in cases {
            let trigger =
                Trigger::Schedule { at: time(4, 0, 0, 0), days: vec![], day_of_month: Some(dom) };
            assert_eq!(trigger.describe(), format!("Monthly on the {want} at 04:00"));
        }
    }

    #[test]
    fn a_dreams_kind_and_defaults_are_the_applications() {
        for scope in DreamScope::ALL {
            let r = Routine::dream(scope);
            assert_eq!(r.kind, RoutineKind::Dream { scope });
            assert!(!r.enabled, "set_dreaming is what turns it on");
            // Empty instructions must not fail validation for a dream: the
            // box is the person's optional paragraph, not the whole prompt.
            r.validate().expect("a dream with nothing added yet is still valid");
        }
    }

    #[test]
    fn the_work_done_once_a_week_or_a_month_asks_for_the_large_model() {
        use crate::llm::ModelTier;
        // Where the inferring is done, and the one brief that brings the week
        // together: Large, which answers with the medium model when none is set.
        assert_eq!(Routine::dream(DreamScope::Week).model, Some(ModelTier::Large));
        assert_eq!(Routine::dream(DreamScope::Month).model, Some(ModelTier::Large));
        assert_eq!(Routine::brief(BriefKind::Weekly).model, Some(ModelTier::Large));
        // The daily ones are the assistant's own medium model, as ever.
        assert_eq!(Routine::dream(DreamScope::Day).model, None);
        for kind in [BriefKind::Morning, BriefKind::Evening, BriefKind::Podcast] {
            assert_eq!(Routine::brief(kind).model, None, "{kind:?}");
        }
        Routine::brief(BriefKind::Weekly).validate().expect("still a routine that would save");
    }

    #[test]
    fn a_reply_of_nothing_to_report_is_recognised_however_it_is_written() {
        for said in [
            NOTHING_TO_REPORT,
            "nothing to report",
            "  Nothing to report.\n",
            "**Nothing to report.**",
            "NOTHING TO REPORT!",
        ] {
            assert!(is_nothing_to_report(said), "{said:?}");
        }
        for said in [
            "",
            "Nothing to report on the Acme thread, but the 3 pm needs a deck.",
            "Nothing to report. Though the dentist rang.",
            "Something to report.",
        ] {
            assert!(!is_nothing_to_report(said), "{said:?}");
        }
    }

    #[test]
    fn a_custom_routine_still_needs_instructions() {
        let mut r = Routine::new("Brief", "", Trigger::Manual);
        assert!(r.validate().is_err());
        r.kind = RoutineKind::Dream { scope: DreamScope::Day };
        r.validate().expect("a dream may have nothing added");
    }

    #[test]
    fn a_skipped_run_is_finished_and_needs_no_looking_at() {
        let r = morning(&[]);
        let run = RoutineRun::skipped(&r, None, "the vault was locked");
        assert_eq!(run.outcome, Outcome::Skipped);
        assert!(run.outcome.is_finished());
        assert!(run.seen, "there is nothing to look at, so nothing to count on the app bar");
        assert_eq!(run.routine_name, "Morning brief", "a log row reads after a rename");
    }

    #[test]
    fn a_routine_saved_before_delivery_existed_stays_in_the_app() {
        let mut json = serde_json::to_value(morning(&[])).unwrap();
        json.as_object_mut().unwrap().remove("delivery");
        let r: Routine = serde_json::from_value(json).unwrap();
        assert_eq!(r.delivery, Delivery::default());
        assert!(r.delivery.is_empty());
        assert!(!r.delivery.asks());
    }

    #[test]
    fn asking_needs_a_chat_to_ask_in() {
        let mut d = Delivery { ask: true, ..Default::default() };
        assert!(!d.asks(), "nowhere to ask");
        d.chat = Some(ChatLinkId::new());
        assert!(d.asks());
    }

    #[test]
    fn a_dream_cannot_be_sent_anywhere() {
        let mut r = Routine::dream(DreamScope::Day);
        r.delivery.email = Some(AccountId::new());
        assert!(r.validate().is_err());
        let mut r = morning(&[]);
        r.delivery.email = Some(AccountId::new());
        r.validate().expect("a person's routine may be emailed");
    }
}
