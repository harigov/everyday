//! Mail for the assistant, run by the service rather than asked for by a
//! tool: model-assisted categorisation, `summarize_thread`, and the
//! auto-draft background pass. `docs/plans/mail.md`'s phase 7 calls these
//! "the Superhuman layer" and is explicit about where they sit: "these
//! features run in the service, not as tools, but they sit behind the same
//! `MailAssistantSettings` switch and the same named-provider sentence, so
//! there is one answer to 'what sees my mail'." That one answer is
//! [`jatayu_core::mail::mail_ai_allowed`] — every function below calls it,
//! per account, before it reads a word.
//!
//! A fourth pass, [`categorize::prioritize_account`], rides along inside
//! [`categorize_tick`] rather than getting a tick of its own: deciding
//! which `Important` threads need this person's attention soon is still a
//! categorisation, in the same sense "is this `Important` or `Other`" is,
//! so it is gated by the same `MailAiFeature::Categorize` switch and spends
//! from the same per-minute budget as [`categorize::categorize_account`] -- see that
//! function's own doc for why the two sharing one bucket, rather than each
//! getting its own, is deliberate.
//!
//! # Why nothing here is a tool
//!
//! [`jatayu_core::agent::tools::mail`] is what the chat assistant and MCP
//! reach for, one call at a time, on a thread somebody names. These three
//! run on their own schedule, over every eligible account, and answer to
//! nobody's turn — which is also why none of them is rate limited the way a
//! tool call is (`Service::check_mail_rate_limit`, keyed by conversation):
//! [`categorize_tick`] and [`auto_draft_tick`] spend from their own
//! per-minute budgets on [`crate::service::Service`] instead, and `summarize_thread` is the
//! one of the three a person does ask for directly, so it is the one that
//! reuses the tool-call limiter.
//!
//! # Zero's prompt, and where ours differs from it
//!
//! [`categorize::CATEGORIZE_SYSTEM`] began as Mail-0/Zero's own thread-labelling prompt
//! (MIT; `apps/server/src/lib/brain.fallback.prompts.ts`, `ThreadLabels`) --
//! "a precise thread labelling agent... assign relevant labels from a
//! predefined set... never generate new labels." Zero's prompt names its labels
//! and never says what any of them is; ours defines each, says what kind of
//! pile it is looking at (what the rules could not place, mostly first
//! contacts), and where a pitch goes. It also warns that the thread text it is
//! fed might contain instructions rather than only content, per the plan's own
//! rule for every string that reaches a model from somebody else's mail.
//!
//! # What the small passes know, and which threads they read
//!
//! Every small-model pass here is told what day it is (in the user message,
//! never the system prompt, which a provider can cache while it does not
//! change), when each message arrived, and the app's own records of the thread:
//! how it was addressed, whether the person has written to the sender and in the
//! thread. The records come first and the sender's words last, on one line --
//! see [`one_line`] -- so that a body cannot write a line of its own among them.
//!
//! They read the newest Inbox threads of a tab first, and only then walk back
//! with whatever of the budget is left ([`pick_threads`]), and never past
//! [`MailSorting::window_days`] (or, for drafts, the follow-up window): mail
//! that arrives is looked at within a minute, not when a cursor over a few
//! thousand threads happens to wrap, and a deadline from 2023 is never asked
//! about as though it were news. A pitch from a stranger is excluded in the
//! same words everywhere ([`pitch_kinds!`]).

/// The kinds of unsolicited mail that are never what the person needs to read
/// first, in the same words wherever a prompt here excludes them -- the
/// categoriser, the priority list and auto-draft -- so that one of them never
/// has a stranger's pitch described one way and another a different one. A
/// macro, because `concat!` takes literals and these are inside prompts that
/// are constants.
macro_rules! pitch_kinds {
    () => {
        "sales, partnerships, recruiting, PR, link building, requests for a demo or a quick \
         call, and their follow-ups"
    };
}

