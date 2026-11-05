//! Writing help end to end, against a fake model that keeps what it was sent:
//! what a suggested reply is written from, and the signature a draft starts
//! with. Everything a person sees depends on what reaches the prompt, and only
//! the whole path can say a meeting arrived there as a meeting.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, MailAi, Provider};
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, Event, EventStatus,
};
use everyday_core::id::{CalendarId, EventId, MailMessageId, PackId, ThreadId};
use everyday_core::llm::{LLMConfig, LLMModelConfig, ModelSettings};
use everyday_core::mail::{
    Address, Body, Category, CategorySource, MailWriting, Mailbox, MailboxRole, Message,
    MessageFlags,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_core::task::{BlockSubject, TimeBlock};
use everyday_service::{Ctx, Service};
use jiff::Timestamp;
use serde_json::{Value, json};

const ZONE: &str = "America/Los_Angeles";

// ---- a model that remembers ------------------------------------------------

struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
    sent: Arc<Mutex<Vec<Value>>>,
}

impl FakeModel {
    /// Requests so far. `run_prompt` makes more than one to get one answer --
    /// the tool call, then its result -- so a test can compare these but not
    /// count them.
    fn asked(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// The system and user messages of the `n`th request.
    fn prompt(&self, n: usize) -> (String, String) {
        let sent = self.sent.lock().unwrap();
        let messages = sent[n]["messages"].as_array().unwrap().clone();
        let of = |role: &str| {
            messages
                .iter()
                .filter(|m| m["role"] == role)
                .map(|m| match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => {
                        parts.iter().filter_map(|p| p["text"].as_str()).collect::<String>()
                    }
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        (of("system"), of("user"))
    }
}

async fn fake_model(arguments: Value) -> FakeModel {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_for_task = sent.clone();
    let reply = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "scripted",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "submit", "arguments": arguments.to_string() },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string();

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let sent = sent_for_task.clone();
            let reply = reply.clone();
            tokio::spawn(async move {
                // The whole request: a prompt with a calendar in it is larger
                // than one read.
                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    let Some(head_end) = text.find("\r\n\r\n") else { continue };
                    let length = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + length {
                        let body = &buf[head_end + 4..head_end + 4 + length];
                        sent.lock().unwrap().push(serde_json::from_slice(body).unwrap());
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx, sent }
}

fn three_replies() -> Value {
    json!({ "replies": [
        { "label": "Offer Tuesday", "body": "Tuesday at 2 works.\n\nThanks,\nHari" },
        { "label": "Not this week", "body": "Not this week, sorry." },
        { "label": "Ask what for", "body": "What is it about?" },
    ]})
}

// ---- fixtures --------------------------------------------------------------

/// Thursday 5 November 2026, 8am in Los Angeles.
fn thursday_morning() -> Timestamp {
    at("2026-11-05", 8, 0)
}

fn at(day: &str, hour: i8, minute: i8) -> Timestamp {
    let day: jiff::civil::Date = day.parse().unwrap();
    day.at(hour, minute, 0, 0).in_tz(ZONE).unwrap().timestamp()
}

struct Env {
    svc: Arc<Service>,
    account: Account,
    thread: ThreadId,
    message: MailMessageId,
    clock: Arc<support::clock::FakeClock>,
    _dir: tempfile::TempDir,
}

fn env(endpoint: &str) -> Env {
    let (svc, dir) = support::vault::service(None);
    let clock = Arc::new(support::clock::FakeClock::at(thursday_morning()));
    svc.set_clock(clock.clone());
    let vault = svc.get().unwrap();
    let settings = everyday_core::AgentSettings {
        enabled: true,
        timezone: Some(ZONE.into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    models.small = Some(LLMConfig {
        connection: models.medium.connection.clone(),
        model: LLMModelConfig {
            model: "scripted".into(),
            temperature: Some(0.0),
            max_tokens: Some(512),
            reasoning_effort: None,
        },
    });
    vault.save_model_settings(&models).unwrap();

    let mut account = Account::new(Provider::Custom, "hari@example.com");
    account.display_name = "Hari G".into();
    account.services.mail = true;
    account.mail_ai = MailAi { writing: true, ..Default::default() };
    account.assistant_provider_acknowledged = Some(models.acknowledgement_name());
    vault.save_account(&account).unwrap();

    let mailbox = Mailbox::new(account.id, "Inbox", MailboxRole::Inbox);
    vault.save_mailbox(&mailbox).unwrap();
    let thread = ThreadId::new();
    let message = Message {
        id: MailMessageId::new(),
        account_id: account.id,
        thread_id: thread,
        message_id_header: "<one@example.com>".to_string(),
        date: thursday_morning(),
        from: Address::new("Dana Scully", "dana@example.com"),
        to: vec![Address::bare(account.address.as_str())],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Catch up next week?".into(),
        snippet: "Do you have an hour next week?".into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: Some(Category::Important),
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef { account: account.id.to_string(), pack: PackId::new(), offset: 0, len: 1 },
        gmail: None,
    };
    vault
        .ingest_mail(
            account.id,
            vec![IngestMessage { message: message.clone(), mailbox: mailbox.id, uid: 1 }],
        )
        .unwrap();
    vault
        .save_body(&Body {
            message_id: message.id,
            html_sanitised: "<p>Do you have an hour next week?</p>".into(),
            text: "Do you have an hour next week?".into(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        })
        .unwrap();

    Env { svc, account, thread, message: message.id, clock, _dir: dir }
}

fn event(calendar: CalendarId, title: &str, start: Timestamp, end: Timestamp) -> Event {
    let local_date = start.in_tz(ZONE).unwrap().date();
    Event {
        id: EventId::new(),
        calendar_id: calendar,
        uid: title.to_string(),
        title: title.to_string(),
        description: String::new(),
        location: String::new(),
        start,
        end,
        local_date,
        end_date: local_date,
        tz: ZONE.to_string(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: String::new(),
        attendees: Vec::new(),
        url: "https://calendar.example.com/event".to_string(),
        busy: true,
        series: None,
        own: Some(true),
        updated_at: start,
    }
}

/// An account calendar with a meeting and a plan on Monday, and a block on
/// this computer on Tuesday.
fn seed_week(e: &Env) -> Calendar {
    let vault = e.svc.get().unwrap();
    let calendar = Calendar::from_account(
        e.account.id,
        Provider::Custom,
        AccountCalendarSource::CalDav,
        "/cal/",
        "Work",
    );
    vault.save_calendar(&calendar).unwrap();

    let mut meeting =
        event(calendar.id, "Q4 review", at("2026-11-09", 14, 0), at("2026-11-09", 15, 0));
    meeting.attendees = vec!["Fox Mulder".into(), "hari@example.com".into()];
    meeting.description = "The dial-in is 555-0100".into();
    let mut plan =
        event(calendar.id, "Write the spec", at("2026-11-09", 10, 0), at("2026-11-09", 12, 0));
    plan.attendees = vec!["hari@example.com".into()];
    vault
        .sync_account_calendar(calendar.id, &[meeting, plan], &[], AccountSyncCursor::default())
        .unwrap();

    let mut block = TimeBlock::new(BlockSubject::Adhoc, at("2026-11-10", 9, 0), 60, ZONE);
    block.title = "Inbox zero".into();
    vault.save_block(&block).unwrap();
    calendar
}

/// Settings → Mail writing with the calendar ticked, which it is not until then.
fn use_calendar(e: &Env) {
    let writing = MailWriting { use_calendar: true, ..Default::default() };
    e.svc.get().unwrap().save_mail_writing(&writing).unwrap();
}

async fn suggest(e: &Env) -> Value {
    e.svc
        .call(Ctx::local(), "suggest_replies", json!({ "id": e.thread }))
        .await
        .expect("suggest_replies")
}

// ---- the calendar ----------------------------------------------------------

#[tokio::test]
async fn meetings_and_plans_reach_the_model_as_different_things() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    seed_week(&e);

    suggest(&e).await;

    let (system, user) = fake.prompt(0);
    assert!(system.contains("\"meetings\" are commitments"), "{system}");
    assert!(system.contains("\"plans\" are notes to themselves"), "{system}");

    assert!(user.contains("calendar, their own and private"), "{user}");
    assert!(
        user.contains(
            "Mon 9 Nov | meetings: 14:00-15:00 Q4 review | plans: 10:00-12:00 Write the spec \
             | open: 09:00-10:00, 12:00-14:00, 15:00-17:00"
        ),
        "{user}"
    );
    assert!(
        user.contains("Tue 10 Nov | plans: 09:00-10:00 Inbox zero | open: 10:00-17:00"),
        "a block on this computer is a plan: {user}"
    );
    assert!(!user.contains("Thu 3 Dec"), "four weeks and no further: {user}");
    assert!(user.contains("Wed 2 Dec"), "{user}");
    assert!(user.contains("America/Los_Angeles"), "{user}");

    // Times and titles, and nothing else off an invitation.
    assert!(!user.contains("555-0100"), "a description stays home: {user}");
    assert!(!user.contains("Fox Mulder"), "a guest list stays home: {user}");
}

#[tokio::test]
async fn a_hidden_calendar_is_not_read() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    let mut calendar = seed_week(&e);
    calendar.visible = false;
    e.svc.get().unwrap().save_calendar(&calendar).unwrap();

    suggest(&e).await;

    let (_, user) = fake.prompt(0);
    assert!(!user.contains("Q4 review"), "{user}");
    assert!(user.contains("Inbox zero"), "this computer has no calendar to hide: {user}");
}

#[tokio::test]
async fn the_calendar_is_not_read_until_it_is_asked_for() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    seed_week(&e);

    suggest(&e).await;

    let (system, user) = fake.prompt(0);
    assert!(!user.contains("Q4 review"), "{user}");
    assert!(!user.contains("calendar"), "{user}");
    assert!(!system.contains("\"meetings\""), "{system}");
}

#[tokio::test]
async fn an_account_without_writing_help_gives_its_times_and_not_its_titles() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    let vault = e.svc.get().unwrap();
    seed_week(&e);

    let mut work = Account::new(Provider::Custom, "work@example.com");
    work.services.mail = true;
    vault.save_account(&work).unwrap();
    let calendar = Calendar::from_account(
        work.id,
        Provider::Custom,
        AccountCalendarSource::CalDav,
        "/work/",
        "Work",
    );
    vault.save_calendar(&calendar).unwrap();
    let mut board =
        event(calendar.id, "Board meeting", at("2026-11-11", 14, 0), at("2026-11-11", 15, 0));
    board.attendees = vec!["Dana".into()];
    vault.sync_account_calendar(calendar.id, &[board], &[], AccountSyncCursor::default()).unwrap();

    suggest(&e).await;

    let (_, user) = fake.prompt(0);
    assert!(!user.contains("Board meeting"), "{user}");
    assert!(
        user.contains("Wed 11 Nov | meetings: 14:00-15:00 (private) | open: 09:00-14:00"),
        "the time is still kept clear: {user}"
    );
    assert!(user.contains("Q4 review"), "the account that agreed keeps its titles: {user}");
}

/// A calendar that is behind would read as free time. Each of these is a way of
/// being behind, and each leaves the model with no calendar, not a wrong one.
#[tokio::test]
async fn a_calendar_that_may_be_behind_means_no_times_at_all() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    let vault = e.svc.get().unwrap();
    let synced = vault.calendar(seed_week(&e).id).unwrap();

    let mut failing = synced.clone();
    failing.mark_failed("the server said no");
    let mut never = synced.clone();
    never.last_synced_at = None;
    let mut asleep = synced.clone();
    asleep.last_synced_at = Some(thursday_morning() - jiff::SignedDuration::from_hours(24 * 7));

    for (case, calendar) in [("failing", failing), ("never read", never), ("behind", asleep)] {
        vault.save_calendar(&calendar).unwrap();
        e.svc.mail_replies_cache_clear();
        let before = fake.asked();
        suggest(&e).await;
        let (system, user) = fake.prompt(before);
        assert!(!user.contains("open:"), "{case}: {user}");
        assert!(!system.contains("\"meetings\""), "{case}: {system}");
    }

    vault.save_calendar(&synced).unwrap();
    e.svc.mail_replies_cache_clear();
    let before = fake.asked();
    suggest(&e).await;
    assert!(fake.prompt(before).1.contains("open:"), "and back again once it is current");
}

/// A feed of holidays says nothing about how anybody's time is spent.
#[tokio::test]
async fn a_lone_holiday_feed_does_not_make_somebody_free() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    let vault = e.svc.get().unwrap();
    let mut feed = Calendar::imported("Holidays", "holidays.ics");
    feed.mark_synced();
    vault.save_calendar(&feed).unwrap();

    suggest(&e).await;

    assert!(!fake.prompt(0).1.contains("open:"), "{}", fake.prompt(0).1);
}

/// With a one-word name, the profile's "first last" used to be "hari " and so
/// nobody on a guest list.
#[tokio::test]
async fn being_on_your_own_event_under_one_name_is_still_a_plan() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    use_calendar(&e);
    let vault = e.svc.get().unwrap();
    let mut profile = vault.profile().unwrap();
    profile.first_name = "Prince".into();
    vault.save_profile(&profile).unwrap();
    let calendar = seed_week(&e);
    let mut solo = event(calendar.id, "Practice", at("2026-11-11", 10, 0), at("2026-11-11", 11, 0));
    solo.attendees = vec!["Prince".into()];
    vault.sync_account_calendar(calendar.id, &[solo], &[], AccountSyncCursor::default()).unwrap();

    suggest(&e).await;

    assert!(
        fake.prompt(0).1.contains("Wed 11 Nov | plans: 10:00-11:00 Practice"),
        "{}",
        fake.prompt(0).1
    );
}

/// Told "open: 09:00-17:00" every day of a month, a model would offer any of
/// them to somebody with a calendar it simply cannot see.
#[tokio::test]
async fn with_no_calendar_at_all_nobody_is_called_free() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);

    suggest(&e).await;

    let (system, user) = fake.prompt(0);
    assert!(!user.contains("open:"), "{user}");
    assert!(!system.contains("\"meetings\""), "{system}");
}

