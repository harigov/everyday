//! What a spoken conversation makes of the words in it: whether what was heard
//! is the assistant's own voice coming back, whether it means stop, and what to
//! say while the assistant works.

use karmayoga_core::meeting::words::tokens;

/// Whether `heard` is most likely the assistant's own `spoken` words coming
/// back through the microphone.
///
/// Words are compared as the meeting notes compare them: lower case, no
/// punctuation. At least 70% of what was heard must appear in `spoken`, in
/// order, and the match must be more than a coincidence of common words: three
/// words running together, or all of what was heard when that is three words
/// or more. So "wait", which a long reply is likely to contain somewhere, is
/// somebody talking, and a recogniser's slightly mangled copy of a sentence
/// just said is not.
pub fn sounds_like_echo(heard: &str, spoken: &str) -> bool {
    let heard = tokens(heard);
    let spoken = tokens(spoken);
    if heard.is_empty() || spoken.is_empty() {
        return false;
    }
    let (in_order, run) = overlap(&heard, &spoken);
    if in_order * 10 < heard.len() * 7 {
        return false;
    }
    run >= 3 || (heard.len() >= 3 && in_order == heard.len())
}

/// The longest run of `a` that is a subsequence of `b`, and the longest run
/// of words they have next to each other in both.
fn overlap(a: &[String], b: &[String]) -> (usize, usize) {
    let mut subsequence = vec![0usize; b.len() + 1];
    let mut contiguous = vec![0usize; b.len() + 1];
    let mut longest_run = 0;
    for word in a {
        let mut diagonal_subsequence = 0;
        let mut diagonal_contiguous = 0;
        for (j, other) in b.iter().enumerate() {
            let above_subsequence = subsequence[j + 1];
            let above_contiguous = contiguous[j + 1];
            if word == other {
                subsequence[j + 1] = diagonal_subsequence + 1;
                contiguous[j + 1] = diagonal_contiguous + 1;
                longest_run = longest_run.max(contiguous[j + 1]);
            } else {
                subsequence[j + 1] = subsequence[j + 1].max(subsequence[j]);
                contiguous[j + 1] = 0;
            }
            diagonal_subsequence = above_subsequence;
            diagonal_contiguous = above_contiguous;
        }
    }
    (subsequence[b.len()], longest_run)
}

/// What somebody says to stop the assistant, whole.
const STOP_WORDS: &[&str] = &[
    "stop",
    "stop it",
    "stop talking",
    "be quiet",
    "quiet",
    "shush",
    "hold on",
    "wait",
    "that's enough",
    "never mind",
    "cancel",
];

/// Whether `heard` is only somebody saying stop: the whole utterance, case,
/// punctuation and apostrophes aside.
pub fn is_stop_word(heard: &str) -> bool {
    let heard = plain(heard);
    !heard.is_empty() && STOP_WORDS.iter().any(|stop| plain(stop) == heard)
}

/// Lower case words with single spaces between them and no apostrophes.
fn plain(text: &str) -> String {
    let words: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '‘'))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    words.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the assistant is busy with when the session says a few words for
/// it: which list [`Phrasebook::deal`] picks them from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Errand {
    /// Working out what to say, with nothing to show for it yet: a model
    /// taking its time over its first words.
    Thinking,
    /// The web: a search, a page, the browser, the weather, a map.
    Web,
    Mail,
    Calendar,
    Tasks,
    /// The vault's own words: notes, the journal, documents, memory, a
    /// search of everything.
    Finding,
    /// Adding, changing, sending or removing something.
    Changing,
    Other,
}

/// The first words of the tools that change something.
const CHANGING: &[&str] = &[
    "accept",
    "add",
    "archive",
    "cancel",
    "complete",
    "confirm",
    "create",
    "decline",
    "delete",
    "draft",
    "draw",
    "edit",
    "finish",
    "follow",
    "forward",
    "generate",
    "mark",
    "move",
    "remember",
    "remove",
    "rename",
    "reply",
    "reschedule",
    "respond",
    "save",
    "schedule",
    "send",
    "set",
    "snooze",
    "start",
    "trash",
    "unfollow",
    "update",
    "write",
];

