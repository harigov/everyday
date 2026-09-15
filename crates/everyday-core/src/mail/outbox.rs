//! The outbox: legal state transitions, retry backoff, undo-send's delay,
//! and the reversible local write an optimistic action makes.
//!
//! `docs/plans/mail.md`'s "What an action does" is the whole of this
//! module's brief: "the local rows change in the same write that appends an
//! `Op` to the outbox... a failure is retried with backoff and, if
//! permanent, reverses the local change and says so beside the thread." Four
//! pure, synchronous pieces make that true without any of them touching a
//! socket or a database:
//!
//! - [`OpState::transition_to`] — which moves are legal, so a store or a
//!   drain loop cannot walk an op into a state it never promised.
//! - [`backoff_for_attempt`] — how long to wait before the next try.
//! - [`undo_send_delay`] — the window a send waits inside, five to thirty
//!   seconds, before the account task is allowed to touch it.
//! - [`apply_optimistic`] and [`revert`] — the local write itself, and its
//!   exact inverse, for the ops that change a message in place.

use super::records::{MessageFlags, OpKind, OpState};
use crate::error::{Error, Result};
use jiff::SignedDuration;

// ---- state transitions ----------------------------------------------------

impl OpState {
    /// Is moving from `self` to `next` one this state machine allows?
    ///
    /// Every edge the drain loop and a person's "cancel" or "retry" button
    /// actually use, and nothing else:
    ///
    /// ```text
    ///   Pending ──────► InFlight ──────► Done
    ///      │                │
    ///      │                ├──────────► Failed { permanent: true  }   (terminal)
    ///      │                └──────────► Failed { permanent: false }
    ///      │                                  │         ▲
    ///      ├──────────► Cancelled  (terminal) │         │
    ///      │                ▲                 │         │
    ///      └──────► Failed { permanent: true } ◄─────────┘  (a check before
    ///                (terminal; rejected      the first attempt, e.g. a
    ///                 before ever attempting) permission the account no
    ///                                         longer grants)
    ///      InFlight ◄───────────────── Pending   (a transient failure,
    ///                                             retried without ever
    ///                                             becoming `Failed`)
    ///      Failed { permanent: false } ──────► Pending    (explicit retry)
    ///      Failed { permanent: false } ──────► Cancelled
    /// ```
    ///
    /// `Done`, `Cancelled` and `Failed { permanent: true }` are terminal:
    /// nothing here allows a move out of any of them.
    pub fn can_transition_to(&self, next: &OpState) -> bool {
        use OpState::*;
        matches!(
            (self, next),
            (Pending, InFlight)
                | (Pending, Cancelled)
                | (Pending, Failed { .. })
                | (InFlight, Done)
                | (InFlight, Pending)
                | (InFlight, Failed { .. })
                | (Failed { permanent: false, .. }, Pending)
                | (Failed { permanent: false, .. }, Cancelled)
        )
    }
}

impl super::records::Op {
    /// Move this op to `next`, or refuse with [`Error::Invalid`] naming both
    /// states. Stamps [`Op::updated_at`](super::records::Op::updated_at) on
    /// success, which is what a store's `update_op` writes back.
    pub fn transition_to(&mut self, next: OpState) -> Result<()> {
        if !self.state.can_transition_to(&next) {
            return Err(Error::Invalid(format!(
                "an op cannot move from {:?} to {next:?}",
                self.state
            )));
        }
        self.state = next;
        self.updated_at = jiff::Timestamp::now();
        Ok(())
    }
}

// ---- retry backoff ----------------------------------------------------

/// How long to wait before attempt *N* (0-indexed: the first retry, after
/// attempt 0 failed, waits [`RETRY_BACKOFF`]`[0]`). Chosen to clear a flaky
/// connection quickly and a genuinely down server slowly: thirty seconds,
/// then a minute, five minutes, fifteen, and an hour for every attempt after
/// that -- an outbox op is durable (it is a table row), so there is no cost
/// to waiting rather than spinning.
pub const RETRY_BACKOFF: &[SignedDuration] = &[
    SignedDuration::from_secs(30),
    SignedDuration::from_secs(60),
    SignedDuration::from_secs(5 * 60),
    SignedDuration::from_secs(15 * 60),
    SignedDuration::from_secs(60 * 60),
];

/// The backoff for the attempt about to be made, given how many have already
/// happened. Attempts past the schedule's length repeat its last (longest)
/// entry rather than growing without bound.
pub fn backoff_for_attempt(attempts: u32) -> SignedDuration {
    let i = (attempts as usize).min(RETRY_BACKOFF.len() - 1);
    RETRY_BACKOFF[i]
}

// ---- undo send --------------------------------------------------------

/// Undo send's default window, in seconds, when nobody has configured one.
pub const UNDO_SEND_DEFAULT_SECONDS: u32 = 10;
/// The narrowest window Settings will offer.
pub const UNDO_SEND_MIN_SECONDS: u32 = 5;
/// The widest window Settings will offer.
pub const UNDO_SEND_MAX_SECONDS: u32 = 30;