// ---- what they said about how they write -----------------------------------

#[tokio::test]
async fn what_they_said_reaches_the_model_and_changes_what_is_offered_at_once() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);

    suggest(&e).await;
    assert!(!fake.prompt(0).0.contains("Never apologise"));
    let asked = fake.asked();
    suggest(&e).await;
    assert_eq!(fake.asked(), asked, "a thread reopened is answered from the cache");

    let saved = e
        .svc
        .call(
            Ctx::local(),
            "save_mail_writing",
            json!({ "writing": { "instructions": "Never apologise.", "useCalendar": true } }),
        )
        .await
        .unwrap();
    assert_eq!(saved["instructions"], "Never apologise.");

    suggest(&e).await;
    assert!(fake.asked() > asked, "saving it throws away what was written without it");
    let (system, _) = fake.prompt(fake.asked() - 1);
    assert!(system.contains("---\nNever apologise.\n---"), "{system}");
}

#[tokio::test]
async fn too_much_to_say_is_refused() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    let err = e
        .svc
        .call(
            Ctx::local(),
            "save_mail_writing",
            json!({ "writing": { "instructions": "x".repeat(4_001), "useCalendar": false } }),
        )
        .await
        .expect_err("over the limit");
    assert!(err.to_string().contains("too long"), "{err}");
    assert_eq!(e.svc.get().unwrap().mail_writing().unwrap(), MailWriting::default());
}

