//! What a timer's notification says.
//!
//! A timer that has run out is a notification with a sound, and what it
//! has to say is short: that the timer is done, which one, and enough about
//! it -- how long, set when -- that somebody who set three of them and went
//! to another room knows which this was. All of it is plain text from a
//! [`Timer`](jatayu_service::desk::Timer), with no desktop anywhere near
//! it, so it is worked out here and tested here.

use jiff::civil::Time;

/// The longest a label is allowed to run in a notification's title: the
/// service's own limit for a label, so a label the model was told was kept is
/// the label the title shows. A label is what the model took from "a timer
/// for the pasta" -- a word or two -- and one that is a paragraph is the
/// model's mistake, which a title must not pass on in full.
const MAX_LABEL_CHARS: usize = jatayu_service::desk::timers::MAX_LABEL_CHARS;

/// The title and the body of the notification for a timer that has run out:
/// "Pasta \u{2014} timer done" and "10 minutes, set at 2:05 pm".
///
/// `seconds` is how long the timer was set for, `set_at` the time of day it
/// was set (the person's own clock, not UTC).
pub(super) fn timer_done(label: Option<&str>, seconds: i64, set_at: Time) -> (String, String) {
    let title = match label.and_then(tidy_label) {
        Some(label) => format!("{label} \u{2014} timer done"),
        None => "Timer done".to_string(),
    };
    let set = format!("set at {}", clock(set_at));
    // The service's wording, so the notification says what `list_timers`
    // told the model.
    let body = match jatayu_service::desk::spoken_length(seconds) {
        Some(length) => format!("{length}, {set}"),
        None => set,
    };
    (title, body)
}

/// `label` on one line, trimmed and cut to a length a title can hold, or
/// `None` when nothing is left of it.
fn tidy_label(label: &str) -> Option<String> {
    let one_line = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    if one_line.chars().count() <= MAX_LABEL_CHARS {
        return Some(one_line);
    }
    let cut: String = one_line.chars().take(MAX_LABEL_CHARS - 1).collect();
    Some(format!("{}\u{2026}", cut.trim_end()))
}

/// A time of day on a twelve-hour clock: "2:05 pm", "12:00 am".
fn clock(time: Time) -> String {
    let hour = time.hour();
    let twelve = match hour % 12 {
        0 => 12,
        h => h,
    };
    let half = if hour < 12 { "am" } else { "pm" };
    format!("{twelve}:{:02} {half}", time.minute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: i8, minute: i8) -> Time {
        Time::new(hour, minute, 0, 0).unwrap()
    }

    #[test]
    fn a_labelled_timer_says_which_one_it_was() {
        assert_eq!(
            timer_done(Some("Pasta"), 600, at(14, 5)),
            ("Pasta \u{2014} timer done".to_string(), "10 minutes, set at 2:05 pm".to_string())
        );
    }

    #[test]
    fn an_unlabelled_timer_is_just_done() {
        assert_eq!(
            timer_done(None, 90, at(0, 0)),
            ("Timer done".to_string(), "1 minute 30 seconds, set at 12:00 am".to_string())
        );
        // A label of nothing is no label.
        assert_eq!(timer_done(Some("   "), 60, at(9, 30)).0, "Timer done");
    }

    #[test]
    fn a_label_is_one_short_line() {
        assert_eq!(tidy_label("  the\n  pasta \t water "), Some("the pasta water".to_string()));
        let long = "x".repeat(200);
        let cut = tidy_label(&long).unwrap();
        assert_eq!(cut.chars().count(), MAX_LABEL_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn a_time_of_day_is_on_a_twelve_hour_clock() {
        assert_eq!(clock(at(0, 5)), "12:05 am");
        assert_eq!(clock(at(11, 59)), "11:59 am");
        assert_eq!(clock(at(12, 0)), "12:00 pm");
        assert_eq!(clock(at(23, 41)), "11:41 pm");
    }

    #[test]
    fn a_timer_of_no_length_says_only_when_it_was_set() {
        assert_eq!(timer_done(None, 0, at(7, 0)).1, "set at 7:00 am");
    }
}
