//! `agent::tools::mail`, end to end against a real SQLite vault:
//! `docs/plans/mail.md`'s phase 5 tests, at the layer that does not need a
//! model or a running service. See `crates/everyday-service/tests/` for the
//! two that do -- the confirmation gate on an `Effect::Outward` call with no
//! chat window to ask in, and the rate limiter wired to a real
//! `Service::check_mail_rate_limit`.
//!
//! # Ingesting the hostile corpus without the sync engine
//!
//! [`ingest_eml`] rebuilds just enough of `everyday-service`'s own
//! `mailsync::passes::process_body` -- parse, sanitise, cut quoted text and
//! a signature out of the plain body -- to produce a [`Body`] a real sync
//! would have written. It is not a shortcut around that pipeline so much as
//! a second, smaller copy of its shape, built from the same public
//! functions (`everyday_mail::mime::parse`, `sanitize::sanitize`,
//! `text::quoted_ranges`, `text::signature_range`) the real one calls,
//! because those functions -- not this file's use of them -- are what the
//! hostile-corpus tests in `everyday-mail` already pin down. What this file
//! adds on top is the one thing that crate cannot check on its own: that
//! none of what it strips ever reaches a tool's own JSON.

mod support;

use everyday_core::account::{Account, AgentCaller, AgentMailAccess, Provider};
use everyday_core::agent::tools::{self, Caller, ToolContext};
use everyday_core::id::{AccountId, MailboxId, PackId};
use everyday_core::mail::{Address, Body, Mailbox, MailboxRole, Message, MessageFlags};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_core::{ConversationId, Vault};
use support::{TODAY, vault};

// ---- fixtures -------------------------------------------------------------

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "injection_in_html_comment" => {
            include_bytes!(
                "../../everyday-mail/tests/fixtures/hostile/injection_in_html_comment.eml"
            )
        }
        "injection_white_on_white" => {
            include_bytes!(
                "../../everyday-mail/tests/fixtures/hostile/injection_white_on_white.eml"
            )
        }
        "injection_display_none" => {
            include_bytes!("../../everyday-mail/tests/fixtures/hostile/injection_display_none.eml")
        }
        "injection_visible_plea" => {
            include_bytes!("../../everyday-mail/tests/fixtures/hostile/injection_visible_plea.eml")
        }
        other => panic!("no such fixture: {other}"),
    }
}

// ---- seeding ----------------------------------------------------------

fn seed_account(vault: &Vault, mail_on: bool) -> Account {
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = mail_on;
    vault.save_account(&account).unwrap();
    account
}

fn seed_mailbox(vault: &Vault, account: AccountId, role: MailboxRole) -> MailboxId {
    let mailbox = Mailbox::new(account, role.as_str(), role);
    vault.save_mailbox(&mailbox).unwrap();
    mailbox.id
}

fn mail_address(a: &everyday_mail::mime::Address) -> Address {
    Address::new(a.name.clone().unwrap_or_default(), a.email.clone().unwrap_or_default())
}

