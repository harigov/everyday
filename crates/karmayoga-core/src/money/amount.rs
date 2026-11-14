//! Money as integers: minor units and the currency they are counted in.
//!
//! A float cannot hold a cent exactly, and a budget that is off by one
//! cent after a thousand additions is a budget nobody trusts. So every
//! amount in this domain is an `i64` of the currency's smallest unit --
//! cents for dollars, yen for yen, fils for dinars -- and the only places a
//! decimal is ever read or written are [`parse_decimal`] (from a bank, a
//! file, a person) and [`format_minor`] (for an export).
//!
//! The exponent comes from ISO 4217. Almost everything has two decimals; a
//! handful have none or three, and those are listed. An unknown code is
//! assumed to have two, which is right for every currency that has been
//! issued since the list below was written.

use crate::error::{Error, Result};

/// An ISO 4217 code, upper case. A plain string on records and the wire;
/// this alias only says what one means.
pub type Currency = String;

/// How many decimal places `currency`'s minor unit is.
pub fn minor_exponent(currency: &str) -> u32 {
    match currency.trim().to_ascii_uppercase().as_str() {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        _ => 2,
    }
}

/// Read a decimal amount -- `"12.34"`, `"-1,234.5"`, `"(12.00)"`, `"$5"` --
/// as minor units of `currency`, exactly.
///
/// Accepts what banks and spreadsheets write: an optional sign or
/// accounting parentheses, a currency symbol, thousands separators (`,` or
/// a space or an apostrophe), and a `.` decimal point. A comma decimal
/// point (`"12,34"`) is read as one only when it is the sole separator and
/// is followed by exactly the currency's number of digits; anything more
/// ambiguous is refused rather than guessed. More decimal places than the
/// currency has are rounded half away from zero, which is how a bank
/// rounds a fraction of a cent.
pub fn parse_decimal(text: &str, currency: &str) -> Result<i64> {
    let exp = minor_exponent(currency);
    let invalid = || Error::Invalid(format!("not an amount: {text:?}"));
    let mut s = text.trim().to_string();
    if s.is_empty() {
        return Err(invalid());
    }
    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].trim().to_string();
    }
    // "42.17 DR" and "100.00 CR": how many British, Australian and Indian
    // exports say which way money went. The marker *is* the sign -- a debit
    // is money out, a credit money in -- so it decides, rather than being
    // dropped with the other letters around a number.
    let (rest, marker) = debit_credit(&s);
    s = rest;
    // A sign, a currency symbol or code, and spaces may come before the
    // number; a symbol, a code or a trailing minus after it. Nothing but
    // digits and separators may come in the middle.
    let is_decoration =
        |c: char| c.is_alphabetic() || c.is_whitespace() || "$€£¥₹₩₽₺₪₫¢".contains(c);
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0;
    while start < chars.len() {
        match chars[start] {
            '-' | '\u{2212}' => negative = !negative,
            '+' => {}
            c if is_decoration(c) => {}
            _ => break,
        }
        start += 1;
    }
    let mut end = chars.len();
    while end > start {
        match chars[end - 1] {
            '-' | '\u{2212}' => negative = !negative,
            c if is_decoration(c) => {}
            _ => break,
        }
        end -= 1;
    }
    let mut cleaned = String::with_capacity(end - start);
    for &c in &chars[start..end] {
        match c {
            '0'..='9' | '.' | ',' => cleaned.push(c),
            ' ' | '\'' | '\u{a0}' | '\u{202f}' => {}
            _ => return Err(invalid()),
        }
    }
    if cleaned.is_empty() || !cleaned.chars().any(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }

    // Decide which character is the decimal point.
    let dots = cleaned.matches('.').count();
    let commas = cleaned.matches(',').count();
    let (int_part, frac_part) = match (dots, commas) {
        (0, 0) => (cleaned.clone(), String::new()),
        (1, _) => {
            let (a, b) = cleaned.split_once('.').expect("one dot");
            if b.contains(',') {
                return Err(invalid());
            }
            (a.replace(',', ""), b.to_string())
        }
        (0, 1) => {
            let (a, b) = cleaned.split_once(',').expect("one comma");
            if b.len() == exp as usize && exp > 0 && b.len() != 3 {
                (a.to_string(), b.to_string())
            } else if b.len() == 3 {
                // "1,234": a thousands separator.
                (format!("{a}{b}"), String::new())
            } else {
                return Err(invalid());
            }
        }
        (0, _) => (cleaned.replace(',', ""), String::new()),
        _ => return Err(invalid()),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(invalid());
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(invalid());
    }

    let whole: i64 =
        if int_part.is_empty() { 0 } else { int_part.parse::<i64>().map_err(|_| invalid())? };
    let scale = 10_i64.pow(exp);
    let mut frac: i64 = 0;
    let digits: Vec<u32> = frac_part.chars().filter_map(|c| c.to_digit(10)).collect();
    for i in 0..exp as usize {
        frac = frac * 10 + i64::from(*digits.get(i).unwrap_or(&0));
    }
    // Round half away from zero on the first dropped digit.
    if digits.get(exp as usize).is_some_and(|d| *d >= 5) {
        frac += 1;
    }
    let minor = whole
        .checked_mul(scale)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| Error::Invalid(format!("amount too large: {text:?}")))?;
    Ok(match marker {
        Some(Marker::Debit) => -minor,
        Some(Marker::Credit) => minor,
        None if negative => -minor,
        None => minor,
    })
}