/// A suggestion that says "Tuesday at 2" must not still be offered on Tuesday
/// at 3, with the thread unchanged all the while.
#[tokio::test]
async fn suggestions_are_not_kept_long_enough_for_their_times_to_pass() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);

    suggest(&e).await;
    let asked = fake.asked();
    e.clock.set(thursday_morning() + jiff::SignedDuration::from_mins(59));
    suggest(&e).await;
    assert_eq!(fake.asked(), asked);

    e.clock.set(thursday_morning() + jiff::SignedDuration::from_mins(61));
    suggest(&e).await;
    assert!(fake.asked() > asked);
}

// ---- the signature ---------------------------------------------------------

fn sign(e: &Env, signature_html: &str) {
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = signature_html.to_string();
    vault.save_account(&account).unwrap();
}

async fn new_draft(e: &Env, args: Value) -> Value {
    let mut args = args;
    args["account"] = json!(e.account.id);
    e.svc.call(Ctx::local(), "new_draft", args).await.expect("new_draft")
}

const SIGNED: &str = r#"<div data-signature=""><p>Hari G</p><p>ViyaMD</p></div>"#;

#[tokio::test]
async fn a_new_message_starts_with_the_accounts_signature() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Hari G</p><p>ViyaMD</p>");

    let draft = new_draft(&e, json!({})).await;

    assert_eq!(draft["bodyHtml"], format!("<p></p>{SIGNED}"));
}