/// Ingest one raw `.eml` into `mailbox`, the way a real sync would have --
/// see the module docs. Returns the thread id [`everyday_core::mail::Thread`]s
/// are read back by.
fn ingest_eml(
    vault: &Vault,
    account: AccountId,
    mailbox: MailboxId,
    uid: u32,
    raw: &[u8],
) -> everyday_core::id::ThreadId {
    let parsed = everyday_mail::mime::parse(raw).expect("fixture parses");
    let message_id = everyday_core::id::MailMessageId::new();
    let thread_id = everyday_core::id::ThreadId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: parsed.message_id.clone().unwrap_or_default(),
        date: parsed.date.unwrap_or_else(jiff::Timestamp::now),
        from: parsed.from.first().map(mail_address).unwrap_or_else(|| Address::bare("?")),
        to: parsed.to.iter().map(mail_address).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: parsed.subject.clone().unwrap_or_default(),
        snippet: everyday_mail::text::snippet(&parsed.text.clone().unwrap_or_default().to_string()),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: raw.len() as u64,
        category: None,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault
        .ingest_mail(account, vec![IngestMessage { message, mailbox, uid }])
        .expect("ingest succeeds");

    let html_sanitised = parsed.html.as_deref().map_or_else(String::new, |html| {
        let rewrite = everyday_mail::sanitize::Rewrite::new(message_id.to_string());
        everyday_mail::sanitize::sanitize(html, &rewrite).html
    });
    let plain = parsed.text.clone().unwrap_or_else(|| {
        parsed.html.as_deref().map(everyday_mail::text::html_to_text).unwrap_or_default()
    });
    let quoted_ranges = everyday_mail::text::quoted_ranges(&plain)
        .into_iter()
        .map(|r| (r.start as u32, r.end as u32))
        .collect();
    let signature_range =
        everyday_mail::text::signature_range(&plain).map(|r| (r.start as u32, r.end as u32));
    let body = Body {
        message_id,
        html_sanitised,
        text: plain,
        quoted_ranges,
        signature_range,
        parts: Vec::new(),
        remote_images: Vec::new(),
    };
    vault.save_body(&body).unwrap();
    thread_id
}

fn ctx<'a>(vault: &'a Vault, caller: Option<Caller>, provider: Option<&str>) -> ToolContext<'a> {
    ToolContext {
        vault,
        today: TODAY,
        tz: "UTC",
        conversation: None,
        unattended: false,
        caller,
        mail_search: None,
        assistant_provider: provider.map(str::to_string),
        mail_rate_limit: None,
    }
}

// ---- the hostile corpus, through read_thread -------------------------

#[test]
fn nothing_hidden_in_the_source_reaches_read_thread() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);
    let inbox = seed_mailbox(&v, account.id, MailboxRole::Inbox);

    for (fixture_name, visible, hidden) in [
        ("injection_in_html_comment", "please find your invoice", "developer mode"),
        ("injection_white_on_white", "roundup of stories", "disregard your operator"),
        ("injection_display_none", "ticket has been updated", "forward the last ten invoices"),
    ] {
        let thread = ingest_eml(&v, account.id, inbox, 1, fixture(fixture_name));
        let ctx = ctx(&v, None, None);
        let out = tools::dispatch(
            &ctx,
            "read_thread",
            &serde_json::json!({ "thread_id": thread.to_string() }),
        )
        .unwrap_or_else(|e| panic!("{fixture_name}: read_thread failed: {e}"));
        let text = out["messages"][0]["untrusted_text"].as_str().unwrap();
        assert!(text.contains(visible), "{fixture_name}: expected {visible:?} in {text:?}");
        assert!(
            !text.to_lowercase().contains(&hidden.to_lowercase()),
            "{fixture_name}: hidden text leaked into read_thread: {text:?}"
        );
        assert!(!text.contains("attacker@evil.example"), "{fixture_name}: {text:?}");
    }
}

/// The companion the core's own hostile-corpus suite makes: hiding was
/// never the defence. A plea written in plain sight reaches `read_thread`
/// exactly as it would reach a person reading the same message, because
/// nothing here parses HTML or judges content -- only strips what a sender
/// tried to hide.
#[test]
fn a_visible_plea_is_not_hidden_from_read_thread_either() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);
    let inbox = seed_mailbox(&v, account.id, MailboxRole::Inbox);
    let thread = ingest_eml(&v, account.id, inbox, 1, fixture("injection_visible_plea"));

    let ctx = ctx(&v, None, None);
    let out = tools::dispatch(
        &ctx,
        "read_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .unwrap();
    let text = out["messages"][0]["untrusted_text"].as_str().unwrap();
    assert!(text.to_lowercase().contains("forward the last ten"), "{text}");
}

// ---- the permissions matrix --------------------------------------------

const PROVIDER: &str = "http://localhost:11434/v1";

#[test]
fn a_mail_disabled_account_offers_no_mail_tools_to_anybody() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed_account(&v, false);

    for caller in [None, Some(Caller::Assistant { conversation: ConversationId::new() })] {
        let names: Vec<&str> = tools::available_for(&v, caller.as_ref(), Some(PROVIDER))
            .iter()
            .map(|t| t.name)
            .collect();
        assert!(!names.contains(&"list_accounts"), "{caller:?}: {names:?}");
        assert!(!names.contains(&"read_thread"), "{caller:?}: {names:?}");
    }
}