/// Which way a `DR`/`CR` marker says the money went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Debit,
    Credit,
}

/// `text` without a leading or trailing `DR`/`CR` marker (any case, with or
/// without a space or a full stop), and the marker. Only a whole word, or
/// letters straight after the last digit: "CRC", a currency, is not one.
fn debit_credit(text: &str) -> (String, Option<Marker>) {
    let marker = |word: &str| match word.trim_end_matches('.').to_ascii_uppercase().as_str() {
        "DR" => Some(Marker::Debit),
        "CR" => Some(Marker::Credit),
        _ => None,
    };
    let trimmed = text.trim();
    // A trailing word: "42.17 DR", or glued to the digits: "42.17DR".
    let tail_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphabetic() || *c == '.')
        .last()
        .map(|(i, _)| i);
    if let Some(i) = tail_start {
        let (head, word) = trimmed.split_at(i);
        let glued_or_spaced = head.ends_with(|c: char| c.is_ascii_digit() || c.is_whitespace());
        if glued_or_spaced && let Some(found) = marker(word) {
            return (head.trim().to_string(), Some(found));
        }
    }
    // A leading word: "DR 42.17".
    if let Some((word, rest)) = trimmed.split_once(char::is_whitespace)
        && let Some(found) = marker(word)
    {
        return (rest.trim().to_string(), Some(found));
    }
    (trimmed.to_string(), None)
}

/// Minor units as a plain decimal string -- `-1234.50` -- with no symbol
/// and no separators: what a CSV export writes, and what [`parse_decimal`]
/// reads back exactly.
pub fn format_minor(minor: i64, currency: &str) -> String {
    let exp = minor_exponent(currency);
    if exp == 0 {
        return minor.to_string();
    }
    let scale = 10_u64.pow(exp);
    let abs = minor.unsigned_abs();
    let sign = if minor < 0 { "-" } else { "" };
    format!("{sign}{}.{:0width$}", abs / scale, abs % scale, width = exp as usize)
}