#[tokio::test]
async fn a_reply_and_a_forward_carry_it_above_the_quote() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Hari G</p><p>ViyaMD</p>");

    for args in [json!({ "inReplyTo": e.message }), json!({ "forwardOf": e.message })] {
        let draft = new_draft(&e, args).await;
        let body = draft["bodyHtml"].as_str().unwrap();
        assert!(body.starts_with(&format!("<p></p>{SIGNED}")), "{body}");
        let (own, quote) = body.split_at(body.find("<blockquote").unwrap());
        assert!(own.contains("Hari G"), "{body}");
        assert!(quote.contains("Do you have an hour next week?"), "{body}");
    }
}

#[tokio::test]
async fn an_account_with_no_signature_starts_as_it_always_has() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    assert_eq!(new_draft(&e, json!({})).await["bodyHtml"], "");
    let reply = new_draft(&e, json!({ "inReplyTo": e.message })).await;
    assert!(reply["bodyHtml"].as_str().unwrap().starts_with("<p>On "), "{reply}");
}

#[tokio::test]
async fn deleting_it_from_a_draft_leaves_that_draft_without_one() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Hari G</p>");

    let mut draft = new_draft(&e, json!({})).await;
    draft["bodyHtml"] = json!("<p>No signature on this one.</p>");
    e.svc.call(Ctx::local(), "save_draft", json!({ "draft": draft.clone() })).await.unwrap();

    let id: everyday_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    let stored = e.svc.get().unwrap().draft(id).unwrap();
    assert_eq!(stored.body_html, "<p>No signature on this one.</p>");
}

