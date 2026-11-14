//! Keeping in touch, on the scheduler's minute.
//!
//! The records are `karmayoga_core::Vault`'s and the wording is
//! [`karmayoga_core::keepintouch`]'s. What is here is when, and the one part that
//! owns a socket: asking the model who a journal entry says you were in touch
//! with. See `docs/plans/keep-in-touch.md`.
//!
//! # When
//!
//! Once a day, on the first tick after [`RUN_AT`], which is before the nightly
//! dream so that it reads what this wrote. Not at a moment on the clock, because
//! this is a desktop application and may well be shut at half past two. Every
//! minute until then it only looks for tasks somebody has ticked off.
//!
//! # What may fail
//!
//! The journal is the only part that needs a model. A model that cannot be
//! reached costs that part alone, says so in Settings, and leaves the entries to
//! be read tomorrow, since nothing but a whole pass moves
//! [`read_up_to`](KeepInTouch::read_up_to). Sent mail, meetings, suggestions and
//! occasions go ahead regardless.

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use karmayoga_core::keepintouch::{
    KeepInTouch, LOOK_BACK_DAYS, MAX_ASKS_PER_PASS, MENTIONS_SYSTEM, PassReport, mentions_prompt,
    mentions_schema, parse_mentions,
};
use karmayoga_core::llm::ModelTier;
use karmayoga_core::{EntryId, Vault};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The local time a day's pass may start from.
const RUN_AT: Time = Time::constant(2, 30, 0, 0);

/// How long the model may take over one entry. Nobody is waiting, and a long
/// entry on a small model takes longer than a quick ask is allowed.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The vaults a pass is under way for, by address, so that the minute's tick
/// does not start a second while the first waits on a model.
static RUNNING: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Claim the pass for this vault. `None` when one is already running.
fn claim(vault: &Arc<Vault>) -> Option<Claim> {
    let key = Arc::as_ptr(vault) as usize;
    let mut running = RUNNING.lock().unwrap();
    running.get_or_insert_with(HashSet::new).insert(key).then_some(Claim(key))
}

/// A pass under way, given back when it is dropped, however the pass ended.
struct Claim(usize);

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(running) = RUNNING.lock().unwrap().as_mut() {
            running.remove(&self.0);
        }
    }
}

/// One minute's look: tasks ticked off, and whether today's pass is due.
pub(crate) async fn sweep(service: &Arc<Service>) {
    let Some(vault) = service.get() else { return };
    let now = service.now();
    let looked = blocking({
        let vault = vault.clone();
        move || {
            if !vault.supports_library() {
                return Ok((0, false));
            }
            let record = vault.keep_in_touch()?;
            if !record.settings.enabled {
                return Ok((0, false));
            }
            let tz = vault.person_tz();
            // Whatever goes wrong here, the day's pass is still worked out.
            let settled = vault.settle_touch_tasks(now, &tz).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not settle the tasks keeping in touch follows");
                0
            });
            let local = now.to_zoned(zone(&tz));
            Ok((settled, local.time() >= RUN_AT && record.last_run != Some(local.date())))
        }
    })
    .await;
    let (settled, due) = match looked {
        Ok(looked) => looked,
        Err(e) => {
            tracing::warn!(error = %e.message, "could not look at who to keep in touch with");
            return;
        }
    };
    if settled > 0 {
        service.events().changed(Change::new(Kind::Log, Op::Created));
    }
    if due && let Some(claim) = claim(&vault) {
        let service = service.clone();
        tokio::spawn(async move {
            if let Err(e) = run_pass(&service, &vault, Pass::Nightly).await {
                tracing::warn!(error = %e.message, "keeping in touch could not finish its pass");
            }
            drop(claim);
        });
    }
}

/// Run a pass because somebody pressed the button. Refused while one is running,
/// and while the whole thing is switched off, since it reads a journal.
pub(crate) async fn run_now(service: &Arc<Service>) -> CommandResult<PassReport> {
    let vault = service.require()?;
    let Some(_claim) = claim(&vault) else {
        return Err(CommandError::new(codes::CONFLICT, "a check is already running"));
    };
    run_pass(service, &vault, Pass::Asked).await
}

