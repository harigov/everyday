//! What a spoken conversation makes of the words in it: whether what was heard
//! is the assistant's own voice coming back, whether it means stop or "I'm
//! done", whether it is a murmur or a filler rather than something meant,
//! what to type of a dictation, and what to say while the assistant works.

use jatayu_core::meeting::words::tokens;

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

// ---- what is not meant ------------------------------------------------------

/// Whether `heard` is a piece of what the assistant said, one or two words of
/// it, coming back through the microphone: every word of it is in `spoken`, in
/// order.
///
/// [`sounds_like_echo`] wants three words running together, or all of three
/// or more, so that "wait" is somebody talking, and that is right for it. But
/// a recogniser that hears the end of a sentence in the room writes only
/// "added it", and a reply cut short by that is as bad as one cut by a
/// person. So this is for the caller that knows better than the words do: one
/// that gives it only what began while the assistant's own audio was playing,
/// and only what was said a few seconds ago, and never lets it near a stop
/// word. Nor does it ever take a word that answers (yes, no, sure, okay).
pub fn is_echo_fragment(heard: &str, spoken: &str) -> bool {
    let heard = tokens(heard);
    if heard.is_empty() || heard.len() > 2 {
        return false;
    }
    // A word that answers is an answer, whatever was said: "yes" is in as
    // many replies as it is said to.
    if heard.iter().any(|word| ANSWERS.contains(&word.as_str())) {
        return false;
    }
    let spoken = tokens(spoken);
    let mut rest = spoken.iter();
    heard.iter().all(|word| rest.any(|said| said == word))
}

/// Words that answer a question, and so are never taken for the room.
const ANSWERS: &[&str] = &["yes", "no", "yeah", "yep", "nope", "sure", "okay", "ok"];

/// What a listener murmurs without meaning to take the floor. Not "yes",
/// "yeah", "okay" or "sure": those answer a question, and are as often said to
/// one.
const BACKCHANNELS: &[&str] = &[
    "mm", "mhm", "mhmm", "mmhmm", "mm hmm", "mm hm", "uh huh", "hmm", "right", "i see", "got it",
    "gotcha",
];

/// Sounds with no meaning in them, which a recogniser writes for a breath, a
/// chair or a thought.
const FILLERS: &[&str] = &["um", "uh", "er", "erm", "ah", "hmm", "hm", "mm", "oh"];

/// Whether `heard` is only a listener's murmur ("mm-hmm", "uh-huh", "right",
/// "got it"), however many of them: it is not an answer or a request, and
/// said while the assistant is talking, it is no reason to stop.
pub fn is_backchannel(heard: &str) -> bool {
    made_of(heard, BACKCHANNELS)
}

/// Whether `heard` is nothing but fillers ("um", "hmm", "oh"): nothing was
/// meant by it, and nothing is done about it.
pub fn is_filler(heard: &str) -> bool {
    made_of(heard, FILLERS)
}

/// Whether `heard` is made entirely of `phrases`, any number of them, one
/// after another, with nothing left over. The words are compared as
/// [`plain`] has them, so "Mm-hmm." is "mm hmm".
fn made_of(heard: &str, phrases: &[&str]) -> bool {
    let heard = plain(heard);
    let words: Vec<&str> = heard.split(' ').filter(|w| !w.is_empty()).collect();
    let phrases: Vec<Vec<String>> =
        phrases.iter().map(|p| plain(p).split(' ').map(str::to_string).collect()).collect();
    !words.is_empty() && made_of_words(&words, &phrases)
}

