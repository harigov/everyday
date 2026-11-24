//! What the talk key's presses and releases mean.
//!
//! Voice typing's key is held or pressed to dictate one thing and be done;
//! the talk key opens a conversation that goes on after the key is let go, so
//! what a release means is a question of its own. Nothing here touches the
//! desktop, the panel or the session: the key is a function of the edges it
//! has sent and the state of the conversation, so that every case can be
//! tested.
//!
//! # What a desktop sends
//!
//! A held key is activated again and again -- GNOME repeats it after its
//! repeat delay and then every 30 ms or so -- and a key let go with a
//! modifier going up first is never reported as let go at all. So a press is
//! not always a press and a missing release is not always a key still held:
//! [`classify`] decides which edges are the person's -- by voice typing's own
//! rule, against the repeat delay and interval GNOME says it is using -- and
//! a conversation that is waiting for a release decides the key has gone up
//! when the repeats stop, after [`release_wait`].
//!
//! # Two ways to hold it
//!
//! In toggle mode, a press starts the conversation and the next press ends
//! it. In hold mode the key is a push-to-talk: down opens the microphone, up
//! closes it once what was said has been captured. A tap, held for less than
//! [`TAP`], is "hands free" instead -- the microphone stays open and the next
//! press ends the conversation -- which is how a conversation is begun by
//! somebody who has not learnt to hold.

use std::time::{Duration, Instant};

use karmayoga_service::voice::VoiceState;

pub(crate) use crate::voicetyping::Edge as Down;
use crate::voicetyping::{ARRIVAL_SLACK, KeyMode, Repeats, first_press};

/// A key held for less than this was a tap: in hold mode it starts a
/// conversation hands-free instead of muting it the moment it is let go.
pub const TAP: Duration = Duration::from_millis(400);

/// How long the microphone stays open after the key says it was let go: a
/// device hands audio over a little after it hears it, and the end of the
/// last word is in that little.
pub const TAIL: Duration = Duration::from_millis(250);

/// One thing the key said, once the keyboard's repeating is told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The person pressed the key.
    Press,
    /// The keyboard repeating a key that is held.
    Repeat,
    /// The key came up.
    Release,
}

/// Which kind of edge this is, keeping `down` -- what the key last said while
/// down, with no release since -- up to date.
///
/// Voice typing's rule, so that the two keys cannot disagree about the same
/// keyboard: a press that comes when `repeats` says the keyboard would repeat
/// a held key is the repeat, and any other press with no release between is a
/// new press whose release went missing -- GNOME sends no release when a
/// modifier is let go before the key.
pub fn classify(
    down: &mut Option<Down>,
    pressed: bool,
    at: Instant,
    stamp: Option<Duration>,
    repeats: Repeats,
) -> Edge {
    match (first_press(down, pressed, at, stamp, repeats), pressed) {
        (_, false) => Edge::Release,
        (true, true) => Edge::Press,
        (false, true) => Edge::Repeat,
    }
}

/// How long the repeats of a held key may stop before the key is taken to
/// have been let go, when the desktop did not say so: the interval between
/// repeats and the slack for a busy runtime, as voice typing waits. `None`
/// where the desktop does not repeat, or is not known to -- nothing is then
/// waiting on repeats to stop, and a key let go without saying so leaves the
/// microphone open until the next press.
pub fn release_wait(repeats: Repeats) -> Option<Duration> {
    match repeats {
        Repeats::Every { interval, .. } => Some(interval + ARRIVAL_SLACK),
        Repeats::Unknown | Repeats::Off => None,
    }
}

/// How the key is working for the conversation under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Toggle mode, or a conversation begun without the key: the next press
    /// ends it.
    Toggled,
    /// Hold mode, the key down since `since`: the microphone is open.
    Held { since: Instant },
    /// Hold mode, after a tap: the microphone stays open and the next press
    /// ends the conversation.
    Free,
    /// Hold mode, after the key was let go: the microphone is shut until the
    /// next press.
    Muted,
}

