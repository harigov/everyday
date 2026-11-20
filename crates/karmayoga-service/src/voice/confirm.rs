//! Approving something out loud.
//!
//! **What the person asks for out loud is approved by asking.** Adding,
//! finishing and moving tasks, scheduling, moving and cancelling events,
//! changing what is on a shelf and writing notes are the things somebody talks
//! to an assistant to get done, and a card to click after saying "cancel my
//! three o'clock" undoes the point of saying it. So in a spoken turn at this
//! computer the agent's gate does not stop [`CHANGED_BY_VOICE`]'s calls --
//! as long as the person began saying it while nothing of the assistant's own
//! was audible (an echo of a reply, or of an article read aloud, must never be
//! the yes), and, once a stranger's words have come into the conversation (a
//! mail, a page, an invitation's description), only when what they said asked
//! for that kind of change ([`request_asks_for`]) and, for the calendar, the
//! guests are told nothing but a new time or a cancellation. Anything else
//! may be a stranger's instruction, and it is asked about. A call through a
//! chat always asks: a phone is the likelier thing to be lost or borrowed.
//!
//! Three kinds of confirmation may then be answered by voice: a web search
//! the assistant wants to make after it has read mail, a page it wants to
//! open at an address it composed itself, and a delete of one of those
//! things. The first two exist because the words or the address could carry
//! something out, so they may be answered aloud only when all of it can be
//! said: a search too long to read, or an address with a query, a login,
//! escapes or anything else a listener could not judge, waits for the card,
//! where it is shown in full. A delete is asked by the name of what goes.
//! Everything else -- a mail to somebody, an invitation sent after a
//! stranger's words were read, a write of anything else -- waits for a click
//! on the card already in the thread, because a voice is easy to forge and
//! easy to mishear, and an echo of the assistant's own speech must never be
//! able to approve anything.
//!
//! What counts as an answer is deliberately narrow too. A confirmation is
//! given by a whole utterance made of nothing but agreement ("yeah, go
//! ahead"), never by a sentence that happens to contain a yes; and a refusal
//! anywhere in what was said, however it is dressed, is never read as consent.

/// What somebody said to a yes-or-no question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenAnswer {
    Yes,
    No,
    Unclear,
}

/// The most words a whole answer may be. A longer utterance is somebody
/// saying something else.
const MAX_ANSWER_WORDS: usize = 5;

/// The most separate phrases of agreement in one answer: "yes, please do".
const MAX_AGREEMENTS: usize = 3;

/// Phrases that are agreement on their own. A whole utterance made of these
/// and nothing else is a yes.
const AGREEMENTS: &[&[&str]] = &[
    &["yes"],
    &["yeah"],
    &["yep"],
    &["yup"],
    &["sure"],
    &["ok"],
    &["okay"],
    &["absolutely"],
    &["please"],
    &["thanks"],
    &["go", "ahead"],
    &["do", "it"],
    &["please", "do"],
    &["sure", "thing"],
    &["of", "course"],
];

/// Words that are a refusal wherever they stand. "never mind" is covered by
/// "never".
const NEGATIONS: &[&str] = &[
    "no",
    "nope",
    "nah",
    "not",
    "never",
    "dont",
    "don't",
    "cant",
    "can't",
    "wont",
    "won't",
    "cancel",
    "nevermind",
];