/// [`made_of`], over words. Every way of cutting it up counts: "mm hmm" is a
/// phrase of its own and two fillers. Worked out a position at a time -- which
/// positions can be reached by whole phrases from the start -- and not by
/// trying each cut in turn, which doubles at every "mm hmm" and would hold a
/// session up for ever on a recogniser's loop of them.
fn made_of_words(words: &[&str], phrases: &[Vec<String>]) -> bool {
    let mut reached = vec![false; words.len() + 1];
    reached[0] = true;
    for at in 0..words.len() {
        if !reached[at] {
            continue;
        }
        for phrase in phrases {
            let end = at + phrase.len();
            if end <= words.len() && phrase.iter().zip(&words[at..end]).all(|(a, b)| a == b) {
                reached[end] = true;
            }
        }
    }
    reached[words.len()]
}

// ---- saying it is over ------------------------------------------------------

/// What somebody says, whole, to say they have nothing more for now. "That's
/// it" is not here, nor "no" or "no thanks": each is as often the answer to
/// a question as a way of leaving, and the model, which has the question in
/// front of it, is the one to tell.
const CLOSINGS: &[&str] = &[
    "that's all",
    "that is all",
    "that'll be all",
    "that will be all",
    "nothing else",
    "nothing more",
    "i don't have anything else",
    "i don't need anything else",
    "i have nothing else",
    "that's all i have",
    "that's all i need",
    "that's all for now",
    "i'm done",
    "i am done",
    "i'm all done",
    "we're done",
    "all done",
    "i'm finished",
    "bye",
    "bye bye",
    "goodbye",
    "good night",
    "goodnight",
    "see you",
    "see you later",
    "talk to you later",
    "talk later",
    "end voice mode",
    "end the conversation",
    "stop listening",
    "go to sleep",
];

/// Words said before the point, which are manners and no part of it. The
/// longer of two that begin alike come first.
const COURTESY_BEFORE: &[&str] = &[
    "thanks a lot",
    "thank you",
    "that's great",
    "all right",
    "alright",
    "okay",
    "ok",
    "great",
    "cool",
    "perfect",
    "thanks",
    "nope",
    "no",
    "oh",
];

/// Words said after the point, likewise.
const COURTESY_AFTER: &[&str] = &["thank you", "thanks", "for now", "for today", "goodbye", "bye"];

/// Whether `heard`, the whole of what somebody said, is only a way of saying
/// "I'm done for now": "that's all", "nothing else", "I don't have anything
/// else", "bye", "talk to you later", with "no", "okay", "thanks" and the like
/// around it. Whoever is talking to them takes it for the goodbye it is,
/// without asking a model.
///
/// Courtesy is taken off the ends one word at a time and what is left looked
/// for after each, so "thanks, bye" is a goodbye and "thank you" and "no
/// thanks" -- which are nothing but courtesy, with nothing under it -- are not.
pub fn is_closing(heard: &str) -> bool {
    closes_with(heard, &[], CLOSINGS)
}

/// Whether `heard` is only thanks, a goodbye or an acknowledgement, and
/// asks for nothing: what a person says back to a goodbye, which is no reason
/// to take the goodbye back.
pub fn is_courtesy(heard: &str) -> bool {
    const COURTESY: &[&str] = &[
        "thanks",
        "thanks a lot",
        "thanks so much",
        "thank you",
        "thank you so much",
        "thank you very much",
        "bye",
        "goodbye",
        "you too",
        "same to you",
        "have a good one",
        "have a good day",
        "take care",
        "ok",
        "okay",
        "alright",
        "great",
        "cool",
        "perfect",
        "sounds good",
        "will do",
        "got it",
    ];
    made_of(heard, COURTESY) || is_closing(heard) || is_backchannel(heard)
}

/// What somebody says to say goodbye, as a part of something else.
const GOODBYE_WORDS: &[&str] = &[
    "bye",
    "bye bye",
    "goodbye",
    "good night",
    "goodnight",
    "see you",
    "see you later",
    "talk to you later",
    "talk later",
];

/// Whether `heard` has a goodbye in it, anywhere. For what is already known to
/// be only courtesy round a way of saying it is over ("that's all, bye"): not
/// for anything else, which may be somebody dictating "goodbye" into a letter.
pub fn has_goodbye(heard: &str) -> bool {
    let heard = format!(" {} ", plain(heard));
    GOODBYE_WORDS.iter().any(|goodbye| heard.contains(&format!(" {} ", plain(goodbye))))
}

