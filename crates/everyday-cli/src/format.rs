//! Turning a value into the handful of words a terminal shows for it.
//!
//! Three small, pure functions with no vault and no clap in sight, which is
//! the whole reason they are worth keeping apart from `verbs.rs`: each one is
//! easy to get subtly wrong -- a truncation that splits a character in half,
//! a unit that rounds the wrong way -- and easy to test in isolation once it
//! is not tangled up with the command that happens to call it.

/// Shorten `s` to at most `max` characters, respecting character boundaries
/// rather than byte ones -- a title ending in an emoji must not be cut
/// through the middle of it.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(['\u{2026}']).collect()
}

/// A byte count, in whichever unit reads most naturally.
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

/// The first sentence of a tool's description, for `everyday do --list`'s
/// one-line-per-tool table -- the full description is written for a model
/// deciding whether to call the tool, not for a terminal's width.
pub(crate) fn first_sentence(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.find(". ") {
        Some(at) => text[..=at].to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_reads_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("short", 10), "short");
        let out = truncate(&"\u{1f600}".repeat(50), 5);
        assert_eq!(out.chars().count(), 5);
    }
}