/// A float from an API (Plaid's `amount: 12.34`) as minor units, rounded to
/// the nearest. Goes through the shortest decimal string that round-trips
/// the float, so `0.1 + 0.2`-style noise in the float never reaches a cent.
pub fn from_f64(value: f64, currency: &str) -> Result<i64> {
    if !value.is_finite() {
        return Err(Error::Invalid("not a finite amount".into()));
    }
    parse_decimal(&format!("{value}"), currency)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_amounts_are_exact() {
        assert_eq!(parse_decimal("12.34", "USD").unwrap(), 1234);
        assert_eq!(parse_decimal("-12.34", "USD").unwrap(), -1234);
        assert_eq!(parse_decimal("12", "USD").unwrap(), 1200);
        assert_eq!(parse_decimal("0.5", "USD").unwrap(), 50);
        assert_eq!(parse_decimal(".5", "USD").unwrap(), 50);
        assert_eq!(parse_decimal("1,234.56", "USD").unwrap(), 123_456);
        assert_eq!(parse_decimal("1 234.56", "EUR").unwrap(), 123_456);
    }

    #[test]
    fn accounting_forms_and_symbols() {
        assert_eq!(parse_decimal("(12.00)", "USD").unwrap(), -1200);
        assert_eq!(parse_decimal("$5", "USD").unwrap(), 500);
        assert_eq!(parse_decimal("-$5.25", "USD").unwrap(), -525);
        assert_eq!(parse_decimal("12.00-", "USD").unwrap(), -1200);
        assert_eq!(parse_decimal("+3.10", "USD").unwrap(), 310);
    }

    #[test]
    fn a_comma_decimal_only_when_unambiguous() {
        assert_eq!(parse_decimal("12,34", "EUR").unwrap(), 1234);
        assert_eq!(parse_decimal("1,234", "USD").unwrap(), 123_400);
        assert!(parse_decimal("1,2", "EUR").is_err());
    }

    #[test]
    fn exponents_follow_the_currency() {
        assert_eq!(parse_decimal("1500", "JPY").unwrap(), 1500);
        assert_eq!(parse_decimal("1.234", "KWD").unwrap(), 1234);
        assert_eq!(format_minor(1234, "KWD"), "1.234");
        assert_eq!(format_minor(1500, "JPY"), "1500");
    }

    #[test]
    fn extra_places_round_half_away_from_zero() {
        assert_eq!(parse_decimal("0.005", "USD").unwrap(), 1);
        assert_eq!(parse_decimal("-0.005", "USD").unwrap(), -1);
        assert_eq!(parse_decimal("0.004", "USD").unwrap(), 0);
    }

    #[test]
    fn a_debit_or_credit_marker_is_the_sign() {
        assert_eq!(parse_decimal("42.17 DR", "GBP").unwrap(), -4217);
        assert_eq!(parse_decimal("42.17DR", "GBP").unwrap(), -4217);
        assert_eq!(parse_decimal("100.00 CR", "AUD").unwrap(), 10_000);
        assert_eq!(parse_decimal("100.00 cr.", "AUD").unwrap(), 10_000);
        assert_eq!(parse_decimal("DR 5.00", "INR").unwrap(), -500);
        // The marker decides even against a sign written beside it.
        assert_eq!(parse_decimal("-42.17 DR", "GBP").unwrap(), -4217);
        assert_eq!(parse_decimal("-100.00 CR", "GBP").unwrap(), 10_000);
        // A currency code that happens to start with the letters is not one.
        assert_eq!(parse_decimal("12.00 CRC", "CRC").unwrap(), 1200);
        assert_eq!(parse_decimal("12.00 USD", "USD").unwrap(), 1200);
    }

    #[test]
    fn rubbish_is_refused() {
        for bad in ["", "abc", "1.2.3", "--", "1..2", "12a34", "$"] {
            assert!(parse_decimal(bad, "USD").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn format_and_parse_round_trip() {
        for minor in [0, 1, -1, 99, -100, 123_456, -987_654_321] {
            assert_eq!(parse_decimal(&format_minor(minor, "USD"), "USD").unwrap(), minor);
        }
    }

    #[test]
    fn floats_from_an_api_land_on_the_cent() {
        assert_eq!(from_f64(12.34, "USD").unwrap(), 1234);
        assert_eq!(from_f64(0.1 + 0.2, "USD").unwrap(), 30);
        assert_eq!(from_f64(-4.5, "USD").unwrap(), -450);
        assert!(from_f64(f64::NAN, "USD").is_err());
    }
}