/// What somebody says to end a dictation, whole. Some of these are also ways
/// of ending the conversation; ending the dictation is what they do first.
const END_OF_DICTATION: &[&str] = &[
    "stop dictating",
    "stop dictation",
    "end dictation",
    "end the dictation",
    "done dictating",
    "finish dictating",
    "i'm done dictating",
    "that's all",
    "that's it",
    "i'm done",
    "done",
    "finished",
    "stop",
];

/// Whether `heard` is somebody saying they have finished dictating. The
/// courtesy that goes with "stop dictating" ("okay, stop dictating now",
/// "thanks, that's all") is no part of it.
pub fn is_end_of_dictation(heard: &str) -> bool {
    closes_with(heard, &["please", "now"], END_OF_DICTATION)
}

/// Whether what is left of `heard`, once courtesy has been taken off its ends
/// as far as it will come, is one of `cores`. `after` is more courtesy that
/// may stand at the end.
fn closes_with(heard: &str, after: &[&str], cores: &[&str]) -> bool {
    let cores: Vec<String> = cores.iter().map(|c| plain(c)).collect();
    let mut rest = plain(heard);
    loop {
        if rest.is_empty() {
            return false;
        }
        if cores.contains(&rest) {
            return true;
        }
        let Some(next) = COURTESY_BEFORE
            .iter()
            .find_map(|phrase| without_leading(&rest, &plain(phrase)))
            .or_else(|| {
                COURTESY_AFTER
                    .iter()
                    .chain(after)
                    .find_map(|phrase| without_trailing(&rest, &plain(phrase)))
            })
        else {
            return false;
        };
        rest = next;
    }
}

/// `rest` without `phrase` at its start, whole words only.
fn without_leading(rest: &str, phrase: &str) -> Option<String> {
    if rest == phrase {
        return Some(String::new());
    }
    rest.strip_prefix(phrase)?.strip_prefix(' ').map(str::to_string)
}

/// `rest` without `phrase` at its end, whole words only.
fn without_trailing(rest: &str, phrase: &str) -> Option<String> {
    if rest == phrase {
        return Some(String::new());
    }
    rest.strip_suffix(phrase)?.strip_suffix(' ').map(str::to_string)
}

// ---- dictation ------------------------------------------------------------------

/// Fillers a dictation leaves out, as words: what a person says while they
/// think, which was never meant for the page.
const DICTATION_FILLERS: &[&str] = &["um", "umm", "uhm", "uh", "uhh", "er", "erm"];