impl Errand {
    /// What running `tool` is. The assistant's tools are named verb then
    /// thing (`list_events`, `create_task`), so the first word says whether
    /// it changes something and the rest what it touches.
    pub fn of(tool: &str) -> Errand {
        let lower = tool.to_lowercase();
        let words: Vec<&str> = lower.split('_').collect();
        let has = |names: &[&str]| {
            words.iter().any(|w| names.iter().any(|n| w == n || w.strip_suffix('s') == Some(*n)))
        };
        if lower.starts_with("browser")
            || has(&["web", "image", "place", "weather", "direction", "research"])
        {
            Errand::Web
        } else if CHANGING.contains(&words[0]) {
            Errand::Changing
        } else if has(&["mail", "thread", "inbox", "message", "email"]) {
            Errand::Mail
        } else if has(&["event", "calendar", "invite"]) {
            Errand::Calendar
        } else if has(&["task", "project"]) {
            Errand::Tasks
        } else if has(&[
            "note",
            "search",
            "journal",
            "entry",
            "memory",
            "document",
            "file",
            "conversation",
        ]) {
            Errand::Finding
        } else {
            Errand::Other
        }
    }

    /// What may be said for it. Each is short -- it stands in front of the
    /// answer -- and promises nothing about how it will go: the call may be
    /// refused, fail, or need asking about ("Should I delete it?" can come
    /// straight after a [`Errand::Changing`] phrase), so none says what is
    /// being done to what.
    pub fn phrases(self) -> &'static [&'static str] {
        match self {
            Errand::Thinking => &[
                "Hmm, let me think.",
                "Let me see.",
                "Hmm, let me see.",
                "Okay, let me see.",
                "Give me a moment.",
            ],
            Errand::Web => &[
                "Let me look that up.",
                "Let me check online.",
                "One sec, I'll look it up.",
                "Let me search for that.",
            ],
            Errand::Mail => &[
                "Let me check your mail.",
                "Let me look through your email.",
                "One sec, checking your inbox.",
                "Let me look at your mail.",
            ],
            Errand::Calendar => &[
                "Let me check your calendar.",
                "Let me look at your schedule.",
                "One sec, checking your calendar.",
                "Let me see what's on your calendar.",
            ],
            Errand::Tasks => &[
                "Let me look at your tasks.",
                "Let me check your list.",
                "One sec, pulling up your tasks.",
            ],
            Errand::Finding => &[
                "Let me find that.",
                "Let me dig that up.",
                "Let me have a look.",
                "Let me look for that.",
            ],
            Errand::Changing => &["Okay, one sec.", "Sure, one moment.", "Alright, give me a sec."],
            Errand::Other => &["One moment.", "Give me a second.", "Just a sec.", "Let me see."],
        }
    }
}

/// Deals the phrases for each [`Errand`], every one of a list before any of
/// it again, in an order shuffled afresh each time round -- so the same few
/// words are not heard twice running, which is what made the old single
/// phrase per tool sound like a machine. One per session.
#[derive(Default)]
pub struct Phrasebook {
    /// What is left to deal for each errand, the next on top (last).
    decks: std::collections::HashMap<Errand, Vec<&'static str>>,
    /// The last phrase dealt, for whatever errand: two lists share some
    /// words ("Let me see."), and those are not said back to back either.
    last: Option<&'static str>,
}

