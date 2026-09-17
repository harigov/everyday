use everyday_core::mail::Category;

use super::fixtures::*;
// ---------------------------------------------------------------------
// Categorisation at ingest -- docs/plans/mail.md's phase 7, rules half
// ---------------------------------------------------------------------

fn raw_newsletter(message_id: &str, from: &str, subject: &str, date: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{message_id}>\r\n\
         From: {from}\r\n\
         To: me@example.com\r\n\
         Subject: {subject}\r\n\
         Date: {date}\r\n\
         List-Id: Example Weekly <weekly.example.com>\r\n\
         List-Unsubscribe: <https://example.com/unsub>\r\n\
         Content-Type: text/plain\r\n\r\n\
         Hello.\r\n"
    )
    .into_bytes()
}

/// Categorisation is always on: a fresh message lands with a category the
/// moment it is ingested, with no `mail_ai` switch anywhere near it -- the
/// rules-based half sends nothing anywhere and needs no consent.
#[tokio::test]
async fn categorisation_runs_at_ingest_with_no_switch_to_turn_on() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let news = raw_newsletter(
            "news-1@example.com",
            "weekly@example.com",
            "This week",
            "01 Jan 2024 09:00:00 +0000",
        );
        s.append("INBOX", news, flags_seen(), None);
        let plain = raw_message(
            "plain-1@example.com",
            None,
            "dana@example.com",
            "Hi",
            "01 Jan 2024 09:00:00 +0000",
            "hello",
        );
        s.append("INBOX", plain, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let news = env
        .vault
        .message_by_message_id_header(env.account_id, "news-1@example.com")
        .unwrap()
        .expect("newsletter stored");
    assert_eq!(news.category, Some(Category::Newsletter));
    let (news_thread, _) = env.vault.thread(news.thread_id).unwrap();
    assert_eq!(
        news_thread.category,
        Some(Category::Newsletter),
        "the thread mirrors its newest message's category"
    );

    let plain = env
        .vault
        .message_by_message_id_header(env.account_id, "plain-1@example.com")
        .unwrap()
        .expect("plain message stored");
    assert_eq!(
        plain.category,
        Some(Category::Other),
        "a first message from a stranger with no signals at all is Other"
    );
}

/// `set_thread_category`'s underlying mechanism, `Vault::correct_mail_category`:
/// records a correction, applies it to that sender's existing mail, and a
/// later message from the same sender picks it up at ingest with no second
/// correction needed.
#[tokio::test]
async fn a_correction_outranks_the_rules_and_reaches_existing_and_future_mail() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "news-2@example.com",
            None,
            "weekly@example.com",
            "This week",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let before = env
        .vault
        .message_by_message_id_header(env.account_id, "news-2@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(before.category, Some(Category::Other));

    let changed = env
        .vault
        .correct_mail_category(env.account_id, "weekly@example.com", Category::Important)
        .unwrap();
    assert_eq!(changed, 1, "the one existing message from this sender");
    let after = env
        .vault
        .message_by_message_id_header(env.account_id, "news-2@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(after.category, Some(Category::Important));

    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "news-3@example.com",
            None,
            "weekly@example.com",
            "Next week",
            "02 Jan 2024 09:00:00 +0000",
            "hi again",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    env.sync(&mut session).await;
    let fresh = env
        .vault
        .message_by_message_id_header(env.account_id, "news-3@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(
        fresh.category,
        Some(Category::Important),
        "a fresh message from a corrected sender is categorised by the correction at ingest"
    );
}

/// `recategorize_mail`'s one-off backfill: a message stuck with a stale
/// category (as it would be, ingested before this build knew a rule that
/// now applies) is put right by asking for it again, with no correction
/// involved at all.
///
/// The rule is saved directly, through `save_category_rules` alone rather
/// than `correct_mail_category`'s own sweep, so the message's category is
/// genuinely stale -- still whatever the rules said at ingest -- until the
/// backfill below asks again. `set_mail_message_category` is deliberately
/// not how this test creates staleness any more: that call writes through
/// as a model's own answer (see `everyday_core::mail::CategorySource`), and
/// a model's answer is exactly what `recategorize_mail_never_overrides_a_models_own_answer`,
/// just below, checks the backfill must never touch.
#[tokio::test]
async fn recategorize_mail_backfills_a_stale_category() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "old-1@example.com",
            None,
            "noreply@service.example.com",
            "Receipt",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let msg = env
        .vault
        .message_by_message_id_header(env.account_id, "old-1@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(msg.category, Some(Category::Notification), "the rules already got this right");

    let mut rules = env.vault.category_rules(env.account_id).unwrap();
    rules.set_sender("noreply@service.example.com", Category::Important);
    env.vault.save_category_rules(env.account_id, &rules).unwrap();

    let changed = env.vault.recategorize_mail(env.account_id).unwrap();
    assert_eq!(changed, 1);
    let fixed = env.vault.mail_message(msg.id).unwrap();
    assert_eq!(fixed.category, Some(Category::Important));
}

/// Regression: `recategorize_mail`'s backfill must never override a
/// model's own answer -- see `everyday_core::mail::CategorySource`'s own
/// docs on the ranking that makes this true. Before the fix, a category
/// set through `set_mail_message_category` left no trace of where it came
/// from, so the very next backfill blindly reasserted whatever the rules
/// engine said instead, discarding the model's answer.
#[tokio::test]
async fn recategorize_mail_never_overrides_a_models_own_answer() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "old-2@example.com",
            None,
            "noreply@service.example.com",
            "Receipt",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let msg = env
        .vault
        .message_by_message_id_header(env.account_id, "old-2@example.com")
        .unwrap()
        .unwrap();
    env.vault.set_mail_message_category(msg.id, Category::Other).unwrap();

    let changed = env.vault.recategorize_mail(env.account_id).unwrap();
    assert_eq!(changed, 0, "a model's own answer is never the backfill's to touch");
    let after = env.vault.mail_message(msg.id).unwrap();
    assert_eq!(after.category, Some(Category::Other));
}