/// What compose does when the person picks a second address under From, down
/// to the address the message then goes out from.
#[tokio::test]
async fn a_draft_saved_from_a_second_address_stays_from_it() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = "<p>The account's</p>".into();
    account.identities = vec![everyday_core::account::Identity {
        name: "Hari".into(),
        address: "alias@example.com".into(),
        signature_html: "<p>The alias's</p>".into(),
    }];
    vault.save_account(&account).unwrap();

    let mut draft = new_draft(&e, json!({})).await;
    assert!(draft["bodyHtml"].as_str().unwrap().contains("The account's"), "{draft}");
    draft["identity"] = json!("alias@example.com");
    e.svc.call(Ctx::local(), "save_draft", json!({ "draft": draft.clone() })).await.unwrap();
    e.svc.call(Ctx::local(), "save_draft", json!({ "draft": draft.clone() })).await.unwrap();

    let id: everyday_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(vault.draft(id).unwrap().identity, "alias@example.com");
}

#[tokio::test]
async fn write_with_ai_is_told_the_signature_of_the_address_it_is_writing_from() {
    let fake = fake_model(json!({ "body": "Sure." })).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = "<p>The account's</p>".into();
    account.identities = vec![everyday_core::account::Identity {
        name: "Hari".into(),
        address: "alias@example.com".into(),
        signature_html: "<p>The alias's</p>".into(),
    }];
    vault.save_account(&account).unwrap();

    e.svc
        .call(
            Ctx::local(),
            "draft_with_ai",
            json!({ "account": e.account.id, "instruction": "say yes", "identity": "alias@example.com" }),
        )
        .await
        .unwrap();

    let (_, user) = fake.prompt(0);
    assert!(user.contains("The alias's"), "{user}");
    assert!(!user.contains("The account's"), "{user}");
    assert!(user.contains("written from alias@example.com"), "{user}");
}

#[tokio::test]
async fn saving_a_signature_is_checked_and_forgets_what_was_measured_with_the_old_one() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let save = |account: &Account| {
        let args = json!({ "account": account });
        let svc = e.svc.clone();
        async move { svc.call(Ctx::local(), "save_account", args).await }
    };
    let measured = || e.svc.mail_voice_cached(e.account.id, jiff::SignedDuration::from_hours(6));

    suggest(&e).await;
    assert!(measured().is_some());

    let mut account = vault.account(e.account.id).unwrap();
    account.display_name = "Something else".into();
    save(&account).await.unwrap();
    assert!(measured().is_some(), "nothing about the signature changed");

    account.signature_html = "<p>Hari G</p>".into();
    save(&account).await.unwrap();
    assert!(measured().is_none(), "it may have the signature in as their sign-off");

    account.signature_html = "x".repeat(everyday_core::account::MAX_SIGNATURE_BYTES + 1);
    let err = save(&account).await.expect_err("over the limit");
    assert!(err.to_string().contains("too long"), "{err}");
}

/// Sync marks an account's status through this and does not look at the answer,
/// so a signature from before there was a limit must not stop it.
#[tokio::test]
async fn a_signature_over_the_limit_does_not_stop_the_account_being_written() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = "x".repeat(everyday_core::account::MAX_SIGNATURE_BYTES + 1);
    vault.save_account(&account).unwrap();

    // Nor does it stop the person flipping a switch on the same account.
    account.mail_ai.summaries = true;
    e.svc.call(Ctx::local(), "save_account", json!({ "account": account })).await.unwrap();
}