/// Which kind of pass, for the two things that differ.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// The day's. Claims the day, leaves today's entry for when today is over,
    /// and moves [`KeepInTouch::read_up_to`].
    Nightly,
    /// Asked for. Reads today's entry as it stands, which is what somebody
    /// trying this out expects, and leaves the nightly pass's bookkeeping alone.
    /// Whatever it logs, that pass will find already there.
    Asked,
}

fn zone(tz: &str) -> TimeZone {
    TimeZone::get(tz).unwrap_or(TimeZone::UTC)
}

fn days_before(day: Date, days: i64) -> Date {
    day.checked_sub(Span::new().days(days)).unwrap_or(day)
}

async fn run_pass(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    pass: Pass,
) -> CommandResult<PassReport> {
    let now = service.now();
    let (record, tz) = blocking({
        let vault = vault.clone();
        move || Ok((vault.keep_in_touch()?, vault.person_tz()))
    })
    .await?;
    if !record.settings.enabled {
        return Err(CommandError::new(codes::UNSUPPORTED, "keeping in touch is switched off"));
    }
    let today = now.to_zoned(zone(&tz)).date();
    if pass == Pass::Nightly {
        // Before anything that can fail: a pass that fails is not tried again
        // every minute for the rest of the day.
        let vault = vault.clone();
        blocking(move || Ok(vault.update_keep_in_touch(|k| k.last_run = Some(today))?)).await?;
    }

    let mut report = PassReport::default();
    // `Some` when the journal was read through to the end, with whichever
    // entries the model failed on; `None` when it was not read, or reading
    // stopped part-way, and is to be read again tomorrow.
    let mut journal: Option<Vec<EntryId>> = None;
    if record.settings.read_journal {
        match read_journal(vault, &record, today, pass, &tz, now, &mut report).await {
            Ok(failed) => journal = Some(failed),
            Err(problem) => report.journal_problem = Some(problem),
        }
    }

    // Everything that asks no model, each part on its own so that one going
    // wrong does not cost the others. Looked through from where the last night
    // got to, never further back than a pass reads the journal.
    let report = blocking({
        let vault = vault.clone();
        move || {
            let since = record
                .scanned_up_to
                .unwrap_or(today)
                .max(days_before(today, LOOK_BACK_DAYS))
                .min(days_before(today, 1));
            report.logged += counted("sent mail", sent_mail(&vault, since, now, &tz));
            report.logged += counted("meetings", vault.log_meetings(since, now, &tz));
            report.suggested += counted("suggestions", vault.suggest_people(today, now, &tz));
            report.occasions += counted("occasions", vault.propose_occasions(today, now, &tz));
            let midnight = today.at(0, 0, 0, 0).to_zoned(zone(&tz)).ok().map(|z| z.timestamp());
            let kept = report.clone();
            vault.update_keep_in_touch(|k| {
                k.last_report = Some(kept);
                if pass == Pass::Nightly {
                    k.scanned_up_to = Some(today);
                    if let Some(failed) = &journal {
                        k.read_up_to = midnight;
                        // A second chance, and no third: an entry that failed
                        // as a retry is let go.
                        let retried = std::mem::take(&mut k.retry);
                        k.retry =
                            failed.iter().filter(|id| !retried.contains(id)).copied().collect();
                    }
                }
                k.prune(today);
            })?;
            Ok(report)
        }
    })
    .await?;

    if report.logged > 0 {
        service.events().changed(Change::new(Kind::Log, Op::Created));
        service.events().changed(Change::new(Kind::Item, Op::Updated));
    }
    if report.asked + report.suggested + report.occasions > 0 {
        service.events().changed(Change::new(Kind::Proposal, Op::Created));
    }
    Ok(report)
}

/// How many a part wrote, or none and a line in the log.
fn counted(what: &str, result: karmayoga_core::Result<u32>) -> u32 {
    result.unwrap_or_else(|e| {
        tracing::warn!(error = %e, part = what, "keeping in touch skipped a part of its pass");
        0
    })
}