impl Posture {
    /// How a conversation begun by the key at `at` is held. `releases` is
    /// whether the desktop says when the key is let go; where it does not,
    /// holding to talk cannot work, and the key starts and ends as in toggle
    /// mode.
    pub fn starting(mode: KeyMode, releases: bool, at: Instant) -> Self {
        match mode {
            KeyMode::Hold if releases => Posture::Held { since: at },
            _ => Posture::Toggled,
        }
    }
}

/// Where the conversation is, as far as the key is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// None, or one that is over and only its card lingers: a press starts one.
    Idle,
    /// Starting or under way. `opened` is whether the session is open and the
    /// microphone listening yet.
    Talking { posture: Posture, opened: bool },
}

/// What an edge of the key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Nothing,
    /// Begin a conversation.
    Start,
    /// End it.
    End,
    /// Hold mode: the key went down on a conversation that was waiting for
    /// it. Open the microphone.
    Unmute,
    /// Hold mode: the key was let go quickly, or before the session was open
    /// to hear anything. Keep listening until the next press.
    HandsFree,
    /// Hold mode: the key was let go after talking. Shut the microphone once
    /// what was said has been captured -- see [`may_mute_now`].
    Mute,
}

/// What the key does, as a function of nothing but its inputs.
pub fn decide(stage: Stage, edge: Edge, now: Instant) -> Act {
    let (posture, opened) = match stage {
        Stage::Idle => return if edge == Edge::Press { Act::Start } else { Act::Nothing },
        Stage::Talking { posture, opened } => (posture, opened),
    };
    match (posture, edge) {
        // The keyboard's own repeating says nothing, whatever is going on.
        (_, Edge::Repeat) => Act::Nothing,
        (Posture::Toggled | Posture::Free, Edge::Press) => Act::End,
        (Posture::Toggled | Posture::Free | Posture::Muted, Edge::Release) => Act::Nothing,
        // A press while it is thought to be down already comes after a
        // release that went missing, or is a repeat that was not recognised;
        // either way it is the end, as it is for a hands-free conversation.
        (Posture::Held { .. }, Edge::Press) => Act::End,
        (Posture::Held { since }, Edge::Release) => {
            // Let go before anything could be heard is not push-to-talk, and
            // muting then would leave a conversation nobody could be heard
            // in: it is hands-free.
            if !opened || now.saturating_duration_since(since) < TAP {
                Act::HandsFree
            } else {
                Act::Mute
            }
        }
        (Posture::Muted, Edge::Press) => Act::Unmute,
    }
}