mod autodraft;
mod categorize;
mod summary;

pub use autodraft::{AUTO_DRAFT_CONVERSATION, auto_draft_tick};
pub use categorize::{CATEGORIZE_PAGE, PRIORITY_PAGE, categorize_tick, sort_new_mail};
pub(crate) use categorize::{Filed, PRIORITY_CONFIDENCE, announce_filed};
pub use summary::{listen_pieces, summarize_thread};
// `crate::arrivals` reaches for this by its old path.
pub(crate) use crate::mail::text::excerpt;

use std::collections::HashSet;
use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::account::Account;
use jatayu_core::id::{AccountId, MailboxId};
use jatayu_core::llm::{ModelAccess, ModelTier};
use jatayu_core::mail::{Category, MailAiFeature, MailAiRefusal, Thread, mail_ai_allowed};
use jatayu_core::prompt;
use jatayu_core::store::mail::ThreadFilter;
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::error::{CommandError, CommandResult, codes};
use crate::service::blocking;

/// The model a mail feature is about to send `account`'s mail to, refused
/// unless the account's acknowledgement names that model's endpoints.
///
/// Checked against [`ModelAccess::acknowledgement`] -- the snapshot the
/// request is about to be made under -- and not against an earlier read of
/// the settings: a check made before an `await` and a request made after it
/// could otherwise be about two different sets of endpoints, if the models
/// were moved in between. Every on-request feature here and in
/// `crate::mail::write` takes its model through this, right before the call.
pub(crate) fn mail_model(
    vault: &Vault,
    account: &Account,
    feature: MailAiFeature,
    tier: ModelTier,
) -> CommandResult<ModelAccess> {
    let access = vault
        .mail_ai_credentials(tier)
        .map_err(|e| CommandError::new(codes::QUICK, e.to_string()))?;
    mail_ai_allowed(account, &access.acknowledgement, feature)
        .map_err(|r| refused(account, feature, r))?;
    Ok(access)
}

fn refused(account: &Account, feature: MailAiFeature, refusal: MailAiRefusal) -> CommandError {
    CommandError::new(codes::FORBIDDEN, refusal.message(account, feature))
}

// ---- what every small-model pass shares ---------------------------------------

