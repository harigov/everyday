//! Sending a finished run's result to where its routine says, besides the app.
//!
//! A routine's [`Delivery`] names up to two places: the owner's chat on one
//! of their chat links, and their own inbox through one of their mail
//! accounts. The run is kept in the Assistant app whatever happens here; this
//! adds copies, and writes down on the run whether each one got there, so
//! "why did I not get my brief on my phone" has an answer in the log.
//!
//! What is sent is one message: the routine's name, then the model's last
//! word -- which it was told would be sent, and wrote as that message (see
//! `agent::unattended_context`) -- then a line about anything it left waiting
//! for a decision. A run that failed says so, once per streak of failures,
//! exactly as the desktop notification does.
//!
//! The email goes from the account to its own address and nowhere else. The
//! address is read from the account, never from anything a model wrote, so a
//! routine set up by the assistant cannot be talked into mailing somebody
//! else.

use crate::chat::{ChatOutlet, FromRun};
use crate::service::{Service, blocking};
use everyday_core::mail::{Address, Draft, Origin};
use everyday_core::routine::{Delivered, DeliveredTo, Outcome, Routine, RoutineRun};
use everyday_core::{ProposalQuery, Vault};
use std::sync::Arc;
use std::time::Duration;

/// How long a message to the chat may take to go before the run stops
/// waiting for it. Generous, because it waits behind whatever the owner's
/// own chat is doing at the time.
const POST_TIMEOUT: Duration = Duration::from_secs(120);

/// Send `run`'s result to every place `routine` names. One [`Delivered`] per
/// place, in the order the routine's [`Delivery`] lists them; empty when it
/// names none, or the run has nothing to say.
///
/// `outlet` is the chat link's, looked up when the run started -- `None` when
/// the routine names no chat, or names one that is not running here.
pub(crate) async fn deliver(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    run: &RoutineRun,
    outlet: Option<Arc<dyn ChatOutlet>>,
) -> Vec<Delivered> {
    let delivery = &routine.delivery;
    if routine.kind.is_dream() || delivery.is_empty() {
        return Vec::new();
    }
    let waiting = blocking({
        let vault = vault.clone();
        let run_id = run.id;
        move || {
            if !vault.supports_proposals() {
                return Ok(0);
            }
            let query = ProposalQuery { run_id: Some(run_id), ..ProposalQuery::pending() };
            Ok(vault.proposals(&query)?.len())
        }
    })
    .await
    .unwrap_or(0);
    let Some(message) = report(&routine.name, run, waiting) else { return Vec::new() };

    let mut delivered = Vec::new();
    if let Some(link) = delivery.chat {
        let label = blocking({
            let vault = vault.clone();
            move || Ok(vault.chat_settings()?.link(link).map(|l| l.service.label().to_string()))
        })
        .await
        .ok()
        .flatten();
        delivered.push(match (label, outlet) {
            (None, _) => Delivered {
                to: DeliveredTo::Chat,
                ok: false,
                detail: "the chat link it named no longer exists".into(),
            },
            (Some(label), None) => Delivered {
                to: DeliveredTo::Chat,
                ok: false,
                detail: format!("{label} was not connected"),
            },
            (Some(label), Some(outlet)) => {
                let from =
                    FromRun { routine: routine.name.clone(), conversation: run.conversation_id };
                match tokio::time::timeout(POST_TIMEOUT, outlet.post(from, message.clone())).await {
                    Ok(Ok(())) => Delivered { to: DeliveredTo::Chat, ok: true, detail: label },
                    Ok(Err(why)) => Delivered { to: DeliveredTo::Chat, ok: false, detail: why },
                    Err(_) => Delivered {
                        to: DeliveredTo::Chat,
                        ok: false,
                        detail: format!("{label} did not take the message in time"),
                    },
                }
            }
        });
    }
    if let Some(account) = delivery.email {
        let sent = blocking({
            let vault = vault.clone();
            let (name, run) = (routine.name.clone(), run.clone());
            let html = html_of(&message);
            let origin = Origin::Routine { run: run.id.to_string() };
            move || {
                let subject = subject(&name, &run, vault.agent_settings().ok());
                let account = vault.account(account)?;
                if !account.services.mail {
                    return Ok(Err(format!("mail is switched off for {}", account.address)));
                }
                let mut draft = Draft::new(account.id, account.address.clone(), origin.clone());
                draft.to =
                    vec![Address::new(account.display_name.clone(), account.address.clone())];
                draft.subject = subject;
                draft.body_html = html;
                vault.save_draft(&draft)?;
                vault.queue_draft_send(draft.id, jiff::Timestamp::now(), origin)?;
                Ok(Ok((account.id, account.address)))
            }
        })
        .await;
        delivered.push(match sent {
            Ok(Ok((account, address))) => {
                service.notify_mail_write(account);
                Delivered { to: DeliveredTo::Email, ok: true, detail: address }
            }
            Ok(Err(why)) => Delivered { to: DeliveredTo::Email, ok: false, detail: why },
            Err(e) => Delivered {
                to: DeliveredTo::Email,
                ok: false,
                detail: format!("the mail account could not be used: {}", e.message),
            },
        });
    }
    delivered
}