/// Whether the microphone may be shut now that the key has been let go.
///
/// Muting while somebody is being heard throws away what they had said so
/// far -- the session forgets a half-heard utterance when it is muted -- so
/// it waits until the state leaves [`VoiceState::Hearing`]: the pause that
/// ends the utterance has gone by, and what was said is being understood.
pub fn may_mute_now(state: VoiceState) -> bool {
    state != VoiceState::Hearing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        // One base for every call, so differences are what the test says.
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    /// GNOME's defaults: the first repeat 500 ms after the press, then one
    /// every 30.
    const GNOME: Repeats =
        Repeats::Every { delay: Duration::from_millis(500), interval: Duration::from_millis(30) };

    fn edges(list: &[(u64, bool)], stamped: bool, repeats: Repeats) -> Vec<Edge> {
        let mut down = None;
        list.iter()
            .map(|&(ms, pressed)| {
                let stamp = stamped.then(|| Duration::from_millis(10_000 + ms));
                classify(&mut down, pressed, at(ms), stamp, repeats)
            })
            .collect()
    }

    #[test]
    fn a_held_keys_repeats_are_not_presses() {
        let held = [(0, true), (500, true), (530, true), (560, true), (590, true), (620, false)];
        for stamped in [false, true] {
            assert_eq!(
                edges(&held, stamped, GNOME),
                [
                    Edge::Press,
                    Edge::Repeat,
                    Edge::Repeat,
                    Edge::Repeat,
                    Edge::Repeat,
                    Edge::Release
                ]
            );
        }
    }

    #[test]
    fn a_press_when_the_keyboard_would_not_repeat_is_a_new_press_even_within_a_second() {
        // Ctrl let go first, so GNOME never says the key came up; the person
        // presses again 800 ms later -- not when a held key would repeat.
        let twice = [(0, true), (800, true)];
        assert_eq!(edges(&twice, true, GNOME), [Edge::Press, Edge::Press]);
        // And a quick double press, 200 ms apart.
        let quick = [(0, true), (200, true)];
        assert_eq!(edges(&quick, true, GNOME), [Edge::Press, Edge::Press]);
    }

    #[test]
    fn a_press_long_after_the_last_edge_is_a_new_press_even_with_no_release_between() {
        let toggled_twice = [(0, true), (5_000, true)];
        for stamped in [false, true] {
            assert_eq!(edges(&toggled_twice, stamped, GNOME), [Edge::Press, Edge::Press]);
        }
        // And the repeats of a long hold keep it a hold, however long.
        let long: Vec<(u64, bool)> =
            std::iter::once((0, true)).chain((0..200).map(|i| (500 + i * 30, true))).collect();
        let kinds = edges(&long, false, GNOME);
        assert_eq!(kinds[0], Edge::Press);
        assert!(kinds[1..].iter().all(|k| *k == Edge::Repeat));
    }

    #[test]
    fn where_the_keyboard_does_not_repeat_a_press_is_always_a_press() {
        let twice = [(0, true), (40, true), (80, true)];
        assert_eq!(edges(&twice, false, Repeats::Off), [Edge::Press; 3]);
    }

    #[test]
    fn where_the_repeat_is_not_known_a_press_within_a_second_is_taken_for_one() {
        let list = [(0, true), (500, true), (530, true), (5_000, true)];
        assert_eq!(
            edges(&list, false, Repeats::Unknown),
            [Edge::Press, Edge::Repeat, Edge::Repeat, Edge::Press]
        );
    }

    #[test]
    fn a_release_means_the_next_press_is_new_however_soon() {
        let double_tap = [(0, true), (60, false), (150, true), (220, false)];
        for repeats in [GNOME, Repeats::Unknown, Repeats::Off] {
            assert_eq!(
                edges(&double_tap, false, repeats),
                [Edge::Press, Edge::Release, Edge::Press, Edge::Release]
            );
        }
    }

    #[test]
    fn the_desktops_own_timestamps_beat_when_events_got_here() {
        // Arrival times 530 ms apart -- one repeat delay -- but the desktop
        // says the second came a minute after the first.
        let mut down = None;
        classify(&mut down, true, at(0), Some(Duration::from_millis(5_000)), GNOME);
        let late = Some(Duration::from_millis(65_000));
        assert_eq!(classify(&mut down, true, at(530), late, GNOME), Edge::Press);
        // And the other way: they arrived 1.5 s apart -- a runtime that was
        // busy -- but the desktop says the second came when a repeat would.
        let mut down = None;
        classify(&mut down, true, at(0), Some(Duration::from_millis(5_000)), GNOME);
        let on_time = Some(Duration::from_millis(5_500));
        assert_eq!(classify(&mut down, true, at(1_500), on_time, GNOME), Edge::Repeat);
    }

    #[test]
    fn only_a_key_that_has_begun_repeating_has_repeats_to_stop() {
        let mut down = None;
        classify(&mut down, true, at(0), None, GNOME);
        assert_eq!(down.map(|d| d.repeat), Some(false), "the first press");
        classify(&mut down, true, at(500), None, GNOME);
        assert_eq!(down.map(|d| d.repeat), Some(true), "its first repeat");
        classify(&mut down, false, at(900), None, GNOME);
        assert_eq!(down, None);
        classify(&mut down, true, at(950), None, GNOME);
        assert_eq!(down.map(|d| d.repeat), Some(false), "a new press starts again");
    }

    #[test]
    fn a_missing_release_is_waited_for_by_an_interval_and_the_slack_where_the_desktop_repeats() {
        assert_eq!(release_wait(GNOME), Some(Duration::from_millis(30) + ARRIVAL_SLACK));
        assert_eq!(release_wait(Repeats::Off), None);
        assert_eq!(release_wait(Repeats::Unknown), None);
    }

    fn talking(posture: Posture) -> Stage {
        Stage::Talking { posture, opened: true }
    }

    #[test]
    fn toggle_starts_on_a_press_and_ends_on_the_next() {
        let now = at(0);
        assert_eq!(decide(Stage::Idle, Edge::Press, now), Act::Start);
        assert_eq!(decide(Stage::Idle, Edge::Release, now), Act::Nothing);
        assert_eq!(decide(Stage::Idle, Edge::Repeat, now), Act::Nothing);
        assert_eq!(decide(talking(Posture::Toggled), Edge::Press, now), Act::End);
        assert_eq!(decide(talking(Posture::Toggled), Edge::Release, now), Act::Nothing);
        assert_eq!(decide(talking(Posture::Toggled), Edge::Repeat, now), Act::Nothing);
    }

    #[test]
    fn a_conversation_still_starting_is_ended_by_a_press_too() {
        let stage = Stage::Talking { posture: Posture::Toggled, opened: false };
        assert_eq!(decide(stage, Edge::Press, at(0)), Act::End);
    }

    #[test]
    fn letting_go_of_a_tap_is_hands_free_and_the_next_press_ends_it() {
        let since = at(0);
        let held = talking(Posture::Held { since });
        assert_eq!(
            decide(held, Edge::Release, since + TAP - Duration::from_millis(1)),
            Act::HandsFree
        );
        assert_eq!(decide(talking(Posture::Free), Edge::Release, at(900)), Act::Nothing);
        assert_eq!(decide(talking(Posture::Free), Edge::Press, at(900)), Act::End);
    }

    #[test]
    fn letting_go_of_a_hold_mutes_it_until_the_key_goes_down_again() {
        let since = at(0);
        let held = talking(Posture::Held { since });
        assert_eq!(decide(held, Edge::Release, since + TAP), Act::Mute);
        assert_eq!(decide(held, Edge::Release, since + Duration::from_secs(8)), Act::Mute);
        assert_eq!(decide(held, Edge::Repeat, since + Duration::from_secs(1)), Act::Nothing);

        let waiting = talking(Posture::Muted);
        assert_eq!(decide(waiting, Edge::Press, at(20_000)), Act::Unmute);
        assert_eq!(decide(waiting, Edge::Release, at(20_000)), Act::Nothing);
    }

    #[test]
    fn a_press_on_a_key_thought_to_be_down_is_the_end() {
        // The release went missing, so this is a new press: voice typing's
        // answer too.
        let held = talking(Posture::Held { since: at(0) });
        assert_eq!(decide(held, Edge::Press, at(3_000)), Act::End);
    }

    #[test]
    fn letting_go_before_the_session_could_hear_anything_is_hands_free() {
        // Held for two seconds while the models loaded: nothing was heard,
        // and muting would leave the person talking to nobody.
        let since = at(0);
        let stage = Stage::Talking { posture: Posture::Held { since }, opened: false };
        assert_eq!(decide(stage, Edge::Release, since + Duration::from_secs(2)), Act::HandsFree);
    }

    #[test]
    fn hold_mode_where_the_desktop_never_says_release_is_toggle() {
        let now = at(0);
        assert_eq!(Posture::starting(KeyMode::Hold, false, now), Posture::Toggled);
        assert_eq!(Posture::starting(KeyMode::Toggle, true, now), Posture::Toggled);
        assert_eq!(Posture::starting(KeyMode::Hold, true, now), Posture::Held { since: now });
    }

    #[test]
    fn the_microphone_is_not_shut_on_somebody_mid_sentence() {
        assert!(!may_mute_now(VoiceState::Hearing));
        for state in [
            VoiceState::Starting,
            VoiceState::Listening,
            VoiceState::Understanding,
            VoiceState::Thinking,
            VoiceState::Speaking,
            VoiceState::Asking,
            VoiceState::WaitingForClick,
        ] {
            assert!(may_mute_now(state), "{state:?}");
        }
    }
}