/// A word of a dictation, or the line break somebody asked for.
enum Dictated {
    Word(String),
    Break(&'static str),
}

/// What to type for one utterance of a dictation, or nothing when nothing
/// is left of it.
///
/// - Fillers ("um", "uh", "er", "erm") are left out as words, with the comma
///   the recogniser put after them.
/// - "new line" is a line break and "new paragraph" or "next paragraph" is a
///   blank line, whatever the recogniser put round the command ("New
///   paragraph." is the break and no full stop).
/// - The first letter after a break is a capital: what is typed begins a line
///   when `continues_line` is false -- nothing has been typed yet, or what
///   was ended in a line break -- and then its first letter is capital too.
/// - When it continues the line, and begins with a word, it begins with a
///   space, so one utterance runs into the last as a person would type it.
pub fn dictated(heard: &str, continues_line: bool) -> Option<String> {
    let words: Vec<&str> = heard.split_whitespace().collect();
    let bare =
        |word: &str| -> String { word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase() };
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < words.len() {
        let this = bare(words[at]);
        let next = words.get(at + 1).map(|w| bare(w));
        match (this.as_str(), next.as_deref()) {
            (filler, _) if DICTATION_FILLERS.contains(&filler) => at += 1,
            ("new", Some("line")) => {
                pieces.push(Dictated::Break("\n"));
                at += 2;
            }
            ("new" | "next", Some("paragraph")) => {
                pieces.push(Dictated::Break("\n\n"));
                at += 2;
            }
            _ => {
                pieces.push(Dictated::Word(words[at].to_string()));
                at += 1;
            }
        }
    }
    let mut out = String::new();
    let mut at_start_of_line = !continues_line;
    for piece in pieces {
        match piece {
            Dictated::Break(lines) => {
                out.push_str(lines);
                at_start_of_line = true;
            }
            Dictated::Word(word) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push(' ');
                }
                if at_start_of_line {
                    out.push_str(&capitalised(&word));
                } else {
                    out.push_str(&word);
                }
                at_start_of_line = false;
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    if continues_line && !out.starts_with(['\n', ',', '.', ';', ':', '!', '?', ')', ']', '}', '…'])
    {
        out.insert(0, ' ');
    }
    Some(out)
}

/// `word` with its first letter a capital, the quote or bracket before it, if
/// there is one, left where it is.
fn capitalised(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut done = false;
    for c in word.chars() {
        if !done && c.is_alphanumeric() {
            out.extend(c.to_uppercase());
            done = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// What is said when the session ends the conversation itself, because the
/// person said they were done: one of a few, in the order the phrasebook
/// deals them. Nothing here begins "Hmm", and nothing says "sec", which the
/// voice that was tried said wrongly.
const GOODBYES: &[&str] = &[
    "Okay, talk to you later.",
    "Alright, I'm here if you need me.",
    "Okay, bye for now.",
    "Sure, talk soon.",
    "Okay, take care.",
];

/// What is said when a dictation is over and everything of it is typed.
const TYPED: &[&str] = &["Okay, that's typed.", "Done.", "Okay, done.", "All typed up."];

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
    /// The session ending the conversation because the person said they were
    /// done ([`is_closing`]). Never a tool's: nothing runs for it.
    Goodbye,
    /// The session saying a dictation is over and typed ([`is_end_of_dictation`]).
    Typed,
}

/// The first words of the tools that change something.
const CHANGING: &[&str] = &[
    "accept",
    "add",
    "archive",
    "cancel",
    "categorize",
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
    "label",
    "log",
    "mark",
    "move",
    "organize",
    "remember",
    "remove",
    "rename",
    "reply",
    "reschedule",
    "respond",
    "rewrite",
    "run",
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
            Errand::Goodbye => GOODBYES,
            Errand::Typed => TYPED,
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

    // ---- what is not meant ----------------------------------------------------

    #[test]
    fn one_or_two_words_of_what_was_just_said_are_a_fragment_of_it() {
        let spoken = "Okay, I've added it to your list for tomorrow.";
        assert!(is_echo_fragment("added it", spoken));
        assert!(is_echo_fragment("Added it.", spoken));
        assert!(is_echo_fragment("tomorrow", spoken));
        // In order, though not necessarily side by side.
        assert!(is_echo_fragment("added tomorrow", spoken));
        assert!(!is_echo_fragment("tomorrow added", spoken));
        // One word that is not there, or one of two.
        assert!(!is_echo_fragment("deleted it", spoken));
        assert!(!is_echo_fragment("yes", spoken));
        // A word that answers is never the room, whatever the reply held.
        assert!(!is_echo_fragment("yes", "Yes, I can do that."));
        assert!(!is_echo_fragment("No.", "No, I did not."));
        assert!(!is_echo_fragment("sure", "Sure, I'll do it."));
        assert!(!is_echo_fragment("okay thanks", "Okay, thanks for waiting."));
        // But the rest of an answer is: it is for the session to say when.
        assert!(is_echo_fragment("Friday", "Thursday or Friday?"));
        // Three words are for [`sounds_like_echo`] to say.
        assert!(!is_echo_fragment("added it to", spoken));
        assert!(!is_echo_fragment("", spoken));
        assert!(!is_echo_fragment("added", ""));
    }

    #[test]
    fn a_murmur_is_only_a_murmur() {
        for yes in [
            "Mm-hmm.",
            "mm hmm",
            "Mhm.",
            "Uh-huh.",
            "uh huh",
            "Hmm.",
            "Right.",
            "I see.",
            "Got it.",
            "Gotcha!",
            // More than one.
            "Mm-hmm, mm-hmm.",
            "Right, right.",
        ] {
            assert!(is_backchannel(yes), "{yes}");
        }
        // These answer a question, or are one.
        for no in [
            "",
            "yes",
            "yeah",
            "okay",
            "sure",
            "no",
            "right now",
            "right, stop that",
            "I see what you mean",
            "mm-hmm what about tomorrow",
        ] {
            assert!(!is_backchannel(no), "{no}");
        }
    }

    #[test]
    fn a_long_run_of_murmurs_is_worked_out_at_once() {
        // A recogniser looping on one: "mm hmm" is a phrase and two fillers,
        // which tried every way is two to the power of the repeats.
        let looped = "Mm-hmm. ".repeat(60);
        let started = std::time::Instant::now();
        assert!(is_backchannel(&looped));
        assert!(is_filler(&looped));
        assert!(!is_backchannel(&format!("{looped}Thank you.")));
        assert!(!is_filler(&format!("{looped}Thank you.")));
        assert!(!is_courtesy(&format!("{looped}and what about tomorrow")));
        assert!(!is_closing(&format!("{looped}that's all about it")));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_goodbye_is_found_where_it_is_said() {
        for yes in [
            "That's all, bye.",
            "Okay, I'm done. Goodbye!",
            "Stop dictating, good night.",
            "That's all. Talk to you later.",
            "bye",
            "See you",
        ] {
            assert!(has_goodbye(yes), "{yes}");
        }
        for no in ["", "That's all.", "Stop dictating.", "I'm done, thanks.", "side by side"] {
            assert!(!has_goodbye(no), "{no}");
        }
        // And what ends a dictation with one in it is still what ends it.
        assert!(is_end_of_dictation("That's all, bye."));
    }

    #[test]
    fn a_filler_is_nothing_but_fillers() {
        for yes in ["Um.", "uh", "Er, um...", "erm", "Ah.", "Hmm.", "hm", "mm", "Oh.", "um uh hmm"]
        {
            assert!(is_filler(yes), "{yes}");
        }
        for no in ["", "...", "um yes", "oh no", "uh huh", "hmm that is interesting", "okay"] {
            assert!(!is_filler(no), "{no}");
        }
    }

    // ---- saying it is over ----------------------------------------------------

    #[test]
    fn a_plain_goodbye_is_a_closing() {
        for yes in [
            "That's all.",
            "that is all",
            "That'll be all.",
            "that will be all",
            "Nothing else.",
            "nothing more",
            "I don't have anything else.",
            "I don\u{2019}t need anything else",
            "I have nothing else.",
            "That's all I have.",
            "That's all I need",
            "That's all for now.",
            "I'm done.",
            "I am done",
            "I'm all done.",
            "We're done.",
            "All done.",
            "I'm finished.",
            "Bye.",
            "Bye bye!",
            "Goodbye.",
            "Good night.",
            "Goodnight",
            "See you.",
            "See you later.",
            "Talk to you later.",
            "Talk later",
            "End voice mode.",
            "End the conversation.",
            "Stop listening.",
            "Go to sleep.",
        ] {
            assert!(is_closing(yes), "{yes}");
        }
    }

    #[test]
    fn courtesy_round_a_goodbye_is_no_part_of_it() {
        for yes in [
            "No, that's all.",
            "Nope, that's all I have.",
            "Okay, that's all.",
            "OK I don't have anything else",
            "Alright, nothing else.",
            "Great, that's all for now.",
            "Cool. Thanks, that's all.",
            "That's all, thanks.",
            "That's all, thank you.",
            "That's all for today.",
            "Nothing else for now.",
            "Oh, that's all, thank you.",
            "Perfect, thanks a lot, that's all.",
            "Okay, bye.",
            "Thanks, bye!",
            "Thank you, goodbye.",
            "Okay thanks bye bye",
            "Alright, I'm done, thanks.",
            "That's great, thanks. Talk to you later.",
        ] {
            assert!(is_closing(yes), "{yes}");
        }
    }

    #[test]
    fn what_is_as_often_an_answer_is_left_to_the_model() {
        for no in [
            "",
            "...",
            "that's it",
            "That's it!",
            "no",
            "No.",
            "no thanks",
            "No, thanks.",
            "thank you",
            "Thanks!",
            "okay",
            "yes",
            "oh",
            // More than a goodbye.
            "that's all I have on my calendar",
            "that's all wrong",
            "I'm done with the report, can you send it",
            "bye the way, what time is it",
            "nothing else matters",
            "see you at the meeting tomorrow",
            "I don't have anything else to add to that email",
            "tell me what else there is",
        ] {
            assert!(!is_closing(no), "{no}");
        }
    }

    #[test]
    fn only_manners_are_courtesy() {
        for yes in [
            "Thanks.",
            "Thank you!",
            "Thank you so much.",
            "Bye.",
            "Goodbye!",
            "You too.",
            "Same to you.",
            "Okay.",
            "Ok, thanks.",
            "Alright.",
            "Great.",
            "Cool, thanks.",
            "Perfect.",
            "Sounds good.",
            "Will do.",
            "Got it.",
            "Okay, thanks, bye.",
            "Thanks, you too.",
            // Anything that is a closing or a murmur.
            "That's all.",
            "Thanks, that's all.",
            "I don't have anything else.",
            "Mm-hmm.",
            "Uh-huh.",
            "Right.",
        ] {
            assert!(is_courtesy(yes), "{yes}");
        }
        for no in [
            "",
            "no",
            "yes",
            "actually one more thing",
            "thanks, but what about tomorrow",
            "okay, what is the weather",
            "great, now read it to me",
            "that's it",
            "stop",
        ] {
            assert!(!is_courtesy(no), "{no}");
        }
    }

    #[test]
    fn a_closing_is_not_a_stop_word() {
        // The two are told apart by who asks: stopping leaves the
        // conversation open.
        for word in ["stop", "wait", "never mind", "that's enough"] {
            assert!(is_stop_word(word) && !is_closing(word), "{word}");
        }
        assert!(!is_stop_word("that's all") && is_closing("that's all"));
    }

    // ---- dictation ----------------------------------------------------------------

    #[test]
    fn stopping_a_dictation_takes_its_own_words() {
        for yes in [
            "Stop dictating.",
            "stop dictation",
            "End dictation.",
            "End the dictation.",
            "Done dictating.",
            "Finish dictating",
            "I'm done dictating.",
            "That's all.",
            "That's it.",
            "I'm done.",
            "Done.",
            "Finished.",
            "Stop.",
            // With manners round them.
            "Okay, stop dictating.",
            "Okay, stop dictating now.",
            "Stop dictating, please.",
            "Thanks, that's all.",
            "That's it, thank you.",
            "Alright, I'm done.",
            "No, stop.",
        ] {
            assert!(is_end_of_dictation(yes), "{yes}");
        }
        for no in [
            "",
            "thanks",
            "okay",
            "bye",
            "I'm done with the first part",
            "stop the music",
            "that's all for the first paragraph",
            "the work is done",
            "we are finished",
        ] {
            assert!(!is_end_of_dictation(no), "{no}");
        }
    }

    #[test]
    fn what_is_dictated_is_typed_as_it_is_said() {
        // Nothing before it: it begins a line.
        assert_eq!(dictated("Dear Bob,", false).as_deref(), Some("Dear Bob,"));
        // Carried on from what was typed: a space between them.
        assert_eq!(
            dictated("I won't be in tomorrow.", true).as_deref(),
            Some(" I won't be in tomorrow.")
        );
        // Not capitalised for the recogniser, which knows where a sentence begins.
        assert_eq!(dictated("and then some", true).as_deref(), Some(" and then some"));
        // But a line begins with a capital.
        assert_eq!(dictated("and then some", false).as_deref(), Some("And then some"));
    }

    #[test]
    fn fillers_are_left_out_with_their_commas() {
        assert_eq!(dictated("Um, hello there.", false).as_deref(), Some("Hello there."));
        assert_eq!(
            dictated("I think, uh, we should go.", true).as_deref(),
            Some(" I think, we should go.")
        );
        assert_eq!(
            dictated("It is er, quite late erm now", true).as_deref(),
            Some(" It is quite late now")
        );
        assert_eq!(dictated("Uh... um...", true), None);
        assert_eq!(dictated("um", false), None);
        // A word that only looks like one.
        assert_eq!(
            dictated("The umbrella is here", true).as_deref(),
            Some(" The umbrella is here")
        );
    }

    #[test]
    fn a_spoken_new_line_or_paragraph_is_a_line_break() {
        assert_eq!(
            dictated("Dear Bob, new line, thanks for the update.", false).as_deref(),
            Some("Dear Bob,\nThanks for the update.")
        );
        assert_eq!(
            dictated("That is all for now. New paragraph. Best wishes.", true).as_deref(),
            Some(" That is all for now.\n\nBest wishes.")
        );
        assert_eq!(
            dictated("First point next paragraph second point", false).as_deref(),
            Some("First point\n\nSecond point")
        );
        // Alone, it is the break and nothing else, and no space is put in front.
        assert_eq!(dictated("New paragraph.", true).as_deref(), Some("\n\n"));
        assert_eq!(dictated("NEW LINE", true).as_deref(), Some("\n"));
        assert_eq!(dictated("new line, new line", false).as_deref(), Some("\n\n"));
        // What follows a break that ended the last utterance is a new line.
        assert_eq!(dictated("more text", false).as_deref(), Some("More text"));
        // One word of the command is only a word.
        assert_eq!(dictated("a new idea", true).as_deref(), Some(" a new idea"));
        assert_eq!(dictated("the next one", true).as_deref(), Some(" the next one"));
    }

    #[test]
    fn nothing_said_types_nothing() {
        assert_eq!(dictated("", true), None);
        assert_eq!(dictated("   ", false), None);
        assert_eq!(dictated("um, uh", true), None);
    }

    #[test]
    fn what_the_session_says_for_itself_is_short_and_plain() {
        // The goodbyes and the "typed" are among the lists the phrasebook deals
        // (see below); these are the words the voice said badly.
        for errand in [Errand::Goodbye, Errand::Typed] {
            for phrase in errand.phrases() {
                assert!(!phrase.to_lowercase().starts_with("hmm"), "{phrase}");
                assert!(!phrase.to_lowercase().contains("sec"), "{phrase}");
            }
        }
        // A goodbye is not itself taken for the person's, if it comes back.
        for phrase in Errand::Goodbye.phrases() {
            assert!(!is_stop_word(phrase), "{phrase}");
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
                    "log_meal",
                    "log_reading",
                    "categorize_transactions",
                    "organize_files",
                    "label_thread",
                    "run_routine",
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

    const EVERY: [Errand; 10] = [
        Errand::Thinking,
        Errand::Web,
        Errand::Mail,
        Errand::Calendar,
        Errand::Tasks,
        Errand::Finding,
        Errand::Changing,
        Errand::Other,
        Errand::Goodbye,
        Errand::Typed,
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
