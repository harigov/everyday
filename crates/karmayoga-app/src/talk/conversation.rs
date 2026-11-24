//! Which conversation a press carries on, and when a quiet one has had its
//! time.
//!
//! Two small decisions, kept apart from the machinery that acts on them so
//! that the clock is a parameter and every edge of them can be tested.

use std::time::{Duration, Instant};

use karmayoga_core::ConversationId;
use karmayoga_service::voice::VoiceState;

/// How long nothing may be said before a conversation that is waiting on the
/// person ends: the microphone must not stay open behind a card that has been
/// walked away from.
pub const IDLE_AFTER: Duration = Duration::from_secs(120);

/// The same for a conversation the wake word began: somebody who called the
/// assistant by name and has had their answer is done with it unless they
/// say more soon, as with a phone's assistant, and the microphone goes back
/// to listening for the name.
pub const WOKEN_IDLE_AFTER: Duration = Duration::from_secs(15);

/// A conversation from the talk key that has ended, remembered in memory so
/// that a press soon after carries it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Last {
    pub conversation: ConversationId,
    /// When it ended.
    pub at: Instant,
}

/// The conversation to talk in: the last one if it ended no more than
/// `continue_minutes` ago, else `None`, and a new one is begun. `0` always
/// begins a new one.
pub fn choose(last: Option<Last>, now: Instant, continue_minutes: u32) -> Option<ConversationId> {
    let window = Duration::from_secs(u64::from(continue_minutes) * 60);
    last.filter(|last| !window.is_zero() && now.saturating_duration_since(last.at) <= window)
        .map(|last| last.conversation)
}

/// How long a conversation in `state` may be left alone before it ends:
/// [`IDLE_AFTER`], or [`WOKEN_IDLE_AFTER`] for one the wake word began while
/// it is only listening. A question waiting on the person -- a card to read
/// before pressing Allow -- has the whole of [`IDLE_AFTER`] either way, and so
/// has a dictation (`dictating`): somebody dictating stops to think of the
/// next sentence, and a few seconds of that is not their having gone.
pub fn idle_after(state: VoiceState, woke: bool, dictating: bool) -> Duration {
    if woke && !dictating && state == VoiceState::Listening { WOKEN_IDLE_AFTER } else { IDLE_AFTER }
}

/// Whether a conversation that has been in `state` since `quiet_since` has
/// been left alone for `after` -- see [`idle_after`].
///
/// Only while it is waiting on the person -- listening, or holding a question
/// -- does the time count. The assistant working on something for ten minutes,
/// or reading a long answer aloud, is not the person having gone away.
pub fn idle_expired(
    state: VoiceState,
    quiet_since: Instant,
    now: Instant,
    after: Duration,
) -> bool {
    matches!(state, VoiceState::Listening | VoiceState::Asking | VoiceState::WaitingForClick)
        && now.saturating_duration_since(quiet_since) >= after
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(ended: Instant) -> Option<Last> {
        Some(Last { conversation: ConversationId::new(), at: ended })
    }

    #[test]
    fn a_press_soon_after_carries_the_last_conversation_on() {
        let ended = Instant::now();
        let last = last(ended);
        let carried = choose(last, ended + Duration::from_secs(9 * 60), 10);
        assert_eq!(carried, last.map(|l| l.conversation));
        assert_eq!(
            choose(last, ended + Duration::from_secs(10 * 60), 10),
            last.map(|l| l.conversation),
            "on the minute is still within it"
        );
    }

    #[test]
    fn a_press_after_that_begins_a_new_one() {
        let ended = Instant::now();
        assert_eq!(choose(last(ended), ended + Duration::from_secs(10 * 60 + 1), 10), None);
    }

    #[test]
    fn nothing_to_carry_on_begins_a_new_one() {
        assert_eq!(choose(None, Instant::now(), 10), None);
    }

    #[test]
    fn zero_minutes_always_begins_a_new_one() {
        let ended = Instant::now();
        assert_eq!(choose(last(ended), ended, 0), None);
    }

    #[test]
    fn a_quiet_conversation_ends_after_two_minutes_of_waiting() {
        let since = Instant::now();
        let just_under = since + IDLE_AFTER - Duration::from_secs(1);
        assert!(!idle_expired(VoiceState::Listening, since, just_under, IDLE_AFTER));
        assert!(idle_expired(VoiceState::Listening, since, since + IDLE_AFTER, IDLE_AFTER));
        assert!(idle_expired(VoiceState::Asking, since, since + IDLE_AFTER, IDLE_AFTER));
        assert!(idle_expired(VoiceState::WaitingForClick, since, since + IDLE_AFTER, IDLE_AFTER));
        // Woken by name, it is over far sooner.
        let woken = since + WOKEN_IDLE_AFTER;
        assert!(idle_expired(VoiceState::Listening, since, woken, WOKEN_IDLE_AFTER));
        assert!(!idle_expired(VoiceState::Listening, since, woken, IDLE_AFTER));
    }

    #[test]
    fn a_conversation_that_is_busy_is_not_idle_however_long_it_takes() {
        let since = Instant::now();
        let long_after = since + Duration::from_secs(3_600);
        for state in [
            VoiceState::Starting,
            VoiceState::Hearing,
            VoiceState::Understanding,
            VoiceState::Thinking,
            VoiceState::Speaking,
        ] {
            assert!(!idle_expired(state, since, long_after, IDLE_AFTER), "{state:?}");
        }
    }

    #[test]
    fn a_woken_conversation_ends_soon_only_while_it_listens() {
        assert_eq!(idle_after(VoiceState::Listening, true, false), WOKEN_IDLE_AFTER);
        assert_eq!(idle_after(VoiceState::Listening, false, false), IDLE_AFTER);
        for question in [VoiceState::Asking, VoiceState::WaitingForClick] {
            assert_eq!(idle_after(question, true, false), IDLE_AFTER, "{question:?}");
        }
    }

    #[test]
    fn a_dictation_is_given_the_long_wait_however_the_conversation_began() {
        for woke in [false, true] {
            for state in [VoiceState::Listening, VoiceState::Hearing, VoiceState::Understanding] {
                assert_eq!(idle_after(state, woke, true), IDLE_AFTER, "{woke} {state:?}");
            }
        }
        // A few seconds' pause to think is not the end of it, where it would
        // have been for a woken conversation that was not dictating.
        let since = Instant::now();
        let paused = since + WOKEN_IDLE_AFTER + Duration::from_secs(1);
        let after = idle_after(VoiceState::Listening, true, true);
        assert!(!idle_expired(VoiceState::Listening, since, paused, after));
        let after = idle_after(VoiceState::Listening, true, false);
        assert!(idle_expired(VoiceState::Listening, since, paused, after));
    }
}
