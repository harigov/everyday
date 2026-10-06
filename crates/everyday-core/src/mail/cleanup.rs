//! Quick Cleanup's arithmetic: who is filling the Inbox, counted.
//!
//! One pure function, [`inbox_senders`], over messages somebody else has
//! already read -- the same split [`crate::insights`] makes, for the same
//! reason: who a message is from is sealed, so the count is a walk over
//! decrypted records, and the rules of that walk (who is you, what a draft
//! is, which messages are still in the Inbox at all) are tested here without
//! a store.
//!
//! # Why only what is still in the Inbox
//!
//! The list exists to be acted on: pick a sender, archive or trash what they
//! sent. The action works on exactly the threads a row names, so the row
//! must count exactly what the action would move -- a sender whose mail was
//! already archived is not cluttering anything, and listing them would
//! offer to clean up nothing. Counting only threads still in the Inbox is
//! also what makes a cleaned-up sender drop off the list on the next read,
//! rather than lingering with a count that no longer means anything.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use serde::Serialize;

use crate::id::{AccountId, ThreadId};
use crate::insights::Me;
use crate::mail::Message;

/// One sender, as Quick Cleanup lists them: how much of what is in the Inbox
/// is theirs, and the threads an archive or a trash of "everything from
/// them" would move.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSender {
    /// Lower-cased and trimmed -- what every message of theirs was grouped
    /// by, so `News@Example.com` and `news@example.com ` are one sender.
    pub email: String,
    /// The display name on their newest message that carried one, or the
    /// address itself when none did -- never empty, so a row always has
    /// something to say.
    pub name: String,
    /// Messages from them, in the window, in threads still in the Inbox.
    pub messages: u32,
    /// How many of `messages` are unread.
    pub unread: u32,
    /// Every thread those messages are in, each once, newest first: the
    /// threads the cleanup action moves.
    pub threads: Vec<ThreadId>,
    /// Every account those messages arrived in, each once, newest first --
    /// a sender who writes to two of your addresses is one row, not two.
    pub accounts: Vec<AccountId>,
    /// Their newest counted message.
    pub latest: Timestamp,
}

