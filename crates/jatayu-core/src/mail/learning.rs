//! What writing help has learned from the replies the person actually sent.
//!
//! The measured [`crate::mail::voice::VoiceProfile`] counts things -- how
//! long, which greeting, which sign-off. It cannot see how somebody answers:
//! that they say yes in one line, never apologise for a slow reply, turn a
//! request for a call into a time, or that the suggestions offered to them
//! keep being cut in half before they are sent. Those are what
//! [`VoiceLearning::notes`] are: short rules a model writes now and then from
//! their recent replies -- each beside the message it answered and, when
//! there were any, the replies that were suggested for it -- and every
//! drafting prompt carries, under what the person has said themselves in
//! Settings → Mail writing, which wins.
//!
//! Learning runs in `jatayu_service::mail::voicelearn`, on the medium model,
//! behind the same writing-help switch and acknowledgement as everything else
//! that writes mail for them, and only while [`crate::mail::MailWriting::learn`]
//! is on. The notes are shown in Settings → Mail writing, where they can be
//! edited or forgotten.
//!
//! Its own sealed row rather than a field of [`crate::mail::MailWriting`]:
//! that one is the person's, saved whole from the settings pane, and a save
//! made from a pane opened before a pass finished would put back the notes
//! the pass had just replaced.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::{AccountId, MailMessageId, ThreadId};

/// Longest [`VoiceLearning::notes`] may be. They go with every suggested
/// reply, on the fastest model there is.
pub const MAX_NOTES_CHARS: usize = 2_000;
/// How many [`OfferedReplies`] are kept waiting for the person's own reply.
pub const MAX_OFFERED: usize = 40;
/// How long an [`OfferedReplies`] waits for the person's own reply before it
/// is let go: a thread nobody answered in a month tells nothing about how
/// they answer.
pub const OFFERED_KEEP_DAYS: i64 = 30;

/// The sealed singleton behind "What it has learned" in Settings → Mail
/// writing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct VoiceLearning {
    /// What was learned about how they reply, one rule a line. Empty until
    /// the first pass, or after they asked for it to be forgotten.
    #[serde(default)]
    pub notes: String,
    /// When the notes were last written, by a pass or by the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_at: Option<Timestamp>,
    /// How many of their replies the notes have been learned from, all told.
    #[serde(default)]
    pub replies_read: u32,
    /// The newest reply of theirs already learned from. The next pass reads
    /// only replies sent after it. `None`: nothing yet, so the first pass
    /// reads back a few weeks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_through: Option<Timestamp>,
    /// The person edited the notes by hand since the last pass. The next pass
    /// starts from their edit and keeps it unless their replies plainly say
    /// otherwise.
    #[serde(default)]
    pub edited: bool,
    /// Suggested replies offered recently, waiting for the person's own reply
    /// in the same thread to be read beside them. Never shown anywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered: Vec<OfferedReplies>,
}

/// The replies that were suggested for one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OfferedReplies {
    pub account: AccountId,
    pub thread: ThreadId,
    /// The message they were suggested as replies to.
    pub message: MailMessageId,
    pub at: Timestamp,
    /// Each reply's whole text, as offered.
    pub replies: Vec<String>,
}

impl VoiceLearning {
    /// Notes the person typed: checked the way their instructions are.
    pub fn validate(&self) -> Result<()> {
        if self.notes.chars().count() > MAX_NOTES_CHARS {
            return Err(Error::Invalid(format!(
                "that is too long to go with every reply; keep it under {MAX_NOTES_CHARS} \
                 characters"
            )));
        }
        Ok(())
    }

    /// Keep `offered` for `message`, replacing an earlier set for the same
    /// message, dropping what is older than [`OFFERED_KEEP_DAYS`] and, past
    /// [`MAX_OFFERED`], the oldest.
    pub fn remember_offered(&mut self, offered: OfferedReplies, now: Timestamp) {
        self.offered.retain(|o| o.message != offered.message);
        self.offered.push(offered);
        self.forget_stale_offers(now);
    }

    /// Drop offers older than [`OFFERED_KEEP_DAYS`], and the oldest past
    /// [`MAX_OFFERED`].
    pub fn forget_stale_offers(&mut self, now: Timestamp) {
        let oldest = now - jiff::SignedDuration::from_hours(24 * OFFERED_KEEP_DAYS);
        self.offered.retain(|o| o.at >= oldest);
        self.offered.sort_by_key(|o| o.at);
        let over = self.offered.len().saturating_sub(MAX_OFFERED);
        self.offered.drain(..over);
    }

    /// The replies offered for `message`, if any were.
    pub fn offered_for(&self, message: MailMessageId) -> Option<&OfferedReplies> {
        self.offered.iter().find(|o| o.message == message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(message: MailMessageId, at: Timestamp) -> OfferedReplies {
        OfferedReplies {
            account: AccountId::new(),
            thread: ThreadId::new(),
            message,
            at,
            replies: vec!["Sounds good.".into()],
        }
    }

    #[test]
    fn nothing_learned_reads_as_the_default() {
        let read: VoiceLearning = serde_json::from_str("{}").unwrap();
        assert_eq!(read, VoiceLearning::default());
        assert!(read.notes.is_empty() && read.learned_at.is_none());
    }

    #[test]
    fn offers_are_kept_once_each_for_a_month_and_forty_at_most() {
        let now = Timestamp::now();
        let mut l = VoiceLearning::default();
        let m = MailMessageId::new();
        l.remember_offered(offer(m, now), now);
        l.remember_offered(offer(m, now), now);
        assert_eq!(l.offered.len(), 1, "a second set for one message replaces the first");

        let old = now - jiff::SignedDuration::from_hours(24 * (OFFERED_KEEP_DAYS + 1));
        l.offered.push(offer(MailMessageId::new(), old));
        l.forget_stale_offers(now);
        assert_eq!(l.offered.len(), 1);

        for _ in 0..MAX_OFFERED + 5 {
            l.remember_offered(offer(MailMessageId::new(), now), now);
        }
        assert_eq!(l.offered.len(), MAX_OFFERED);
        assert!(l.offered_for(m).is_none(), "the oldest went first");
    }

    #[test]
    fn notes_have_a_limit() {
        let mut l = VoiceLearning { notes: "x".repeat(MAX_NOTES_CHARS), ..Default::default() };
        l.validate().unwrap();
        l.notes.push('x');
        assert!(l.validate().is_err());
    }
}