/// The delay a `Send` op's `not_before` sits behind: [`UNDO_SEND_DEFAULT_SECONDS`]
/// unless `configured_seconds` says otherwise, clamped to
/// [`UNDO_SEND_MIN_SECONDS`]..=[`UNDO_SEND_MAX_SECONDS`] either way, so a
/// setting corrupted or typed in wrong cannot produce a send with no undo
/// window at all, or one so long it reads as the send having silently
/// failed. Send-later reuses the very same field with a delay of its own
/// choosing -- there is no second mechanism, per the plan.
pub fn undo_send_delay(configured_seconds: Option<u32>) -> SignedDuration {
    let secs = configured_seconds
        .unwrap_or(UNDO_SEND_DEFAULT_SECONDS)
        .clamp(UNDO_SEND_MIN_SECONDS, UNDO_SEND_MAX_SECONDS);
    SignedDuration::from_secs(i64::from(secs))
}

// ---- optimistic local writes, and their inverse ----------------------

/// Exactly enough of a [`Message`](super::records::Message)'s previous state
/// to undo one flag- or label-changing op. Nothing else on the message is
/// touched by [`apply_optimistic`], so nothing else needs remembering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageUndo {
    flags: MessageFlags,
    labels: Vec<String>,
}

/// Apply `kind`'s local effect to `message` ahead of the server confirming
/// it, and hand back what to pass to [`revert`] if the op later fails
/// permanently.
///
/// `None` for any `kind` that is not [`OpKind::is_flag_change`] --
/// `Archive`, `Trash`, `Move`, `Snooze`, `Send` and `AppendDraft` each change
/// something this function does not reach (mailbox membership, a thread's
/// own snooze state, a draft's state), and applying half an optimistic
/// change while the sync engine applies the other half is exactly the kind
/// of drift a single function boundary should not invite. Those callers
/// change the record they own directly, at the call site, and revert it the
/// same way.
pub fn apply_optimistic(
    kind: &OpKind,
    message: &mut super::records::Message,
) -> Option<MessageUndo> {
    if !kind.is_flag_change() {
        return None;
    }
    let undo = MessageUndo { flags: message.flags, labels: message.labels.clone() };
    match kind {
        OpKind::MarkRead => message.flags.seen = true,
        OpKind::MarkUnread => message.flags.seen = false,
        OpKind::Star => message.flags.flagged = true,
        OpKind::Unstar => message.flags.flagged = false,
        OpKind::Label { label } => {
            if !message.labels.iter().any(|l| l == label) {
                message.labels.push(label.clone());
            }
        }
        OpKind::Unlabel { label } => message.labels.retain(|l| l != label),
        _ => unreachable!("guarded by is_flag_change above"),
    }
    Some(undo)
}