impl Phrasebook {
    /// The next phrase for `errand`, dealt.
    pub fn deal(&mut self, errand: Errand) -> &'static str {
        self.top(errand);
        let phrase = self.decks.get_mut(&errand).and_then(Vec::pop).unwrap_or("One moment.");
        self.last = Some(phrase);
        phrase
    }

    /// What [`Phrasebook::deal`] would give for `errand` now, left on the
    /// deck: what to have the voice make ahead of time.
    pub fn peek(&mut self, errand: Errand) -> &'static str {
        self.top(errand)
    }

    /// The top of `errand`'s deck, shuffling a new one when it has run out,
    /// and never the phrase last dealt.
    fn top(&mut self, errand: Errand) -> &'static str {
        use rand::seq::SliceRandom;
        let last = self.last;
        let deck = self.decks.entry(errand).or_default();
        // The one card left is the words just said for another errand: it
        // goes to the bottom of the next round, which is dealt on top of it.
        if deck.is_empty() || (deck.len() == 1 && Some(deck[0]) == last) {
            let held = deck.pop();
            let mut fresh: Vec<&'static str> =
                errand.phrases().iter().copied().filter(|p| Some(*p) != held).collect();
            fresh.shuffle(&mut rand::rng());
            deck.extend(held);
            deck.extend(fresh);
        }
        let top = deck.len() - 1;
        if top > 0 && Some(deck[top]) == last {
            deck.swap(0, top);
        }
        deck[top]
    }
}

/// What leads updates from background work -- a watch, a project -- when
/// they are said in the session's own voice: it says they are updates, so
/// that "the build passed" is not taken for the end of the reply before it.
/// `count` is how many are being said together.
pub fn update_lead(count: usize) -> &'static str {
    match count {
        0 | 1 => "An update:",
        2 => "A couple of updates:",
        _ => "A few updates:",
    }
}