#[tokio::test]
async fn a_reply_that_was_only_what_the_signature_says_is_no_reply() {
    let fake = fake_model(json!({ "replies": [
        { "label": "Just thanks", "body": "Thanks!" },
        { "label": "A real one", "body": "Tuesday works." },
    ]}))
    .await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Thanks,<br>Hari</p>");

    let answer = suggest(&e).await;

    let replies = answer["suggestions"].as_array().unwrap();
    assert_eq!(replies.len(), 1, "{answer}");
    assert_eq!(replies[0]["label"], "A real one");
}

/// The compose sheet puts what comes back in place of what is there, so nothing
/// must come back as an error and not as an empty answer.
#[tokio::test]
async fn writing_that_comes_to_nothing_is_refused_rather_than_replacing_their_words() {
    let fake = fake_model(json!({ "body": "Thanks!" })).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Thanks,<br>Hari</p>");

    let err = e
        .svc
        .call(
            Ctx::local(),
            "draft_with_ai",
            json!({ "account": e.account.id, "instruction": "say thanks", "currentText": "My words" }),
        )
        .await
        .expect_err("nothing to put there");
    assert!(err.to_string().contains("did not write anything"), "{err}");
}

/// Polishing their own text: if they typed the closing, it stays.
#[tokio::test]
async fn a_rewrite_keeps_a_closing_they_typed_themselves() {
    let fake = fake_model(json!({ "body": "Thursday works.\n\nHari" })).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Hari Govardhanam</p>");

    let written = e
        .svc
        .call(
            Ctx::local(),
            "improve_writing",
            json!({ "account": e.account.id, "text": "thursday works\n\nHari", "mode": "polish" }),
        )
        .await
        .unwrap();
    assert_eq!(written["bodyText"], "Thursday works.\n\nHari");
}

#[tokio::test]
async fn the_model_is_told_about_the_signature_and_a_repeated_name_is_taken_out() {
    let fake = fake_model(three_replies()).await;
    let e = env(&fake.endpoint);
    sign(&e, "<p>Hari Govardhanam</p><p>ViyaMD</p>");

    let answer = suggest(&e).await;

    let (_, user) = fake.prompt(0);
    assert!(user.contains("Their signature goes under what you write"), "{user}");
    assert!(user.contains("Hari Govardhanam"), "{user}");
    assert!(!user.contains("They sign as"), "{user}");

    let replies = answer["suggestions"].as_array().unwrap();
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["bodyText"], "Tuesday at 2 works.\n\nThanks,", "{answer}");
}

#[tokio::test]
async fn an_auto_draft_with_nothing_left_in_it_is_not_saved() {
    let fake = fake_model(json!({ "reply": true, "body_text": "Thanks!" })).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = "<p>Thanks,<br>Hari</p>".into();
    account.mail_ai = MailAi { auto_draft: true, ..Default::default() };
    vault.save_account(&account).unwrap();

    everyday_service::mailai::auto_draft_tick(&e.svc).await;

    assert!(fake.asked() > 0, "the model was asked");
    assert_eq!(vault.drafts(e.account.id).unwrap().len(), 0);
}

#[tokio::test]
async fn an_auto_draft_is_signed_and_follows_what_they_said() {
    let fake = fake_model(json!({ "reply": true, "body_text": "Sure." })).await;
    let e = env(&fake.endpoint);
    let vault = e.svc.get().unwrap();
    let mut account = vault.account(e.account.id).unwrap();
    account.signature_html = "<p>Hari G</p>".into();
    account.mail_ai = MailAi { auto_draft: true, ..Default::default() };
    vault.save_account(&account).unwrap();
    vault
        .save_mail_writing(&MailWriting { instructions: "Be brief.".into(), use_calendar: true })
        .unwrap();
    seed_week(&e);

    everyday_service::mailai::auto_draft_tick(&e.svc).await;

    let drafts = vault.drafts(e.account.id).unwrap();
    assert_eq!(drafts.len(), 1);
    let body = &drafts[0].body_html;
    assert!(
        body.starts_with(r#"<p>Sure.</p><div data-signature=""><p>Hari G</p></div><p>On "#),
        "{body}"
    );
    let (system, user) = fake.prompt(0);
    assert!(system.contains("Be brief."), "{system}");
    // A draft can sit for days before anybody reads it, and a time in it would
    // have gone stale by then.
    assert!(!user.contains("Q4 review"), "{user}");
}