/// The exact inverse of [`apply_optimistic`]: put `message` back exactly as
/// it was before that call, for a permanently failed op.
pub fn revert(undo: MessageUndo, message: &mut super::records::Message) {
    message.flags = undo.flags;
    message.labels = undo.labels;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AccountId, MailMessageId, MailboxId, ThreadId};
    use crate::mail::records::{Address, CategorySource, Message, OpTarget, Origin};
    use crate::packstore::PackRef;
    use jiff::Timestamp;

    fn message() -> Message {
        Message {
            id: MailMessageId::new(),
            account_id: AccountId::new(),
            thread_id: ThreadId::new(),
            message_id_header: "<a@example.com>".into(),
            date: Timestamp::now(),
            from: Address::bare("sender@example.com"),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "hi".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef {
                account: "acc".into(),
                pack: crate::id::PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
        }
    }

    // ---- transitions ----------------------------------------------------

    #[test]
    fn the_happy_path_goes_pending_in_flight_done() {
        assert!(OpState::Pending.can_transition_to(&OpState::InFlight));
        assert!(OpState::InFlight.can_transition_to(&OpState::Done));
    }

    #[test]
    fn a_permanent_failure_is_terminal() {
        let failed = OpState::Failed { permanent: true, message: "refused".into() };
        assert!(OpState::InFlight.can_transition_to(&failed));
        assert!(!failed.can_transition_to(&OpState::Pending));
        assert!(!failed.can_transition_to(&OpState::Cancelled));
    }

    #[test]
    fn a_transient_failure_can_be_retried_or_cancelled() {
        let failed = OpState::Failed { permanent: false, message: "timed out".into() };
        assert!(failed.can_transition_to(&OpState::Pending));
        assert!(failed.can_transition_to(&OpState::Cancelled));
    }

    #[test]
    fn done_and_cancelled_are_terminal() {
        for terminal in [OpState::Done, OpState::Cancelled] {
            for next in [
                OpState::Pending,
                OpState::InFlight,
                OpState::Done,
                OpState::Cancelled,
                OpState::Failed { permanent: false, message: String::new() },
            ] {
                assert!(!terminal.can_transition_to(&next), "{terminal:?} -> {next:?}");
            }
        }
    }

    #[test]
    fn op_transition_to_updates_the_timestamp_and_refuses_an_illegal_move() {
        let mut op = super::super::records::Op::new(
            AccountId::new(),
            OpKind::Archive,
            OpTarget::Thread(ThreadId::new()),
            Origin::Person,
        );
        let created = op.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(2));
        op.transition_to(OpState::InFlight).unwrap();
        assert_eq!(op.state, OpState::InFlight);
        assert!(op.updated_at > created);

        let err = op.transition_to(OpState::Cancelled).unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert_eq!(op.state, OpState::InFlight, "a refused move must not partially apply");
    }

    // ---- backoff ----------------------------------------------------------

    #[test]
    fn backoff_grows_then_holds_its_longest_entry() {
        assert_eq!(backoff_for_attempt(0), RETRY_BACKOFF[0]);
        assert_eq!(backoff_for_attempt(1), RETRY_BACKOFF[1]);
        let last = *RETRY_BACKOFF.last().unwrap();
        assert_eq!(backoff_for_attempt(RETRY_BACKOFF.len() as u32), last);
        assert_eq!(
            backoff_for_attempt(1_000_000),
            last,
            "must never panic on a huge attempt count"
        );
    }

    #[test]
    fn backoff_is_non_decreasing() {
        let mut prev = SignedDuration::ZERO;
        for attempt in 0..20u32 {
            let d = backoff_for_attempt(attempt);
            assert!(d >= prev, "backoff went backwards at attempt {attempt}");
            prev = d;
        }
    }

    // ---- undo send ----------------------------------------------------------

    #[test]
    fn undo_send_defaults_to_ten_seconds() {
        assert_eq!(undo_send_delay(None), SignedDuration::from_secs(10));
    }

    #[test]
    fn undo_send_is_clamped_to_five_and_thirty() {
        assert_eq!(undo_send_delay(Some(0)), SignedDuration::from_secs(5));
        assert_eq!(undo_send_delay(Some(1)), SignedDuration::from_secs(5));
        assert_eq!(undo_send_delay(Some(5)), SignedDuration::from_secs(5));
        assert_eq!(undo_send_delay(Some(30)), SignedDuration::from_secs(30));
        assert_eq!(undo_send_delay(Some(31)), SignedDuration::from_secs(30));
        assert_eq!(undo_send_delay(Some(9_999)), SignedDuration::from_secs(30));
    }

    #[test]
    fn undo_send_in_range_is_used_exactly() {
        assert_eq!(undo_send_delay(Some(17)), SignedDuration::from_secs(17));
    }

    // ---- apply_optimistic / revert ----------------------------------------

    #[test]
    fn mark_read_and_its_revert_are_exact_inverses() {
        let mut m = message();
        assert!(!m.flags.seen);
        let undo = apply_optimistic(&OpKind::MarkRead, &mut m).expect("a flag change");
        assert!(m.flags.seen);
        revert(undo, &mut m);
        assert!(!m.flags.seen);
    }

    #[test]
    fn star_and_unstar_round_trip() {
        let mut m = message();
        let undo = apply_optimistic(&OpKind::Star, &mut m).unwrap();
        assert!(m.flags.flagged);
        revert(undo, &mut m);
        assert!(!m.flags.flagged);
    }

    #[test]
    fn labelling_is_idempotent_and_reverses_cleanly() {
        let mut m = message();
        m.labels.push("Work".into());
        let undo = apply_optimistic(&OpKind::Label { label: "Work".into() }, &mut m).unwrap();
        assert_eq!(m.labels, vec!["Work".to_string()], "labelling twice must not duplicate");
        revert(undo, &mut m);
        assert_eq!(m.labels, vec!["Work".to_string()]);
    }

    #[test]
    fn unlabelling_and_its_revert_are_exact_inverses() {
        let mut m = message();
        m.labels = vec!["Work".into(), "Urgent".into()];
        let undo = apply_optimistic(&OpKind::Unlabel { label: "Work".into() }, &mut m).unwrap();
        assert_eq!(m.labels, vec!["Urgent".to_string()]);
        revert(undo, &mut m);
        assert_eq!(m.labels, vec!["Work".to_string(), "Urgent".to_string()]);
    }

    #[test]
    fn ops_that_are_not_flag_changes_are_left_to_their_own_caller() {
        let mut m = message();
        let before = m.clone();
        for kind in [
            OpKind::Archive,
            OpKind::Trash,
            OpKind::Move { to: MailboxId::new() },
            OpKind::Snooze { until: Timestamp::now() },
            OpKind::Send,
            OpKind::AppendDraft,
        ] {
            assert!(apply_optimistic(&kind, &mut m).is_none(), "{kind:?}");
        }
        assert_eq!(m, before, "a non-flag-change op must not touch the message at all");
    }
}
