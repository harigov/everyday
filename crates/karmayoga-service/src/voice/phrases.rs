//! What a spoken conversation makes of the words in it: whether what was heard
//! is the assistant's own voice coming back, whether it means stop, and what to
//! say while a tool runs.

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

/// A few words to say while `tool` runs, by what the tool is for. The
/// assistant's tools are named verb then thing (`list_events`,
/// `search_mail`), so it is the words of the name that say what it touches.
pub fn tool_phrase(tool: &str) -> &'static str {
    let lower = tool.to_lowercase();
    let words: Vec<&str> = lower.split('_').collect();
    let has = |names: &[&str]| {
        words.iter().any(|w| names.iter().any(|n| w == n || w.strip_suffix('s') == Some(*n)))
    };
    if lower.starts_with("browser") || has(&["web"]) {
        "Let me look that up."
    } else if has(&["mail", "thread", "draft", "inbox", "message"]) {
        "Checking your mail."
    } else if has(&["event", "calendar", "invite"]) {
        "Checking your calendar."
    } else if has(&["task", "project"]) {
        "Looking at your tasks."
    } else if has(&["note", "search", "journal", "entry", "memory"]) {
        "Let me find that."
    } else {
        "One moment."
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

    // ---- tool phrases -----------------------------------------------------

    #[test]
    fn a_tool_is_described_by_what_it_touches() {
        for web in ["web_search", "read_web_page", "browser_open", "browser_type", "browser_other"]
        {
            assert_eq!(tool_phrase(web), "Let me look that up.", "{web}");
        }
        for mail in ["search_mail", "list_threads", "read_thread", "draft_reply", "archive_thread"]
        {
            assert_eq!(tool_phrase(mail), "Checking your mail.", "{mail}");
        }
        for calendar in
            ["list_events", "create_event", "update_event", "list_calendars", "respond_to_invite"]
        {
            assert_eq!(tool_phrase(calendar), "Checking your calendar.", "{calendar}");
        }
        for task in ["list_tasks", "create_task", "update_task", "list_projects"] {
            assert_eq!(tool_phrase(task), "Looking at your tasks.", "{task}");
        }
        for find in
            ["list_notes", "get_note", "search", "search_conversations", "list_meeting_notes"]
        {
            assert_eq!(tool_phrase(find), "Let me find that.", "{find}");
        }
        for other in ["overview", "list_trackers", "spending_summary", ""] {
            assert_eq!(tool_phrase(other), "One moment.", "{other}");
        }
    }
}