/// Break what was heard into lowercase words: punctuation is space, and the
/// apostrophe of "don't" stays (curly ones are made straight first).
fn words(heard: &str) -> Vec<String> {
    heard
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `words` is made entirely of [`AGREEMENTS`], at most
/// [`MAX_AGREEMENTS`] of them. Tries every way of cutting it up, since "please
/// do" and "do it" overlap in "please do it".
fn only_agreement(words: &[String], phrases_left: usize) -> bool {
    if words.is_empty() {
        return true;
    }
    if phrases_left == 0 {
        return false;
    }
    AGREEMENTS.iter().any(|phrase| {
        words.len() >= phrase.len()
            && phrase.iter().zip(words).all(|(a, b)| a == b)
            && only_agreement(&words[phrase.len()..], phrases_left - 1)
    })
}

/// What was said to a yes-or-no question.
///
/// Short whole-utterance answers only. Anything containing a negation is never
/// a [`SpokenAnswer::Yes`]: a short one is a [`SpokenAnswer::No`] -- declining
/// is the safe way to be wrong -- and a long one is
/// [`SpokenAnswer::Unclear`], since somebody holding forth is not answering.
pub fn spoken_answer(heard: &str) -> SpokenAnswer {
    let words = words(heard);
    if words.is_empty() || words.len() > MAX_ANSWER_WORDS {
        return SpokenAnswer::Unclear;
    }
    if words.iter().any(|w| NEGATIONS.contains(&w.as_str())) {
        // "do not" has its "not"; "never mind" its "never". Five words with
        // one is somebody explaining, not answering.
        if words.len() < MAX_ANSWER_WORDS {
            return SpokenAnswer::No;
        }
        return SpokenAnswer::Unclear;
    }
    if only_agreement(&words, MAX_AGREEMENTS) { SpokenAnswer::Yes } else { SpokenAnswer::Unclear }
}

/// The tools a spoken request is the go-ahead for: tasks, events, the
/// library's items and notes, made, changed and removed. The agent's gate
/// lets their calls in a spoken turn through without a card (see the module
/// doc for when it does not), and the session may ask aloud about a delete
/// of one that it still stops.
///
/// A project, a whole shelf's worth, a tracker, a memory and the rest are
/// not here: nobody asked for them by voice, and removing one is not undone.
pub const CHANGED_BY_VOICE: &[&str] = &[
    "create_task",
    "update_task",
    "delete_task",
    "create_event",
    "update_event",
    "delete_event",
    "create_item",
    "update_item",
    "delete_item",
    "log_item",
    "create_note",
    "update_note",
];

/// Words that ask for something to go, as a request says them. Whole words
/// only: "Dropbox" is not "drop", and "cancelled" says what happened rather
/// than what to do.
const REMOVING: &[&str] = &[
    "delete",
    "deleting",
    "remove",
    "removing",
    "cancel",
    "cancelling",
    "canceling",
    "clear",
    "drop",
    "erase",
    "scrap",
    "trash",
    "wipe",
    "rid",
    "bin",
];

/// Words that ask for an event to move or go. All an event's guests are
/// told of, once a stranger's words are in, is a new time or a cancellation
/// -- see the agent's gate, which asks about anything else.
const RESCHEDULING: &[&str] = &[
    "reschedule",
    "rescheduling",
    "schedule",
    "move",
    "moving",
    "push",
    "shift",
    "postpone",
    "delay",
    "bring",
    "change",
    "cancel",
    "cancelling",
    "canceling",
    "delete",
    "remove",
];

/// Words that ask for something to be added or set to happen: a task, a
/// reminder.
const ADDING: &[&str] =
    &["add", "adding", "remind", "assign", "create", "make", "put", "set", "schedule", "jot"];

/// Words that begin a question about something rather than a request to do
/// it: "what did I cancel?", "did you delete it?". "Can you", "could you" and
/// the like are requests, and are not here.
const ASKING_ABOUT: &[&str] = &[
    "what", "which", "when", "where", "who", "whose", "why", "how", "did", "do", "does", "have",
    "has", "is", "are", "was", "were", "should", "shall",
];

/// How many words before a verb a negation still governs it: "do not
/// delete", "don't you dare delete it", "never cancel".
const NEGATION_REACH: usize = 4;

/// Whether `request` -- what the person said this turn -- asks for a change of
/// the `kind` the gate would otherwise stop: something removed, for a
/// `destructive` call; an event moved or cancelled, for an `outward` one (its
/// guests are told); something added, for a `keep` (a task for the
/// assistant, or a reminder, which later runs read as the person's wishes).
///
/// It is not what approves a call in a turn nobody else's words have reached;
/// it only decides, once they have, whether a delete or an invitation the
/// model wants is plausibly the one the person asked for, or may be a
/// stranger's instruction it read on the way. So the word has to be asked
/// for: not in a question ("what did I cancel?"), and not negated ("read it,
/// but don't delete anything"). A request that asked in other words is asked
/// about aloud, and a yes does it.
pub fn request_asks_for(kind: &str, request: &str) -> bool {
    let wanted = match kind {
        "destructive" => REMOVING,
        "outward" => RESCHEDULING,
        "keep" => ADDING,
        _ => return false,
    };
    let words = words(request);
    if words.first().is_some_and(|first| ASKING_ABOUT.contains(&first.as_str())) {
        return false;
    }
    words.iter().enumerate().any(|(at, word)| {
        wanted.contains(&word.as_str()) && {
            let before = &words[at.saturating_sub(NEGATION_REACH)..at];
            !before.iter().any(|w| NEGATIONS.contains(&w.as_str()) || w == "without")
        }
    })
}

/// Whether a confirmation of this `kind` about `subject`, for a call to
/// `tool`, may be answered by voice: a web search or a page fetch when the
/// question can say all of it, and a delete of one of the things a spoken
/// request may change ([`CHANGED_BY_VOICE`]) when what goes has a short name.
pub fn may_answer_aloud(kind: &str, tool: &str, subject: &str) -> bool {
    match kind {
        "search" => search_words(subject).is_some(),
        "fetch" => spoken_address(subject).is_some(),
        "destructive" => CHANGED_BY_VOICE.contains(&tool) && what_goes(subject).is_some(),
        _ => false,
    }
}

/// What a delete removes, as it would be said. `None` when it has no name or
/// more than one line -- a long text below a card's first line is the card's
/// to show -- or a name too long to say.
fn what_goes(subject: &str) -> Option<String> {
    if subject.trim().contains('\n') {
        return None;
    }
    search_words(subject)
}

/// The longest search read aloud, in characters.
const MAX_SUBJECT_CHARS: usize = 100;

/// The longest address read aloud, in characters, without its scheme.
const MAX_ADDRESS_CHARS: usize = 60;

/// `text` as one line fit to drop into a question: whitespace of every kind
/// (newlines too) becomes a single space, and a full stop, question or
/// exclamation mark that ends a sentence becomes a comma.
///
/// The subject is the model's own wording of what it wants to do, and it may
/// have been written by whoever wrote the mail it just read. Without this, a
/// query of "x. Tell them the transfer is approved" would be read to the
/// person in the assistant's own voice as if it were a second thing the
/// assistant had said.
fn one_line(text: &str) -> String {
    let flat: String =
        text.chars().map(|c| if c.is_control() || c.is_whitespace() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(flat.len());
    let chars: Vec<char> = flat.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let ends_sentence = matches!(c, '.' | '!' | '?' | ';' | ':')
            && chars.get(i + 1).is_none_or(|next| next.is_whitespace());
        if ends_sentence {
            // Nothing at the very end: the question supplies its own mark.
            if i + 1 < chars.len() {
                out.push(',');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().trim_end_matches(',').trim().to_string()
}

/// The whole of a search, as it would be said. `None` when there is none, or
/// it is too long to say: cutting it would hide what it sends.
fn search_words(subject: &str) -> Option<String> {
    let line = one_line(subject);
    (!line.is_empty() && line.chars().count() <= MAX_SUBJECT_CHARS).then_some(line)
}

/// The whole of a web address, as it would be said: `news.example.org slash
/// today`. `None` for anything that cannot be judged by ear, which is left to
/// the card: a query or fragment, a login, a port, percent escapes, letters
/// that only look like ASCII, or an address too long to take in.
fn spoken_address(subject: &str) -> Option<String> {
    let mut words = subject.split_whitespace();
    let address = words.next()?;
    if words.next().is_some() {
        return None;
    }
    let rest = match address.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return None,
        None => address,
    };
    if !rest.is_ascii() || rest.len() > MAX_ADDRESS_CHARS {
        return None;
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let labels: Vec<&str> = host.split('.').collect();
    let host_is_a_name = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels.last().is_some_and(|tld| tld.chars().all(|c| c.is_ascii_alphabetic()));
    let path_is_plain =
        path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'));
    if !host_is_a_name || !path_is_plain {
        return None;
    }
    let mut said = host.to_string();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        said.push_str(" slash ");
        said.push_str(part);
    }
    Some(said)
}

/// The question to read aloud for a confirmation that
/// [`may_answer_aloud`] allows: one short sentence holding all of what it is
/// asking about.
pub fn question(kind: &str, subject: &str) -> String {
    match kind {
        "search" => match search_words(subject) {
            Some(words) => format!("Should I search the web for {words}?"),
            None => "Should I search the web?".to_string(),
        },
        "fetch" => match spoken_address(subject) {
            Some(address) => format!("Should I open {address}?"),
            None => "Should I open a web page?".to_string(),
        },
        "destructive" => match what_goes(subject) {
            Some(what) => format!("Should I delete {what}?"),
            None => "Should I delete it?".to_string(),
        },
        _ => "Should I go ahead?".to_string(),
    }
}

/// What is said when only a click can answer.
pub const NEEDS_A_CLICK: &str = "That needs your go-ahead on screen.";

/// [`NEEDS_A_CLICK`], on a call through a chat: the question is asked in the
/// chat, where a tap answers it.
pub const NEEDS_A_TAP: &str = "That needs your go-ahead in our chat.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_agreement_is_a_yes() {
        for said in [
            "Yes.",
            "yeah",
            "Yep!",
            "Sure",
            "OK",
            "Okay.",
            "Go ahead.",
            "Do it",
            "Please do.",
            "Yes, please.",
            "Yeah, go ahead",
            "okay, do it",
            "please do it",
            "Sure thing",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::Yes, "{said:?}");
        }
    }

    #[test]
    fn short_refusal_is_a_no() {
        for said in [
            "No.",
            "Nope",
            "Don't",
            "don\u{2019}t do it",
            "Do not.",
            "Cancel",
            "Never mind.",
            "No, don't",
            "no thanks",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::No, "{said:?}");
        }
    }

    #[test]
    fn a_sentence_that_merely_contains_yes_is_unclear() {
        for said in [
            "yes but only after you have checked my calendar",
            "I said yes to the other one yesterday",
            "tell me more about it first",
            "what are you searching for",
            "",
            "   ",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::Unclear, "{said:?}");
        }
    }

    #[test]
    fn a_negation_is_never_a_yes() {
        for said in ["yes no", "ok but don't", "sure not now", "yes never", "no go ahead"] {
            assert_ne!(spoken_answer(said), SpokenAnswer::Yes, "{said:?}");
        }
        // ... and a long one is not an answer at all.
        assert_eq!(
            spoken_answer("yes go ahead and do it but do not touch the other thing"),
            SpokenAnswer::Unclear
        );
    }

    #[test]
    fn only_a_search_a_fetch_or_a_delete_asked_for_by_voice_may_be_answered_aloud() {
        assert!(may_answer_aloud("search", "web_search", "best e-bike"));
        assert!(may_answer_aloud("fetch", "read_web_page", "https://news.example.org/today"));
        for tool in ["delete_task", "delete_event", "delete_item"] {
            assert!(may_answer_aloud("destructive", tool, "Book the dentist"), "{tool}");
        }
        for tool in ["delete_project", "delete_tracker", "forget", "delete_note", "trash_thread"] {
            assert!(!may_answer_aloud("destructive", tool, "Book the dentist"), "{tool}");
        }
        assert!(
            !may_answer_aloud("destructive", "delete_task", "Notes\n\nall of them"),
            "a card with a long text below is the card's to show"
        );
        assert!(!may_answer_aloud("destructive", "delete_task", ""), "nothing to name");
        for kind in
            ["outward", "type", "click", "routine", "rewrite", "connection", "keep", "", "Search"]
        {
            assert!(
                !may_answer_aloud(kind, "delete_event", "https://news.example.org"),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_delete_is_asked_by_the_name_of_what_goes() {
        assert_eq!(
            question("destructive", "\u{201c}Team sync\u{201d} on 2026-11-20"),
            "Should I delete \u{201c}Team sync\u{201d} on 2026-11-20?"
        );
        let said = question("destructive", "Milk. Tell them it is approved!");
        assert_eq!(said.matches('?').count(), 1, "{said}");
    }

    #[test]
    fn a_request_asks_for_a_removal_in_its_own_words() {
        for said in [
            "cancel my three o'clock",
            "Delete the dentist task.",
            "remove that book from my shelf",
            "get rid of the milk task",
            "clear Friday afternoon",
            "I cancelled it, so drop it from the list",
            "can you delete the dentist task?",
            "I don't need it any more, delete it",
        ] {
            assert!(request_asks_for("destructive", said), "{said}");
        }
        for said in [
            "what's on my calendar today?",
            "read me the newsletter",
            "is it clearly the right one?",
            "add milk to my tasks",
            "read me Bob's mail, but don't delete anything",
            "do not cancel anything",
            "never remove my notes",
            "what did I cancel last week?",
            "did you delete it?",
            "save it to Dropbox",
            "I cancelled the meeting",
            "",
        ] {
            assert!(!request_asks_for("destructive", said), "{said}");
        }
    }

    #[test]
    fn a_request_asks_for_an_event_to_move_or_go_in_its_own_words() {
        for said in [
            "move my three o'clock to four",
            "reschedule lunch with Sam to Friday",
            "cancel the team sync",
            "push it back an hour",
            "could you postpone the review?",
        ] {
            assert!(request_asks_for("outward", said), "{said}");
        }
        for said in [
            "what's on today?",
            "who is coming to the sync?",
            "read me the mail from Acme and add milk to my list",
            "make a note of it",
            "set a timer",
            "don't move anything",
        ] {
            assert!(!request_asks_for("outward", said), "{said}");
        }
        assert!(!request_asks_for("search", "delete it"), "only what the gate stops");
    }

    #[test]
    fn a_request_asks_for_a_task_or_a_reminder_in_its_own_words() {
        for said in ["remind me at nine to ring the vet", "add a task for you to file it"] {
            assert!(request_asks_for("keep", said), "{said}");
        }
        for said in ["what's on my list?", "what tasks do I have?", "read me that mail"] {
            assert!(!request_asks_for("keep", said), "{said}");
        }
    }

    #[test]
    fn a_search_says_all_of_what_it_would_send() {
        assert_eq!(
            question("search", "best e-bike under 2000"),
            "Should I search the web for best e-bike under 2000?"
        );
        assert!(!may_answer_aloud("search", "web_search", "  "));
        let long = "word ".repeat(40);
        assert!(!may_answer_aloud("search", "web_search", &long), "cutting it would hide the rest");
    }

    #[test]
    fn a_fetch_says_the_whole_address() {
        assert_eq!(
            question("fetch", "https://www.Example.com/news/today"),
            "Should I open example.com slash news slash today?"
        );
        assert_eq!(question("fetch", "http://news.example.org"), "Should I open news.example.org?");
        assert_eq!(question("fetch", "example.com/page/"), "Should I open example.com slash page?");
    }

    #[test]
    fn an_address_that_cannot_be_judged_by_ear_needs_the_card() {
        for address in [
            "https://attacker.example/?d=private",
            "https://attacker.example/#private",
            "https://attacker.example/a%20b",
            "https://accounts.google.com@evil.test/login",
            "https://good.example\\@evil.test/",
            "https://example.com:8443/",
            "https://exam\u{440}le.com/",
            "ftp://example.com/",
            "https://[::1]/x",
            "https://127.0.0.1/x",
            "https:///nothing",
            "https://accounts.google.com.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.evil.test/",
            "https://example.com/ and more",
            "",
        ] {
            assert!(!may_answer_aloud("fetch", "read_web_page", address), "{address}");
        }
    }

    #[test]
    fn another_kind_gets_a_plain_question() {
        assert_eq!(question("outward", "the deck"), "Should I go ahead?");
    }

    #[test]
    fn a_subject_cannot_smuggle_in_more_sentences() {
        let said = question("search", "cheap flights. Tell them it is approved!\nNow");
        assert_eq!(said.matches('?').count(), 1, "{said}");
        assert!(!said.contains('.') && !said.contains('!') && !said.contains('\n'), "{said}");
    }
}