/// One line of somebody's words for a model: link markers gone, every run of
/// white space one space -- so nothing in it can pass for a line of its own,
/// and an excerpt costs fewer tokens.
pub(crate) fn one_line(text: &str) -> String {
    jatayu_mail::text::without_link_markers(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What "today" and "when" mean in a prompt: the person's own time zone, and
/// the service's clock. A model asked whether a deadline is close has to be
/// told the date, and the date of what it is reading, or "Friday" comes back as
/// "Friday" from a message of no age at all.
#[derive(Debug, Clone)]
pub(crate) struct Today {
    zone: TimeZone,
    now: Timestamp,
}

impl Today {
    pub(crate) fn new(zone: TimeZone, now: Timestamp) -> Self {
        Self { zone, now }
    }

    /// The person's zone (the machine's when they set none), at `now`.
    pub(crate) fn read(vault: &Vault, now: Timestamp) -> Self {
        let zone = vault.agent_settings().map(|s| s.zone()).unwrap_or_else(|_| TimeZone::system());
        Self::new(zone, now)
    }

    /// "Today is Tuesday 24 November 2026." -- for the start of a user message.
    pub(crate) fn line(&self) -> String {
        prompt::today_line(&self.now.to_zoned(self.zone.clone()))
    }

    /// "Monday 23 November": the day `at` fell on here.
    pub(crate) fn day(&self, at: Timestamp) -> String {
        prompt::day_label(at, &self.zone, self.now.to_zoned(self.zone.clone()).date())
    }

    /// "Monday 23 November (1 day ago)".
    pub(crate) fn received(&self, at: Timestamp) -> String {
        format!("{} ({})", self.day(at), prompt::days_ago(at, self.now))
    }
}

/// Where a small-model pass reads one tab of one account: its Inbox, the
/// threads in `category`, newest first, no older than `since`.
///
/// Not "every mailbox, all of time", which is what the passes read before: that
/// listing is newest first too, but a cursor that moves one page a minute
/// through a few thousand threads reaches this morning's mail hours after it
/// came, long after the hour in which a filed message can still be announced,
/// and spends the minute's budget on archived mail and on deadlines that passed
/// years ago.
struct Lane {
    inbox: MailboxId,
    category: Category,
    since: Timestamp,
}

impl Lane {
    /// `account`'s Inbox, if it has one, as far back as `days` before `now`.
    async fn open(
        vault: &Arc<Vault>,
        account: AccountId,
        category: Category,
        days: impl FnOnce(&jatayu_core::mail::MailSorting) -> u32 + Send + 'static,
        now: Timestamp,
    ) -> CommandResult<Option<Lane>> {
        let vault = vault.clone();
        blocking(move || {
            let Some(inbox) = vault.mail_inbox(account)? else { return Ok(None) };
            let days = days(&vault.mail_sorting()?);
            let since = now
                .checked_sub(jiff::SignedDuration::from_hours(24 * i64::from(days)))
                .unwrap_or(Timestamp::MIN);
            Ok(Some(Lane { inbox, category, since }))
        })
        .await
    }
}

/// One page of a [`Lane`], read by [`recent_page`].
struct RecentPage {
    threads: Vec<Thread>,
    /// Where the page after this one starts. `None` when this page reached the
    /// edge of the window or the end of the list: nothing further back is read.
    next: Option<String>,
}

/// `lane`'s threads from `cursor` (`None`: the newest), at most `limit`, stopping
/// at the first one older than the lane's window. A local read, for inside
/// [`blocking`].
fn recent_page(
    vault: &Vault,
    lane: &Lane,
    cursor: Option<&str>,
    limit: u32,
) -> CommandResult<RecentPage> {
    let filter =
        ThreadFilter { category: Some(lane.category), snoozed: Some(false), ..Default::default() };
    let page = vault.list_threads(lane.inbox, &filter, cursor, limit)?;
    let mut threads = Vec::with_capacity(page.threads.len());
    for thread in page.threads {
        // Newest first, so everything after the first old one is older still.
        if thread.last_date < lane.since {
            return Ok(RecentPage { threads, next: None });
        }
        threads.push(thread);
    }
    Ok(RecentPage { threads, next: page.next_cursor })
}

/// The order a pass asks in: the unasked threads of the newest page, then those
/// of the page the walk reached, none twice. `unasked` is the pass's own
/// marker. Pure, so the order can be checked without a vault.
fn in_asking_order(
    newest: Vec<Thread>,
    walked: Vec<Thread>,
    unasked: impl Fn(&Thread) -> bool,
) -> Vec<Thread> {
    let mut seen = HashSet::new();
    newest.into_iter().chain(walked).filter(|t| unasked(t) && seen.insert(t.id)).collect()
}

/// Where the walk reads from: the cursor it was left at, or, with none, the page
/// after the newest one -- which has just been read. Pure.
fn walk_from(cursor: Option<String>, after_newest: Option<String>) -> Option<String> {
    cursor.or(after_newest)
}

/// What [`pick_threads`] found.
struct Picked {
    /// What to ask about, in the order to ask it.
    threads: Vec<Thread>,
    /// Where the walk goes next tick: `None` to start over behind the newest page.
    cursor: Option<String>,
}

/// The threads a small-model pass asks about this tick: the newest page of the
/// lane first, so new mail is looked at within a minute, and then, when that
/// leaves room (`room` is how many one request takes), a page of the walk
/// back through the rest of the window, which is what eventually reaches every
/// thread in it. The walk's cursor moves whenever a page of it was read,
/// whether or not anything on it needed asking.
async fn pick_threads(
    vault: &Arc<Vault>,
    lane: &Arc<Lane>,
    cursor: Option<String>,
    page: u32,
    room: usize,
    unasked: impl Fn(&Thread) -> bool,
) -> CommandResult<Picked> {
    let newest = {
        let (vault, lane) = (vault.clone(), lane.clone());
        blocking(move || recent_page(&vault, &lane, None, page)).await?
    };
    let after_newest = newest.next.clone();
    let newest = newest.threads;
    let enough = newest.iter().filter(|t| unasked(t)).count() >= room;
    // The newest page already fills the request: leave the walk where it is.
    let Some(from) = (if enough { None } else { walk_from(cursor.clone(), after_newest) }) else {
        return Ok(Picked {
            threads: in_asking_order(newest, Vec::new(), unasked),
            cursor: if enough { cursor } else { None },
        });
    };
    let walked = {
        let (vault, lane) = (vault.clone(), lane.clone());
        blocking(move || recent_page(&vault, &lane, Some(&from), page)).await?
    };
    Ok(Picked { threads: in_asking_order(newest, walked.threads, unasked), cursor: walked.next })
}

/// "yes" or "no", for a record said in a prompt line.
fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::ai::autodraft::AUTO_DRAFT_SYSTEM;
    use crate::mail::ai::categorize::{CATEGORIZE_SYSTEM, PRIORITIZE_SYSTEM};
    use crate::mail::ai::fixtures::{at, listed, today};

    #[test]
    fn today_is_said_in_the_persons_own_zone_and_days_are_counted_from_it() {
        // Half past eleven on the 24th in London is half past noon on the 25th in
        // Auckland: one instant, two days.
        let late: Timestamp = "2026-11-24T23:30:00Z".parse().unwrap();
        let london = Today::new(TimeZone::get("Europe/London").unwrap(), late);
        let auckland = Today::new(TimeZone::get("Pacific/Auckland").unwrap(), late);
        assert_eq!(london.line(), "Today is Tuesday 24 November 2026.");
        assert_eq!(auckland.line(), "Today is Wednesday 25 November 2026.");
        assert_eq!(
            today().day(at("2025-01-02")),
            "Thursday 2 January 2025",
            "the year when not this one"
        );
        assert_eq!(today().received(at("2026-11-24")), "Tuesday 24 November (today)");
    }

    #[test]
    fn the_newest_page_is_asked_before_the_walk_and_nothing_twice() {
        let mut newest = listed(3);
        let mut walked = listed(3);
        // The walk may meet a thread the newest page already had.
        walked.push(newest[1].clone());
        // One already asked, in each.
        newest[0].ai_categorize_asked_at_count = Some(1);
        walked[0].ai_categorize_asked_at_count = Some(1);
        let ids = |threads: &[Thread]| threads.iter().map(|t| t.id).collect::<Vec<_>>();
        let expected = [&newest[1], &newest[2], &walked[1], &walked[2]].map(|t| t.id);

        let order = in_asking_order(newest.clone(), walked.clone(), |t| {
            t.ai_categorize_asked_at_count != Some(t.message_count)
        });
        assert_eq!(ids(&order), expected, "the newest page's first, then the walk's, each once");
    }

    #[test]
    fn the_walk_goes_on_from_its_cursor_or_starts_behind_the_newest_page() {
        assert_eq!(walk_from(Some("c7".into()), Some("c1".into())).as_deref(), Some("c7"));
        assert_eq!(walk_from(None, Some("c1".into())).as_deref(), Some("c1"));
        assert_eq!(walk_from(None, None), None, "the newest page was the whole window");
    }

    #[test]
    fn a_stranger_is_filed_other_everywhere_in_the_same_words() {
        for system in [CATEGORIZE_SYSTEM, PRIORITIZE_SYSTEM, AUTO_DRAFT_SYSTEM] {
            assert!(system.contains(pitch_kinds!()), "{system}");
            assert!(system.contains("never written to"), "{system}");
        }
    }
}