/// Who sent the most of what is still in the Inbox, since `since`.
///
/// Counts every message in `messages` that is dated at or after `since`, is
/// not a draft, is not from one of `me`'s own addresses, and sits in a
/// thread named in `in_inbox`; groups them by sender address, compared
/// without case or surrounding space (a message with no sender address at
/// all is skipped -- there is nobody to list it under). The busiest sender
/// comes first, then the one with more unread, then by address so the order
/// is stable; at most `limit` are returned.
///
/// `messages` may come from several accounts and in any order -- see
/// [`InboxSender`] for what each field is drawn from.
pub fn inbox_senders(
    messages: &[Message],
    in_inbox: &HashSet<ThreadId>,
    me: &Me,
    since: Timestamp,
    limit: usize,
) -> Vec<InboxSender> {
    // Newest first, so the first message seen from a sender is their newest
    // -- which is what `latest`, the newest-first `threads` and `accounts`,
    // and (when it has one) `name` all want -- with the id as a tiebreak so
    // two messages in the same second always walk in the same order.
    let mut counted: Vec<&Message> = messages
        .iter()
        .filter(|m| {
            m.date >= since
                && !m.flags.draft
                && in_inbox.contains(&m.thread_id)
                && !me.has_address(&m.from.email)
        })
        .collect();
    counted.sort_by(|a, b| b.date.cmp(&a.date).then(a.id.cmp(&b.id)));

    struct Tally {
        sender: InboxSender,
        named: bool,
        threads: HashSet<ThreadId>,
    }
    let mut by_email: HashMap<String, Tally> = HashMap::new();
    for message in counted {
        let email = message.from.email.trim().to_lowercase();
        if email.is_empty() {
            continue;
        }
        let tally = by_email.entry(email.clone()).or_insert_with(|| Tally {
            sender: InboxSender {
                name: email.clone(),
                email,
                messages: 0,
                unread: 0,
                threads: Vec::new(),
                accounts: Vec::new(),
                latest: message.date,
            },
            named: false,
            threads: HashSet::new(),
        });
        let sender = &mut tally.sender;
        sender.messages += 1;
        sender.unread += u32::from(message.flags.unread());
        let name = message.from.name.trim();
        if !tally.named && !name.is_empty() {
            sender.name = name.to_string();
            tally.named = true;
        }
        if tally.threads.insert(message.thread_id) {
            sender.threads.push(message.thread_id);
        }
        if !sender.accounts.contains(&message.account_id) {
            sender.accounts.push(message.account_id);
        }
    }

    let mut senders: Vec<InboxSender> = by_email.into_values().map(|t| t.sender).collect();
    senders.sort_by(|a, b| {
        b.messages.cmp(&a.messages).then(b.unread.cmp(&a.unread)).then(a.email.cmp(&b.email))
    });
    senders.truncate(limit);
    senders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{MailMessageId, PackId};
    use crate::mail::{Address, CategorySource, MessageFlags};
    use crate::packstore::PackRef;
    use jiff::SignedDuration;

    fn me() -> Me {
        Me::new(["Me@Example.com", "alias@example.com"], ["Hari"])
    }

    fn since() -> Timestamp {
        Timestamp::from_second(1_790_000_000).unwrap()
    }

    /// `minutes` after [`since`], so every message in these tests is in the
    /// window unless it says otherwise.
    fn at(minutes: i64) -> Timestamp {
        since() + SignedDuration::from_mins(minutes)
    }

    fn mail(account: AccountId, thread: ThreadId, from: Address, date: Timestamp) -> Message {
        Message {
            id: MailMessageId::new(),
            account_id: account,
            thread_id: thread,
            message_id_header: String::new(),
            date,
            from,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
            invite: None,
        }
    }

    fn person(name: &str, email: &str) -> Address {
        Address { name: name.into(), email: email.into() }
    }

    /// One message in a thread of its own, in the inbox `inbox` is
    /// collecting -- the shape most of these tests want.
    fn inbox_mail(
        inbox: &mut HashSet<ThreadId>,
        account: AccountId,
        from: Address,
        date: Timestamp,
    ) -> Message {
        let thread = ThreadId::new();
        inbox.insert(thread);
        mail(account, thread, from, date)
    }

    #[test]
    fn the_busiest_sender_comes_first_then_the_most_unread_then_by_address() {
        let account = AccountId::new();
        let mut inbox = HashSet::new();
        let mut messages = Vec::new();
        for minute in 0..3 {
            messages.push(inbox_mail(
                &mut inbox,
                account,
                person("News", "news@example.com"),
                at(minute),
            ));
        }
        // Two each from bob and carol: carol has one unread more, so she
        // ranks ahead of bob despite the address order.
        for minute in 0..2 {
            let mut bob =
                inbox_mail(&mut inbox, account, person("Bob", "bob@example.com"), at(minute));
            bob.flags.seen = true;
            messages.push(bob);
            let mut carol =
                inbox_mail(&mut inbox, account, person("Carol", "carol@example.com"), at(minute));
            carol.flags.seen = minute == 0;
            messages.push(carol);
        }
        // Two from dave, both read: level with bob, and after him by address.
        for minute in 0..2 {
            let mut dave =
                inbox_mail(&mut inbox, account, person("Dave", "dave@example.com"), at(minute));
            dave.flags.seen = true;
            messages.push(dave);
        }

        let out = inbox_senders(&messages, &inbox, &me(), since(), 10);
        let order: Vec<&str> = out.iter().map(|s| s.email.as_str()).collect();
        assert_eq!(
            order,
            ["news@example.com", "carol@example.com", "bob@example.com", "dave@example.com"]
        );
        assert_eq!((out[0].messages, out[0].unread), (3, 3));
        assert_eq!((out[1].messages, out[1].unread), (2, 1));
        assert_eq!((out[2].messages, out[2].unread), (2, 0));

        let top_two = inbox_senders(&messages, &inbox, &me(), since(), 2);
        assert_eq!(top_two.len(), 2, "truncated to the limit");
        assert_eq!(top_two[1].email, "carol@example.com");
    }

    #[test]
    fn your_own_mail_and_your_drafts_are_never_a_sender() {
        let account = AccountId::new();
        let mut inbox = HashSet::new();
        let mine = inbox_mail(&mut inbox, account, person("Me", " ALIAS@example.com"), at(1));
        let mut draft = inbox_mail(&mut inbox, account, person("Ana", "ana@example.com"), at(2));
        draft.flags.draft = true;
        let real = inbox_mail(&mut inbox, account, person("Ana", "ana@example.com"), at(3));

        let out = inbox_senders(&[mine, draft, real], &inbox, &me(), since(), 10);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].email, "ana@example.com");
        assert_eq!(out[0].messages, 1, "the draft is not counted");
    }

    #[test]
    fn only_the_window_and_only_the_inbox_are_counted() {
        let account = AccountId::new();
        let mut inbox = HashSet::new();
        let before = inbox_mail(&mut inbox, account, person("Ana", "ana@example.com"), at(-1));
        let on_the_line = inbox_mail(&mut inbox, account, person("Ana", "ana@example.com"), at(0));
        // Archived: its thread is not one the inbox holds.
        let archived = mail(account, ThreadId::new(), person("Ana", "ana@example.com"), at(5));

        let messages = [before, on_the_line.clone(), archived];
        let out = inbox_senders(&messages, &inbox, &me(), since(), 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].messages, 1, "the window starts at `since`, inclusive");
        assert_eq!(out[0].threads, vec![on_the_line.thread_id]);
    }

    #[test]
    fn an_address_is_one_sender_whatever_its_case_and_spacing() {
        let (work, home) = (AccountId::new(), AccountId::new());
        let mut inbox = HashSet::new();
        let messages = [
            inbox_mail(&mut inbox, work, person("News", "News@Example.com"), at(1)),
            inbox_mail(&mut inbox, home, person("News", " news@example.com "), at(2)),
            inbox_mail(&mut inbox, work, person("Nobody", "  "), at(3)),
        ];

        let out = inbox_senders(&messages, &inbox, &me(), since(), 10);
        assert_eq!(out.len(), 1, "no address is nobody to list: {out:?}");
        assert_eq!(out[0].email, "news@example.com");
        assert_eq!(out[0].messages, 2);
        assert_eq!(out[0].accounts, vec![home, work], "each account once, newest first");
    }

    #[test]
    fn a_thread_is_named_once_however_many_of_their_messages_are_in_it() {
        let account = AccountId::new();
        let (older, newer) = (ThreadId::new(), ThreadId::new());
        let inbox: HashSet<ThreadId> = [older, newer].into_iter().collect();
        let ana = person("Ana", "ana@example.com");
        let messages = [
            mail(account, older, ana.clone(), at(1)),
            mail(account, older, ana.clone(), at(2)),
            mail(account, newer, ana.clone(), at(3)),
            mail(account, older, ana, at(4)),
        ];

        let out = inbox_senders(&messages, &inbox, &me(), since(), 10);
        assert_eq!(out[0].messages, 4);
        assert_eq!(out[0].threads, vec![older, newer], "each once, by their newest message");
        assert_eq!(out[0].latest, at(4));
    }

    #[test]
    fn the_name_is_the_newest_one_given_and_the_address_when_none_was() {
        let account = AccountId::new();
        let mut inbox = HashSet::new();
        let messages = [
            inbox_mail(&mut inbox, account, person("Old Name", "ana@example.com"), at(1)),
            inbox_mail(&mut inbox, account, person("Ana Lima", "ana@example.com"), at(2)),
            inbox_mail(&mut inbox, account, person("", "ana@example.com"), at(3)),
            inbox_mail(&mut inbox, account, person("", "bot@example.com"), at(4)),
        ];

        let out = inbox_senders(&messages, &inbox, &me(), since(), 10);
        let name_of =
            |email: &str| out.iter().find(|s| s.email == email).map(|s| s.name.clone()).unwrap();
        assert_eq!(name_of("ana@example.com"), "Ana Lima", "newest message that had a name");
        assert_eq!(name_of("bot@example.com"), "bot@example.com", "never empty");
    }
}
