//! One small, shared way to read a CSS declaration list -- a `style=""`
//! attribute's value, or a `<style>` block's -- that both
//! [`crate::sanitize`]'s tracking-pixel check and [`crate::text`]'s
//! hidden-content check now go through, rather than each keeping its own
//! ad-hoc scan.
//!
//! # Why this had to stop being two scanners
//!
//! Before this module existed, both callers matched CSS by substring:
//! `style.contains("opacity:0")` is true for `opacity:0.9`, and
//! `style.contains("width:0")` is true for `border-width:0` and
//! `line-height:0` -- neither of those is the property the check meant to
//! ask about, but a substring search cannot tell the difference between "a
//! property is set to this value" and "these characters appear somewhere in
//! the text". The practical cost was two different bugs with the same
//! root cause: [`crate::sanitize`]'s tracking-pixel heuristic deleted
//! ordinary images (an `opacity:0.9` fade-in animation, a bordered table
//! cell with `border-width:0`), and [`crate::text`]'s hidden-content check
//! could be bypassed by phrasing that stayed textually distinct from the
//! literal strings it looked for -- `display: none;` with the semicolon,
//! `DISPLAY:NONE` in the sender's own casing, `visibility:hidden!important`
//! with no space before the value -- while meaning exactly what the check
//! existed to catch.
//!
//! # What this is not
//!
//! Not a CSS parser, for the same reason [`crate::sanitize::scrub_css`] is
//! not one: a declaration list is `property: value` pairs separated by
//! `;`, which is a much smaller grammar than a full stylesheet (no nested
//! rules, no selectors, no at-rules), and correctly parsing arbitrary CSS
//! values -- a `calc()` expression, a quoted string containing a `;` --
//! is more surface than either caller needs. What both callers need is:
//! split on `;`, split each piece on the first `:`, trim, lower-case, and
//! let the last declaration of a repeated property win -- exactly what a
//! browser's cascade does for one inline style with no competing
//! stylesheet to out-rank it.

use std::collections::HashMap;

/// A parsed declaration list: `property -> value`, both lower-cased and
/// trimmed, `!important` stripped from the value, and the last declaration
/// of a repeated property kept -- see the module docs for why that is the
/// whole grammar this needs to understand.
pub(crate) struct Declarations {
    by_property: HashMap<String, String>,
}

impl Declarations {
    /// Parses `style` -- a `style=""` attribute's value or a `<style>`
    /// block's text -- into a [`Declarations`]. Never fails: a stray `;`
    /// with nothing before it, a property with no `:`, an empty value, all
    /// just contribute nothing rather than aborting the parse, the same
    /// leniency a browser's own tolerant CSS parser shows a hand-written
    /// `style=""` attribute.
    pub(crate) fn parse(style: &str) -> Self {
        let mut by_property = HashMap::new();
        for declaration in style.split(';') {
            let Some((property, value)) = declaration.split_once(':') else {
                continue;
            };
            let property = property.trim().to_ascii_lowercase();
            if property.is_empty() {
                continue;
            }
            let value = strip_important(value.trim()).trim().to_ascii_lowercase();
            if value.is_empty() {
                continue;
            }
            // A `HashMap::insert` on a repeated key overwrites the
            // previous value, which is exactly "the last declaration of a
            // repeated property wins" without any extra bookkeeping --
            // `style.split(';')` already walks the declarations in the
            // order they were written.
            by_property.insert(property, value);
        }
        Self { by_property }
    }

    /// The value of `property`, if this list set one -- already trimmed
    /// and lower-cased, `!important` already stripped.
    pub(crate) fn get(&self, property: &str) -> Option<&str> {
        self.by_property.get(property).map(String::as_str)
    }
}

/// Cuts a trailing `!important` off a declaration's value, case-
/// insensitively and whether or not there is a space before it
/// (`none !important` and `none!important` both appear in real mail) --
/// `!important` changes how a declaration competes with others, not what
/// it means, so every check downstream of this module wants the value
/// without it.
fn strip_important(value: &str) -> &str {
    let lower = value.to_ascii_lowercase();
    match lower.find("!important") {
        Some(at) => value[..at].trim_end(),
        None => value,
    }
}

