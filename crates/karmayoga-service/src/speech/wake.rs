//! Listening for the assistant to be called by name.
//!
//! With the wake word on, the desktop's microphone is listened to whenever
//! nothing else is using it, and somebody who says the assistant's name --
//! "Siri, type I won't be available tomorrow" -- starts a conversation with
//! what they said after it as its first words, as if they had pressed the talk
//! key and said them. See `docs/plans/wake-word.md`.
//!
//! There is no separate wake-word model: the voice detector and the
//! recogniser voice mode already has ([`LiveTranscription`]) hear each
//! utterance, and [`heard`] looks for the name at its start. So any name
//! works, spelled any way, the request can come in the same breath, and
//! nothing runs while nobody is talking but the detector. What somebody said
//! without the name never leaves this module: it is matched and dropped, not
//! sent anywhere, shown or kept.

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use crate::speech::SAMPLE_RATE;
use crate::speech::detect::Tuning;
use crate::speech::expect::Occasion;
use crate::speech::live::{LiveEvent, LiveTranscription};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Somebody said the assistant's name at the start of what they said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Woken {
    /// What they said after the name, as the recogniser wrote it: the first
    /// thing they ask, or empty when they said only the name and are waiting
    /// to be listened to.
    pub rest: String,
}

/// How much audio one detector hears before it is made afresh: an hour of
/// it. Sherpa counts the samples it has been fed in an `i32`, which runs out
/// after about 37 hours, and this listens all day and night if let.
const FRESH_EVERY: u64 = SAMPLE_RATE as u64 * 60 * 60;

/// Both ends of a running listener, from the transport's side: microphone
/// audio at [`SAMPLE_RATE`](crate::speech::SAMPLE_RATE) in, every time the
/// name is heard out. Dropping `input` ends it, and `woken` closes.
pub struct WakeLink {
    pub input: UnboundedSender<Vec<i16>>,
    pub woken: UnboundedReceiver<Woken>,
}

/// Words said before the name that are still calling it: "hey Siri", "OK
/// Siri". One of them, at the very start.
const GREETINGS: &[&str] = &["hey", "hi", "ok", "okay", "oh"];

/// The fewest letters a single-word name has before a recogniser's near miss
/// of it counts ("Carma" for "Karma"). Shorter names must be heard exactly:
/// one letter off a four-letter word is too often another word ("Siri" and
/// "sir").
const NEAR_MISS_FROM: usize = 5;

