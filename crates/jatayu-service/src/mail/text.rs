//! Small text helpers the mail modules share.

/// `s` cut to at most `cap` characters, with nothing added where it was cut.
pub(crate) fn excerpt(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap { s.to_string() } else { s.chars().take(cap).collect() }
}
