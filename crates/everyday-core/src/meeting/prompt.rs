//! What the summarising model is asked, and how a long call is split.

use super::Transcript;

/// Rough token estimate: four characters each, rounded up.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32).div_ceil(4)
}

/// The default budget for one summarising request's transcript, in tokens:
/// smaller for a loopback (local) endpoint.
pub fn default_budget(loopback: bool) -> u32 {
    if loopback { 6_000 } else { 24_000 }
}

/// A system prompt and a user message.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub system: String,
    pub user: String,
}

/// Split the transcript's lines (see [`Transcript::as_lines`]) into pieces
/// that each fit `budget`, only ever on line boundaries. One piece if it fits.
pub fn pieces(transcript: &Transcript, budget: u32) -> Vec<String> {
    let _ = (transcript, budget);
    todo!("core-logic agent")
}

/// The request for one piece (or the whole call). `part` is `Some((n, of))`
/// when the call was split.
pub fn summarise(filled_template: &str, lines: &str, part: Option<(usize, usize)>) -> Request {
    let _ = (filled_template, lines, part);
    todo!("core-logic agent")
}

/// The request that merges the per-piece answers into one note.
pub fn combine(filled_template: &str, partials: &[String]) -> Request {
    let _ = (filled_template, partials);
    todo!("core-logic agent")
}

/// Hints for a transcriber: event title and attendee names, so names are
/// spelled right. Short; recognisers cap their prompts.
pub fn transcriber_hint(title: &str, names: &[String]) -> String {
    let _ = (title, names);
    todo!("core-logic agent")
}