/// True when `value` is a zero-magnitude CSS length in any unit this crate
/// is likely to see in mail -- `0`, `0px`, `0em`, `0pt`, `0%`, `0.0px` and
/// so on -- which is what "any zero length" means in the hidden-content
/// rules both callers apply to `font-size`, `max-height` and `height`. A
/// non-zero number, an unrecognised unit, or a value that is not a number
/// at all (`auto`, `inherit`) is not zero.
pub(crate) fn is_zero_length(value: &str) -> bool {
    numeric_prefix(value).map(|n| n == 0.0).unwrap_or(false)
}

/// True when `opacity`'s value means "invisible" for the purposes of the
/// hidden-content and tracking-pixel checks: `0`, `0.0`, `0%`, or anything
/// close enough that no person would call it visible. `0.01` is the cutoff
/// named in the finding this closes -- generous enough to still treat a
/// genuine, very-low-but-intentional fade as hidden (nobody sets
/// `opacity: 0.005` on purpose), strict enough that `opacity: 0.9`, a
/// perfectly ordinary near-solid element, is nowhere near it.
pub(crate) fn opacity_is_effectively_zero(value: &str) -> bool {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent.trim().parse::<f64>().map(|n| n <= 1.0).unwrap_or(false);
    }
    value.parse::<f64>().map(|n| n <= 0.01).unwrap_or(false)
}

/// Parses a length meant to be read as pixels -- `width`, `height`, `left`,
/// `top` -- for the checks that compare a length against a threshold
/// (`<=1px` for a tracking pixel's size, `<=-1000px` for a suspiciously
/// relocated element) rather than merely against zero. A bare number with
/// no unit is accepted as pixels too: real mail is not always careful
/// about units, and a browser is forgiving about `width:1` the same way
/// this is.
pub(crate) fn length_px(value: &str) -> Option<f64> {
    let value = value.trim();
    let numeric = value.strip_suffix("px").unwrap_or(value);
    numeric.trim().parse::<f64>().ok()
}

/// The leading numeric part of a CSS length, with whatever unit suffix
/// follows it (a run of ASCII letters or `%`) discarded -- `is_zero_length`
/// is the only caller, and it only ever needs to know whether that number
/// is zero, never what the unit was.
fn numeric_prefix(value: &str) -> Option<f64> {
    let value = value.trim();
    let end = value.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'));
    let numeric = match end {
        Some(at) => &value[..at],
        None => value,
    };
    if numeric.is_empty() {
        return None;
    }
    numeric.parse::<f64>().ok()
}