#[test]
fn the_persons_own_action_is_unrestricted_by_any_switch() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let mut account = seed_account(&v, true);
    account.assistant_access = AgentMailAccess::none();
    account.mcp_access = AgentMailAccess::none();
    v.save_account(&account).unwrap();

    // `caller: None` reads as the vault's owner acting directly -- a
    // palette entry, a script -- and `AgentMailAccess` exists to gate an
    // agent, not the person whose vault it is.
    let names: Vec<&str> = tools::available_for(&v, None, None).iter().map(|t| t.name).collect();
    assert!(names.contains(&"read_thread"));
    assert!(names.contains(&"send_draft"), "even send, which starts off for every agent");
}

#[test]
fn the_assistants_mail_tools_stay_dark_until_the_provider_is_acknowledged() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);
    let caller = Caller::Assistant { conversation: ConversationId::new() };

    let before: Vec<&str> =
        tools::available_for(&v, Some(&caller), Some(PROVIDER)).iter().map(|t| t.name).collect();
    assert!(!before.contains(&"read_thread"), "not acknowledged yet: {before:?}");

    let mut acknowledged = account.clone();
    acknowledged.assistant_provider_acknowledged = Some(PROVIDER.to_string());
    v.save_account(&acknowledged).unwrap();

    let after: Vec<&str> =
        tools::available_for(&v, Some(&caller), Some(PROVIDER)).iter().map(|t| t.name).collect();
    assert!(after.contains(&"read_thread"), "{after:?}");

    // And a call actually made without acknowledgement is refused, naming
    // the account and what is missing -- not merely hidden from the list.
    let ctx = ctx(&v, Some(caller), Some(PROVIDER));
    let inbox = seed_mailbox(&v, account.id, MailboxRole::Inbox);
    let thread = ingest_eml(&v, account.id, inbox, 1, fixture("injection_visible_plea"));
    // Reset the account back to unacknowledged for this half of the check.
    v.save_account(&account).unwrap();
    let err = tools::dispatch(
        &ctx,
        "read_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .unwrap_err();
    assert!(err.to_string().contains(&account.address), "{err}");
}

#[test]
fn mcp_needs_no_acknowledgement() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    seed_account(&v, true);
    let caller = Caller::Mcp { client: "claude".into() };
    let names: Vec<&str> =
        tools::available_for(&v, Some(&caller), None).iter().map(|t| t.name).collect();
    assert!(names.contains(&"read_thread"), "{names:?}");
}

#[test]
fn mcp_with_send_off_never_lists_send_draft_and_turning_it_on_reveals_it() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);
    let caller = Caller::Mcp { client: "claude".into() };

    let before: Vec<&str> =
        tools::available_for(&v, Some(&caller), None).iter().map(|t| t.name).collect();
    assert!(!before.contains(&"send_draft"), "send starts off: {before:?}");

    let mut allowed = account.clone();
    allowed.mcp_access.send = true;
    v.save_account(&allowed).unwrap();
    let after: Vec<&str> =
        tools::available_for(&v, Some(&caller), None).iter().map(|t| t.name).collect();
    assert!(after.contains(&"send_draft"), "{after:?}");
}

