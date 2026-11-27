//! The records and the vault the three passes' tests are built from.

use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::account::{Account, Provider};
use jatayu_core::id::{AccountId, MailMessageId, ThreadId};
use jatayu_core::mail::{Address, CategorySource, Message, MessageFlags, Thread};
use jatayu_core::packstore::PackRef;
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::Today;

pub(super) fn account() -> Account {
    Account::new(Provider::Custom, "me@example.com")
}

pub(super) fn message(from: &str, to: &[&str], subject: &str, snippet: &str) -> Message {
    Message {
        id: MailMessageId::new(),
        account_id: AccountId::new(),
        thread_id: ThreadId::new(),
        message_id_header: "x@example.com".into(),
        date: jiff::Timestamp::now(),
        from: Address::bare(from),
        to: to.iter().map(|a| Address::bare(*a)).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: snippet.into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 0,
        category: None,
        category_source: CategorySource::Rules,
        invite: None,
        pack: PackRef {
            account: "a".into(),
            pack: jatayu_core::id::PackId::new(),
            offset: 0,
            len: 1,
        },
        gmail: None,
        unsubscribe: None,
    }
}

pub(super) fn thread(account_id: AccountId) -> Thread {
    Thread {
        id: ThreadId::new(),
        account_id,
        subject: "subject".into(),
        participants: Vec::new(),
        last_date: jiff::Timestamp::now(),
        message_count: 1,
        unread_count: 0,
        category: None,
        snoozed_until: None,
        snippet: String::new(),
        starred: false,
        has_attachments: false,
        ai_categorize_asked_at_count: None,
        ai_auto_draft_asked_at_count: None,
        ai_priority_asked_at_count: None,
        sorted_as: None,
        ai_sorted: None,
        follow_up: None,
        follow_up_asked: None,
    }
}

/// 24 November 2026, noon UTC: what every date in these tests is reckoned from.
pub(super) fn today() -> Today {
    Today::new(TimeZone::UTC, "2026-11-24T12:00:00Z".parse().unwrap())
}

pub(super) fn at(date: &str) -> Timestamp {
    format!("{date}T09:00:00Z").parse().unwrap()
}

pub(super) fn env() -> (Arc<Vault>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(
        jatayu_vault::create(
            dir.path(),
            jatayu_core::VaultConfig { password: None, ..Default::default() },
        )
        .unwrap(),
    );
    (vault, dir)
}

pub(super) fn listed(n: usize) -> Vec<Thread> {
    (0..n).map(|_| thread(AccountId::new())).collect()
}