/// The one message a run's result goes out as, in Markdown -- or `None` when
/// there is nothing worth sending.
///
/// `waiting` is how many of this run's proposals are still waiting for a
/// decision in the app.
pub(crate) fn report(name: &str, run: &RoutineRun, waiting: usize) -> Option<String> {
    let title = format!("**{}**", name.trim());
    match run.outcome {
        Outcome::Done => {
            let said = run.summary.trim();
            if said.is_empty() && run.steps == 0 && waiting == 0 {
                return None;
            }
            let mut out = title;
            if !said.is_empty() {
                out.push_str("\n\n");
                out.push_str(said);
            } else if waiting == 0 {
                out.push_str(
                    "\n\nIt finished without a word. What it did is in the Assistant app.",
                );
            }
            if waiting > 0 {
                let what = if waiting == 1 {
                    "One thing is".to_string()
                } else {
                    format!("{waiting} things are")
                };
                out.push_str(&format!(
                    "\n\n_{what} waiting for you to decide in the Assistant app._"
                ));
            }
            Some(out)
        }
        Outcome::Failed => Some(format!("{title} could not run: {}", run.reason.trim())),
        Outcome::Skipped | Outcome::Queued | Outcome::Running => None,
    }
}

/// "Morning brief — Mon 12 Oct", in the assistant's own time zone.
fn subject(name: &str, run: &RoutineRun, settings: Option<everyday_core::AgentSettings>) -> String {
    let zone =
        settings.map(|s| s.now().time_zone().clone()).unwrap_or_else(jiff::tz::TimeZone::system);
    let day = run.started_at.to_zoned(zone).strftime("%a %-d %b").to_string();
    format!("{} \u{2014} {day}", name.trim())
}

/// `markdown` as the body of an email.
///
/// Raw HTML in it is shown as text rather than passed through: the words are
/// a model's, and a model's output is not markup this application vouches
/// for, even in a message to the person's own inbox.
pub(crate) fn html_of(markdown: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};
    let parser = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES)
        .map(|event| match event {
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            other => other,
        });
    let mut body = String::new();
    html::push_html(&mut body, parser);
    format!(
        "<div style=\"font-family: -apple-system, 'Segoe UI', sans-serif; line-height: 1.5\">\
         {body}<p style=\"color: #888; font-size: 12px\">Sent by a routine in Every Day.</p></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::routine::Trigger;

    fn run(outcome: Outcome, summary: &str, steps: u32) -> RoutineRun {
        let routine = Routine::new("Morning brief", "Say what is due.", Trigger::Manual);
        let mut run = RoutineRun::new(&routine, None);
        run.outcome = outcome;
        run.summary = summary.into();
        run.steps = steps;
        run
    }

    #[test]
    fn a_finished_run_is_its_name_then_what_it_said() {
        let said = report("Morning brief", &run(Outcome::Done, "Three things are due.", 2), 0);
        assert_eq!(said.as_deref(), Some("**Morning brief**\n\nThree things are due."));
    }

    #[test]
    fn what_it_left_for_a_decision_is_mentioned() {
        let said = report("Tidy", &run(Outcome::Done, "Done.", 1), 2).unwrap();
        assert!(said.ends_with("_2 things are waiting for you to decide in the Assistant app._"));
        let said = report("Tidy", &run(Outcome::Done, "", 1), 1).unwrap();
        assert_eq!(
            said,
            "**Tidy**\n\n_One thing is waiting for you to decide in the Assistant app._"
        );
    }

    #[test]
    fn a_run_that_did_things_but_said_nothing_says_where_to_look() {
        let said = report("Tidy", &run(Outcome::Done, "", 3), 0).unwrap();
        assert_eq!(
            said,
            "**Tidy**\n\nIt finished without a word. What it did is in the Assistant app."
        );
    }

    #[test]
    fn a_run_with_nothing_to_say_sends_nothing() {
        assert_eq!(report("Tidy", &run(Outcome::Done, "  ", 0), 0), None);
        assert_eq!(report("Tidy", &run(Outcome::Skipped, "", 0), 0), None);
    }

    #[test]
    fn a_failure_says_why() {
        let mut failed = run(Outcome::Failed, "", 0);
        failed.reason = "the endpoint was unreachable".into();
        assert_eq!(
            report("Morning brief", &failed, 0).as_deref(),
            Some("**Morning brief** could not run: the endpoint was unreachable")
        );
    }

    #[test]
    fn the_email_is_the_markdown_rendered_with_any_html_shown_as_text() {
        let html = html_of("**Due today**\n\n- Book the dentist\n\n<script>alert(1)</script>");
        assert!(html.contains("<strong>Due today</strong>"));
        assert!(html.contains("<li>Book the dentist</li>"));
        assert!(!html.contains("<script>"), "got {html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_subject_names_the_routine_and_the_day() {
        let mut r = run(Outcome::Done, "x", 1);
        r.started_at = "2026-10-12T07:00:00Z".parse().unwrap();
        let settings =
            everyday_core::AgentSettings { timezone: Some("UTC".into()), ..Default::default() };
        assert_eq!(
            subject("Morning brief", &r, Some(settings)),
            "Morning brief \u{2014} Mon 12 Oct"
        );
    }
}