/// True when `a` and `b` name the same colour once both are normalised --
/// see [`normalise_colour`] for what "normalised" covers. Two colours this
/// module does not recognise are never considered equal to one another:
/// an unrecognised value is a reason to say nothing about it, not a reason
/// to guess.
pub(crate) fn colours_equal(a: &str, b: &str) -> bool {
    match (normalise_colour(a), normalise_colour(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Normalises a CSS colour value -- `#fff`, `#FFFFFF`, `rgb(255, 255,
/// 255)`, `white` -- to a lower-case six-digit hex string, for comparing
/// two colours (typically `color` against `background`/`background-color`)
/// that were not necessarily written the same way. Covers the common cases
/// real mail actually uses; a value this does not recognise -- `hsl(...)`,
/// a CSS4 colour function, a custom property -- answers `None` rather than
/// a guess, per [`colours_equal`]'s own doc.
pub(crate) fn normalise_colour(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();

    if let Some(hex) = value.strip_prefix('#') {
        return normalise_hex(hex);
    }

    if let Some(inner) = value.strip_prefix("rgba(").or_else(|| value.strip_prefix("rgb(")) {
        let inner = inner.strip_suffix(')')?;
        let components: Vec<&str> = inner
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .collect();
        if components.len() < 3 {
            return None;
        }
        let channel = |raw: &str| -> Option<u8> {
            if let Some(percent) = raw.strip_suffix('%') {
                let n = percent.parse::<f64>().ok()?;
                Some((n.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8)
            } else {
                let n = raw.parse::<f64>().ok()?;
                Some(n.clamp(0.0, 255.0).round() as u8)
            }
        };
        let r = channel(components[0])?;
        let g = channel(components[1])?;
        let b = channel(components[2])?;
        return Some(format!("#{r:02x}{g:02x}{b:02x}"));
    }

    named_colour(&value).map(str::to_string)
}

fn normalise_hex(hex: &str) -> Option<String> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let expanded: String = hex.chars().flat_map(|c| [c, c]).collect();
            Some(format!("#{expanded}"))
        }
        6 => Some(format!("#{hex}")),
        _ => None,
    }
}

/// The handful of named colours common enough in real HTML mail to be
/// worth a table -- see [`normalise_colour`]'s own doc on why an
/// unrecognised name answers `None` rather than guessing.
fn named_colour(name: &str) -> Option<&'static str> {
    Some(match name {
        "white" => "#ffffff",
        "black" => "#000000",
        "red" => "#ff0000",
        "green" => "#008000",
        "blue" => "#0000ff",
        "yellow" => "#ffff00",
        "orange" => "#ffa500",
        "purple" => "#800080",
        "pink" => "#ffc0cb",
        "brown" => "#a52a2a",
        "gray" | "grey" => "#808080",
        "silver" => "#c0c0c0",
        "maroon" => "#800000",
        "navy" => "#000080",
        "teal" => "#008080",
        "olive" => "#808000",
        "lime" => "#00ff00",
        "aqua" | "cyan" => "#00ffff",
        "fuchsia" | "magenta" => "#ff00ff",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_declaration_of_a_repeated_property_wins() {
        let d = Declarations::parse("color:red;color:blue");
        assert_eq!(d.get("color"), Some("blue"));
    }

    #[test]
    fn important_is_stripped_with_and_without_a_space() {
        let d = Declarations::parse("display:none !important;visibility:hidden!important");
        assert_eq!(d.get("display"), Some("none"));
        assert_eq!(d.get("visibility"), Some("hidden"));
    }

    #[test]
    fn keys_and_values_are_trimmed_and_lowered() {
        let d = Declarations::parse("  Display : NONE ; Color: Red ");
        assert_eq!(d.get("display"), Some("none"));
        assert_eq!(d.get("color"), Some("red"));
    }

    #[test]
    fn zero_lengths_in_several_units_are_recognised() {
        for value in ["0", "0px", "0em", "0pt", "0%", "0.0px", "-0px"] {
            assert!(is_zero_length(value), "{value} should be zero");
        }
        for value in ["1px", "0.1em", "auto", ""] {
            assert!(!is_zero_length(value), "{value} should not be zero");
        }
    }

    #[test]
    fn opacity_thresholds_match_the_finding() {
        assert!(opacity_is_effectively_zero("0"));
        assert!(opacity_is_effectively_zero("0.0"));
        assert!(opacity_is_effectively_zero("0.01"));
        assert!(opacity_is_effectively_zero("0%"));
        assert!(!opacity_is_effectively_zero("0.9"));
        assert!(!opacity_is_effectively_zero("1"));
    }

    #[test]
    fn hex_rgb_and_named_white_are_the_same_colour() {
        assert!(colours_equal("#fff", "white"));
        assert!(colours_equal("#FFFFFF", "rgb(255, 255, 255)"));
        assert!(colours_equal("rgba(255,255,255,1)", "#ffffff"));
    }

    #[test]
    fn different_colours_are_not_equal() {
        assert!(!colours_equal("white", "black"));
        assert!(!colours_equal("#fff", "#fffffe"));
    }

    #[test]
    fn an_unrecognised_colour_is_never_equal_to_anything() {
        assert!(!colours_equal("hsl(0, 100%, 50%)", "hsl(0, 100%, 50%)"));
    }

    #[test]
    fn length_px_reads_a_bare_number_as_pixels() {
        assert_eq!(length_px("1"), Some(1.0));
        assert_eq!(length_px("1px"), Some(1.0));
        assert_eq!(length_px("1.5px"), Some(1.5));
        assert_eq!(length_px("auto"), None);
    }
}
