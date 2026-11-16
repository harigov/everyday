//! Follow-ups: mail waiting on the person, and mail the person is waiting on,
//! shown at the top of the Inbox the way Superhuman shows them.
//!
//! Two kinds, both decided by the decision model that sorts mail (see
//! [`crate::mail::sorting`]) and both kept only for recent mail -- the last
//! [`FollowUpSettings::window_days`]:
//!
//! - [`FollowUpKind::Reply`]: the newest message in a thread is somebody
//!   else's, and it is waiting on an answer from you. Cold outreach is the
//!   case to get right: a stranger's pitch ends in a question too, and
//!   somebody with a public address gets far more of those than mail that
//!   wants them. It is its own answer to the question, not a "no", so that
//!   the model has somewhere to put it -- see
//!   `karmayoga_service::mailfollowup`.
//! - [`FollowUpKind::Nudge`]: the newest message is your own, at least
//!   [`FollowUpSettings::nudge_after_days`] old, nobody has answered, and it
//!   asked for something.
//!
//! A thread's [`FollowUp`] names the message it was decided about. The moment
//! the thread's newest message that is not a draft is another one -- they
//! answered, you answered -- the store drops it
//! (`karmayoga_store_sql::mail::write::recompute_thread`), and the next pass
//! decides afresh. A person dismissing one drops it too, and leaves the
//! [`FollowUpMark`] behind, so it is not asked again until something new
//! arrives.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::id::MailMessageId;

/// What [`FollowUpSettings::window_days`] starts as: two weeks. Long enough
/// for a reply that slipped past a busy week, short enough that what shows is
/// still worth answering.
pub const DEFAULT_WINDOW_DAYS: u32 = 14;
/// The shortest window: a nudge waits at least a day, and less than the
/// window, so one day leaves nothing to nudge about.
pub const MIN_WINDOW_DAYS: u32 = 2;
/// The longest window. Mail older than two months that still wants an answer
/// is not what the top of an inbox is for.
pub const MAX_WINDOW_DAYS: u32 = 60;
/// What [`FollowUpSettings::nudge_after_days`] starts as.
pub const DEFAULT_NUDGE_AFTER_DAYS: u32 = 3;

/// Settings → Mail sorting, "Follow-ups". Part of
/// [`crate::mail::MailSorting`], since it runs on the same model, under the
/// same switch, over the same mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FollowUpSettings {
    /// Whether the pass runs at all. On from the start: it asks the same
    /// model about the same mail, under the same switch, as sorting does --
    /// and asks nothing while no decision model is switched on.
    #[serde(default = "yes")]
    pub on: bool,
    /// How many days back a thread is looked at.
    #[serde(default = "default_window_days")]
    pub window_days: u32,
    /// How long your own message waits for an answer before it is offered
    /// as a nudge.
    #[serde(default = "default_nudge_after_days")]
    pub nudge_after_days: u32,
}

fn yes() -> bool {
    true
}

fn default_window_days() -> u32 {
    DEFAULT_WINDOW_DAYS
}

fn default_nudge_after_days() -> u32 {
    DEFAULT_NUDGE_AFTER_DAYS
}

impl Default for FollowUpSettings {
    fn default() -> Self {
        Self {
            on: true,
            window_days: DEFAULT_WINDOW_DAYS,
            nudge_after_days: DEFAULT_NUDGE_AFTER_DAYS,
        }
    }
}

impl FollowUpSettings {
    pub fn validate(&self) -> Result<()> {
        if !(MIN_WINDOW_DAYS..=MAX_WINDOW_DAYS).contains(&self.window_days) {
            return Err(Error::Invalid(format!(
                "follow-ups look back between {MIN_WINDOW_DAYS} and {MAX_WINDOW_DAYS} days"
            )));
        }
        if self.nudge_after_days == 0 || self.nudge_after_days >= self.window_days {
            return Err(Error::Invalid(format!(
                "a nudge is offered after 1 to {} days, less than the {} days follow-ups look \
                 back",
                self.window_days - 1,
                self.window_days
            )));
        }
        Ok(())
    }
}

/// Which way a thread is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum FollowUpKind {
    /// Somebody is waiting on your answer.
    Reply,
    /// You are waiting on theirs, and might nudge them.
    Nudge,
}

/// A thread that is waiting -- [`crate::mail::Thread::follow_up`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FollowUp {
    pub kind: FollowUpKind,
    /// The message it was decided about: the newest message that is not a
    /// draft, theirs for [`FollowUpKind::Reply`], yours for
    /// [`FollowUpKind::Nudge`].
    pub message: MailMessageId,
    /// When that message was sent -- how long it has been waiting.
    pub since: Timestamp,
}

/// When the follow-up pass last asked about a thread, and about which
/// message -- [`crate::mail::Thread::follow_up_asked`]. Whatever it answered,
/// including nothing at all because the thread was plainly not one to ask
/// about (a newsletter, your own message too new to nudge about is *not*
/// marked: it is asked about once it is old enough).
///
/// The message rather than only the count, so that a draft coming and going
/// -- which changes the count but not what anybody is waiting on -- does not
/// have the model asked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FollowUpMark {
    /// The newest message that is not a draft, when it was asked.
    pub message: MailMessageId,
    /// The thread's message count then -- a thread whose count has not moved
    /// since is skipped without reading its messages.
    pub message_count: u32,
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_ups_start_on_with_two_weeks_and_three_days() {
        let s = FollowUpSettings::default();
        assert!(s.on);
        assert_eq!((s.window_days, s.nudge_after_days), (14, 3));
        s.validate().unwrap();
        let read: FollowUpSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(read, s);
    }

    #[test]
    fn a_nudge_waits_less_than_the_window() {
        let mut s = FollowUpSettings { window_days: 7, nudge_after_days: 7, ..Default::default() };
        assert!(s.validate().is_err());
        s.nudge_after_days = 0;
        assert!(s.validate().is_err());
        s.nudge_after_days = 6;
        s.validate().unwrap();
        s.window_days = MAX_WINDOW_DAYS + 1;
        assert!(s.validate().is_err());
        let one_day =
            FollowUpSettings { window_days: 1, nudge_after_days: 1, ..Default::default() };
        let err = one_day.validate().unwrap_err().to_string();
        assert!(err.contains("between 2 and 60"), "{err}");
    }

    #[test]
    fn kinds_are_named_in_camel_case_on_the_wire() {
        assert_eq!(serde_json::to_value(FollowUpKind::Reply).unwrap(), "reply");
        assert_eq!(serde_json::to_value(FollowUpKind::Nudge).unwrap(), "nudge");
    }
}