/// Whether `transcript` -- one utterance, as the recogniser wrote it --
/// begins with one of `names`, optionally after a greeting ("hey", "OK"):
/// what was said after it, or `None`. Case, punctuation and spacing aside;
/// a one-word name of [`NEAR_MISS_FROM`] letters or more may be one letter
/// off. A name later in the sentence ("I asked Siri yesterday") is somebody
/// talking about the assistant, not to it.
pub fn heard(transcript: &str, names: &[String]) -> Option<String> {
    let words = words_of(transcript);
    let starts = match words.first() {
        Some(first) if GREETINGS.contains(&first.text.as_str()) => vec![0, 1],
        _ => vec![0],
    };
    for name in names {
        let name: Vec<String> = words_of(name).into_iter().map(|w| w.text).collect();
        if name.is_empty() || name.iter().map(String::len).sum::<usize>() < 2 {
            continue;
        }
        for &start in &starts {
            let Some(said) = words.get(start..start + name.len()) else { continue };
            let matches = said.iter().zip(&name).all(|(said, name)| {
                said.text == *name
                    || (name.chars().count() >= NEAR_MISS_FROM && within_one(&said.text, name))
            });
            if matches {
                let end = said.last().map_or(0, |w| w.end);
                let rest = transcript[end..]
                    .trim_start_matches(|c: char| c.is_whitespace() || is_joining_mark(c))
                    .trim_end();
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// The punctuation a recogniser puts between a name and what follows it:
/// "Siri, type ...", "Siri. What's ...", "Siri -- remind me".
fn is_joining_mark(c: char) -> bool {
    matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | '-' | '–' | '—' | '…')
}

/// A word of an utterance: lower case, letters, digits and apostrophes only,
/// and where in the original it ends.
struct Word {
    text: String,
    end: usize,
}

fn words_of(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut end = 0;
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() || matches!(c, '\'' | '’') {
            if c.is_alphanumeric() {
                current.extend(c.to_lowercase());
            }
            end = at + c.len_utf8();
        } else if !current.is_empty() {
            words.push(Word { text: std::mem::take(&mut current), end });
        }
    }
    if !current.is_empty() {
        words.push(Word { text: current, end });
    }
    words
}

/// Whether `a` and `b` are the same but for one letter put in, taken out or
/// changed.
fn within_one(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

/// Start listening over `live` for `names`, on the current Tokio runtime.
pub fn spawn(live: LiveTranscription, names: Vec<String>) -> WakeLink {
    let (input, audio) = unbounded_channel();
    let (tell, woken) = unbounded_channel();
    tokio::spawn(run(live, names, audio, tell));
    WakeLink { input, woken }
}

async fn run(
    live: LiveTranscription,
    names: Vec<String>,
    mut audio: UnboundedReceiver<Vec<i16>>,
    woken: UnboundedSender<Woken>,
) {
    let LiveTranscription { control, events: mut heard_events } = live;
    // Fed since the detector was last made afresh, and whether somebody is
    // talking, whom a fresh one would cut off.
    let mut fed: u64 = 0;
    let mut talking = false;
    loop {
        tokio::select! {
            samples = audio.recv() => match samples {
                Some(samples) => {
                    fed += samples.len() as u64;
                    control.feed(samples);
                    if fed >= FRESH_EVERY && !talking {
                        control.reset();
                        fed = 0;
                    }
                }
                // The transport let go: listening is over.
                None => return,
            },
            event = heard_events.recv() => match event {
                Some(LiveEvent::Transcribed { result: Ok(words), .. }) => {
                    if let Some(rest) = heard(&words, &names)
                        && woken.send(Woken { rest }).is_err()
                    {
                        return;
                    }
                }
                // One utterance the recogniser could not make out is not the
                // end of listening; what it was is nobody's business.
                Some(LiveEvent::Transcribed { result: Err(e), .. }) => {
                    tracing::debug!(error = %e.message, "wake word: an utterance was not transcribed");
                }
                Some(LiveEvent::Started { .. }) => talking = true,
                Some(LiveEvent::Ended { .. }) => talking = false,
                // The recogniser has gone; `woken` closing says so.
                None => return,
            },
        }
    }
}

impl Service {
    /// Listen for `names` at the start of what is said: the chosen speech
    /// model behind voice mode's detector. Refuses, in a sentence, when
    /// listening is not set up, or when the chosen model is not on this
    /// computer: almost everything the microphone hears was not meant for the
    /// assistant, and none of it goes to a server to find the few words that
    /// were. Holds the recogniser for as long as it runs.
    pub async fn wake_open(self: &Arc<Self>, names: Vec<String>) -> CommandResult<WakeLink> {
        let vault = self.require_unlocked()?;
        let speech = blocking(move || Ok(vault.speech_access()?)).await?;
        if speech.config.transcriber.local_model().is_none() {
            return Err(CommandError::new(
                codes::INVALID,
                "The wake word listens only with a speech model on this computer, so nothing \
                 said near it is sent anywhere. Choose one in Settings → Models.",
            ));
        }
        // Every recogniser is told to expect these names from now on, this
        // one first among them.
        self.remember_wake_names(&names);
        let live = self.live_transcription(Tuning::CONVERSATION, Occasion::Conversation).await?;
        Ok(spawn(live, names))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_name_at_the_start_wakes_it_with_what_came_after() {
        let siri = names(&["Siri"]);
        let cases = [
            (
                "Siri, type I won't be available tomorrow. I can attend the meeting on Friday.",
                "type I won't be available tomorrow. I can attend the meeting on Friday.",
            ),
            ("siri what's on my calendar", "what's on my calendar"),
            ("Siri. Remind me at five.", "Remind me at five."),
            ("Hey Siri, how's the weather?", "how's the weather?"),
            ("OK, Siri — add milk", "add milk"),
            ("Siri.", ""),
            ("Hey, Siri!", ""),
        ];
        for (said, rest) in cases {
            assert_eq!(heard(said, &siri).as_deref(), Some(rest), "{said}");
        }
    }

    #[test]
    fn the_name_anywhere_else_is_somebody_talking_about_it() {
        let siri = names(&["Siri"]);
        for said in [
            "I asked Siri yesterday",
            "hey there Siri",
            "Sir, type this",
            "Series of tubes",
            "",
            "Hey",
        ] {
            assert_eq!(heard(said, &siri), None, "{said}");
        }
    }

    #[test]
    fn a_longer_name_may_be_heard_a_letter_off_and_a_short_one_may_not() {
        let karma = names(&["Karma"]);
        assert_eq!(heard("Carma, start a timer", &karma).as_deref(), Some("start a timer"));
        assert_eq!(heard("Karmas, hello", &karma).as_deref(), Some("hello"));
        assert_eq!(heard("Comma, hello", &karma), None, "two letters off");
        assert_eq!(heard("Seri, type hello", &names(&["Siri"])), None);
    }

    #[test]
    fn a_name_of_several_words_and_several_names_are_heard() {
        let both = names(&["Karma Yoga", "Jarvis"]);
        assert_eq!(heard("Karma Yoga, what's next?", &both).as_deref(), Some("what's next?"));
        assert_eq!(heard("hey Karma Yoga", &both).as_deref(), Some(""));
        assert_eq!(heard("Jarvis, lights", &both).as_deref(), Some("lights"));
        assert_eq!(heard("Karma, what's next", &both), None, "half of the name");
        // A name that says "hey" itself is heard with it.
        let hey = names(&["Hey Karma"]);
        assert_eq!(heard("Hey Karma, go", &hey).as_deref(), Some("go"));
        // Nothing to listen for, nothing heard.
        assert_eq!(heard("Siri, go", &names(&["", " ", "a"])), None);
    }

    #[test]
    fn one_letter_apart_is_one_edit() {
        assert!(within_one("karma", "carma"));
        assert!(within_one("karma", "karmas"));
        assert!(within_one("karma", "karm"));
        assert!(within_one("karma", "karma"));
        assert!(!within_one("karma", "comma"));
        assert!(!within_one("karma", "ka"));
    }
}