fn sent_mail(vault: &Vault, since: Date, now: Timestamp, tz: &str) -> karmayoga_core::Result<u32> {
    let Ok(from) = since.at(0, 0, 0, 0).to_zoned(zone(tz)) else { return Ok(0) };
    vault.log_sent_mail(from.timestamp(), now, tz)
}

/// After this many entries in a row the model failed on, it is taken to be down
/// rather than refusing one entry, and reading stops for the night.
const FAILURES_BEFORE_STOPPING: u32 = 3;

/// Read what has been written since last time.
///
/// One entry the model fails on -- a refusal, a malformed answer, a timeout --
/// is noted in the report and skipped, and its id is returned to be tried once
/// more tomorrow. `Err` is reading stopping altogether, in words fit to put in
/// Settings: no model to ask, or several failures in a row.
async fn read_journal(
    vault: &Arc<Vault>,
    record: &KeepInTouch,
    today: Date,
    pass: Pass,
    tz: &str,
    now: Timestamp,
    report: &mut PassReport,
) -> Result<Vec<EntryId>, String> {
    let through = match pass {
        Pass::Nightly => days_before(today, 1),
        Pass::Asked => today,
    };
    let looked = blocking({
        let vault = vault.clone();
        let record = record.clone();
        let tz = tz.to_string();
        move || Ok(vault.entries_to_read(&record, today, through, &tz)?)
    })
    .await;
    let entries = looked.map_err(|e| e.message)?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    // The small model if there is one: this is what it is for. The same door
    // a dream goes through, because this is the same act: what somebody wrote,
    // sent to a model because they turned the assistant on.
    let access = {
        let vault = vault.clone();
        blocking(move || {
            let (_, access) = vault
                .agent_credentials(ModelTier::Small)
                .or_else(|_| vault.agent_credentials(ModelTier::Medium))?;
            Ok(access)
        })
        .await
        .map_err(|e| e.message)?
    };

    let total = entries.len();
    let mut failed: Vec<EntryId> = Vec::new();
    let mut in_a_row = 0;
    let mut last_problem = String::new();
    for summary in entries {
        let entry = {
            let vault = vault.clone();
            blocking(move || Ok(vault.entry(summary.id)?)).await.map_err(|e| e.message)?
        };
        let body = entry.body.to_markdown();
        if body.trim().is_empty() {
            continue;
        }
        let title = entry.display_title();
        let client = crate::llm::client_for(&access)
            .map_err(|e| format!("could not reach the model: {e}"))?;
        let asked = crate::quick::run_prompt_within(
            client,
            &access.config.model,
            MENTIONS_SYSTEM,
            &mentions_prompt(&title, entry.local_date, &body),
            mentions_schema(),
            READ_TIMEOUT,
        )
        .await;
        let answer = match asked {
            Ok(answer) => {
                in_a_row = 0;
                answer
            }
            Err(e) => {
                failed.push(entry.id);
                in_a_row += 1;
                last_problem = e.message;
                if in_a_row >= FAILURES_BEFORE_STOPPING {
                    return Err(last_problem);
                }
                continue;
            }
        };
        report.entries_read += 1;

        let mentions = parse_mentions(&answer, &title, &body);
        if mentions.is_empty() {
            continue;
        }
        let asks_left = MAX_ASKS_PER_PASS.saturating_sub(report.asked);
        let done = {
            let vault = vault.clone();
            let tz = tz.to_string();
            let date = entry.local_date;
            let id = entry.id;
            blocking(move || Ok(vault.log_mentions(id, date, &mentions, asks_left, &tz, now)?))
                .await
                .map_err(|e| e.message)?
        };
        report.logged += done.logged;
        report.asked += done.asked;
    }
    if !failed.is_empty() {
        report.journal_problem = Some(format!(
            "{} of {total} entries could not be read, and will be tried once more tomorrow: \
             {last_problem}",
            failed.len()
        ));
    }
    Ok(failed)
}