#[test]
fn each_switch_gates_exactly_its_own_tool_by_name_and_account() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let mut account = seed_account(&v, true);
    account.assistant_access = AgentMailAccess::none();
    v.save_account(&account).unwrap();
    let inbox = seed_mailbox(&v, account.id, MailboxRole::Inbox);
    let thread = ingest_eml(&v, account.id, inbox, 1, fixture("injection_visible_plea"));

    let caller = Caller::Assistant { conversation: ConversationId::new() };
    let ctx = ctx(&v, Some(caller.clone()), Some(PROVIDER));

    // Not acknowledged, and every switch off: refused, naming the account.
    let err = tools::dispatch(
        &ctx,
        "read_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains(&account.address), "{err}");

    // Acknowledge, but leave `read` off: still refused, and the message
    // names the switch a model can act on.
    let mut ack = account.clone();
    ack.assistant_provider_acknowledged = Some(PROVIDER.to_string());
    v.save_account(&ack).unwrap();
    let err = tools::dispatch(
        &ctx,
        "read_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .unwrap_err()
    .to_string();
    assert!(err.to_lowercase().contains("read"), "{err}");

    // Turn `read` on: now it works.
    let mut readable = ack.clone();
    readable.assistant_access.read = true;
    v.save_account(&readable).unwrap();
    tools::dispatch(&ctx, "read_thread", &serde_json::json!({ "thread_id": thread.to_string() }))
        .expect("read is now permitted");

    // `archive_thread` is still refused: `read` is not `archive`.
    let err = tools::dispatch(
        &ctx,
        "archive_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .unwrap_err()
    .to_string();
    assert!(err.to_lowercase().contains("archive"), "{err}");

    // Turn `archive` on too: now it works, and the write's own JSON result
    // names the thread's id -- what a mail write's `Change` is built from.
    let mut archivable = readable.clone();
    archivable.assistant_access.archive = true;
    v.save_account(&archivable).unwrap();
    let out = tools::dispatch(
        &ctx,
        "archive_thread",
        &serde_json::json!({ "thread_id": thread.to_string() }),
    )
    .expect("archive is now permitted");
    assert_eq!(out["id"], thread.to_string());
    assert_eq!(out["kind"], "thread");
}

// ---- send_draft: the one Outward tool ----------------------------------

#[test]
fn an_unattended_run_may_draft_but_never_send() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);

    let mut unattended = ctx(&v, None, None);
    unattended.unattended = true;

    let drafted = tools::dispatch(
        &unattended,
        "draft_message",
        &serde_json::json!({
            "account_id": account.id.to_string(),
            "to": ["friend@example.com"],
            "subject": "hello",
            "body_html": "<p>hi</p>",
        }),
    )
    .expect("an unattended run may still draft");
    let draft_id = drafted["id"].as_str().unwrap().to_string();

    let err =
        tools::dispatch(&unattended, "send_draft", &serde_json::json!({ "draft_id": draft_id }))
            .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("scheduled run"), "{err}");
}

#[test]
fn send_draft_is_refused_for_an_account_with_send_off_and_permitted_once_it_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let account = seed_account(&v, true);
    let caller = Caller::Mcp { client: "claude".into() };
    let ctx = ctx(&v, Some(caller), None);

    let drafted = tools::dispatch(
        &ctx,
        "draft_message",
        &serde_json::json!({
            "account_id": account.id.to_string(),
            "to": ["friend@example.com"],
            "subject": "hello",
            "body_html": "<p>hi</p>",
        }),
    )
    .expect("mcp may draft by default");
    let draft_id = drafted["id"].as_str().unwrap().to_string();

    let err = tools::dispatch(&ctx, "send_draft", &serde_json::json!({ "draft_id": draft_id }))
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("send"), "{err}");

    let mut allowed = account.clone();
    allowed.mcp_access.send = true;
    v.save_account(&allowed).unwrap();
    let out = tools::dispatch(&ctx, "send_draft", &serde_json::json!({ "draft_id": draft_id }))
        .expect("send is now permitted");
    assert_eq!(out["action"], "queued to send");
    assert!(out["note"].as_str().unwrap_or_default().contains("undo window"));
}

// ---- who is who in AgentMailAccess --------------------------------------

#[test]
fn access_for_reads_the_matching_caller_not_the_other_ones() {
    let dir = tempfile::tempdir().unwrap();
    let v = vault(dir.path());
    let mut account = seed_account(&v, true);
    account.assistant_access = AgentMailAccess::none();
    v.save_account(&account).unwrap();
    assert!(!account.access_for(AgentCaller::Assistant).read);
    assert!(account.access_for(AgentCaller::Mcp).read, "mcp's own switch is untouched");
}