/// What is said after the updates that were read out, when there were more
/// than a few: how many were not, so that none is dropped without a word.
pub fn more_updates(count: usize) -> String {
    match count {
        1 => "And one more.".to_string(),
        n => format!("And {n} more."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- echo and stop ----------------------------------------------------

    const SPOKEN: &str = "The weather today is mostly sunny with a high of seventy degrees and a light breeze from the west.";

    #[test]
    fn what_was_just_said_coming_back_is_an_echo() {
        assert!(sounds_like_echo("mostly sunny with a high of seventy", SPOKEN));
        assert!(sounds_like_echo("Mostly sunny, with a high of seventy!", SPOKEN));
        // A recogniser drops and mangles a little.
        assert!(sounds_like_echo("high of seventy degrees but", SPOKEN));
    }

    #[test]
    fn a_short_interruption_sharing_a_word_is_not() {
        assert!(!sounds_like_echo("wait", "Wait for it. The weather today is mostly sunny."));
        assert!(!sounds_like_echo("sunny", SPOKEN));
        assert!(!sounds_like_echo("sunny breeze", SPOKEN));
        assert!(!sounds_like_echo("no that is not right", SPOKEN));
    }

    #[test]
    fn three_words_in_order_but_apart_are_an_echo_when_they_are_all_of_it() {
        assert!(sounds_like_echo("sunny high west", SPOKEN));
        assert!(!sounds_like_echo("sunny high east", SPOKEN));
    }

    #[test]
    fn mostly_other_words_are_not_an_echo() {
        assert!(!sounds_like_echo("tell me something about high of seventy", SPOKEN));
    }

    #[test]
    fn order_matters() {
        assert!(!sounds_like_echo("west the from breeze light", SPOKEN));
    }

    #[test]
    fn nothing_heard_or_nothing_said_is_not_an_echo() {
        assert!(!sounds_like_echo("", SPOKEN));
        assert!(!sounds_like_echo("  ...  ", SPOKEN));
        assert!(!sounds_like_echo("sunny with a high", ""));
    }

    #[test]
    fn stop_words_are_the_whole_utterance() {
        for yes in [
            "stop",
            "Stop!",
            "Stop it.",
            "stop talking",
            "Be quiet",
            "quiet",
            "Shush!",
            "hold on",
            "Wait",
            "That's enough",
            "that\u{2019}s enough.",
            "thats enough",
            "Never mind.",
            "CANCEL",
        ] {
            assert!(is_stop_word(yes), "{yes}");
        }
        for no in [
            "",
            "...",
            "stop the music",
            "wait a minute",
            "don't stop",
            "cancel my meeting",
            "what is the weather",
        ] {
            assert!(!is_stop_word(no), "{no}");
        }
    }

    // ---- updates ------------------------------------------------------------

    #[test]
    fn updates_are_led_by_how_many_there_are() {
        assert_eq!(update_lead(1), "An update:");
        assert_eq!(update_lead(2), "A couple of updates:");
        assert_eq!(update_lead(3), "A few updates:");
        assert_eq!(update_lead(8), "A few updates:");
        // Nothing to say never gets here, but is not nonsense if it does.
        assert_eq!(update_lead(0), "An update:");
    }

    #[test]
    fn updates_left_unread_are_counted() {
        assert_eq!(more_updates(1), "And one more.");
        assert_eq!(more_updates(2), "And 2 more.");
        assert_eq!(more_updates(12), "And 12 more.");
    }

    // ---- what is said while it works ---------------------------------------

    #[test]
    fn a_tool_is_known_by_what_it_does_and_what_it_touches() {
        let cases: &[(Errand, &[&str])] = &[
            (
                Errand::Web,
                &[
                    "web_search",
                    "read_web_page",
                    "browser_open",
                    "browser_type",
                    "search_images",
                    "search_places",
                    "get_weather",
                    "get_directions",
                    "start_research",
                ],
            ),
            (Errand::Mail, &["search_mail", "list_threads", "read_thread", "summarize_thread"]),
            (Errand::Calendar, &["list_events", "list_calendars", "get_event"]),
            (Errand::Tasks, &["list_tasks", "list_projects"]),
            (
                Errand::Finding,
                &[
                    "list_notes",
                    "get_note",
                    "search",
                    "search_conversations",
                    "read_own_journal",
                    "search_documents",
                    "read_file",
                ],
            ),
            (
                Errand::Changing,
                &[
                    "create_event",
                    "update_task",
                    "create_task",
                    "archive_thread",
                    "draft_reply",
                    "respond_to_invite",
                    "remember",
                    "write_own_journal",
                    "follow_podcast",
                    "trash_files",
                ],
            ),
            (Errand::Other, &["overview", "list_trackers", "spending_summary", "read_aloud", ""]),
        ];
        for (errand, tools) in cases {
            for tool in *tools {
                assert_eq!(Errand::of(tool), *errand, "{tool}");
            }
        }
    }

    const EVERY: [Errand; 8] = [
        Errand::Thinking,
        Errand::Web,
        Errand::Mail,
        Errand::Calendar,
        Errand::Tasks,
        Errand::Finding,
        Errand::Changing,
        Errand::Other,
    ];

    #[test]
    fn every_phrase_is_short_and_plain() {
        for errand in EVERY {
            let phrases = errand.phrases();
            assert!(phrases.len() >= 3, "{errand:?} has too few to vary");
            for phrase in phrases {
                assert!(phrase.split_whitespace().count() <= 7, "{phrase}");
                assert!(phrase.ends_with('.'), "{phrase}");
                // Said as written: nothing the voice would read as a mark.
                assert!(!phrase.contains(['{', '}', '*', '#']), "{phrase}");
            }
        }
    }

    #[test]
    fn a_list_is_dealt_whole_before_any_of_it_comes_round_again() {
        let mut book = Phrasebook::default();
        for errand in EVERY {
            let phrases = errand.phrases();
            let mut dealt: Vec<&str> = (0..phrases.len()).map(|_| book.deal(errand)).collect();
            dealt.sort_unstable();
            let mut all = phrases.to_vec();
            all.sort_unstable();
            assert_eq!(dealt, all, "{errand:?}");
        }
    }

    #[test]
    fn the_same_words_are_never_said_twice_running() {
        let mut book = Phrasebook::default();
        let mut last = "";
        // Round after round, and across lists that share words.
        for round in 0..200 {
            let errand = if round % 3 == 0 { Errand::Other } else { Errand::Thinking };
            let phrase = book.deal(errand);
            assert_ne!(phrase, last, "round {round}");
            last = phrase;
        }
    }

    #[test]
    fn peeking_shows_what_is_dealt_next() {
        let mut book = Phrasebook::default();
        for _ in 0..20 {
            let next = book.peek(Errand::Web);
            assert_eq!(book.peek(Errand::Web), next, "peeking deals nothing");
            assert_eq!(book.deal(Errand::Web), next);
        }
    }
}
