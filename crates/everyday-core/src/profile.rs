//! Who the vault belongs to.
//!
//! One record, one row, and nothing in the clear. It exists because the
//! assistant was being asked to be a secretary while knowing nothing about
//! the person it worked for: not their name, not their age, not what they do
//! all day, not which city's weather matters. Every one of those changes the
//! answer to an ordinary question, and none of them is something a model
//! should be guessing at from the contents of a journal.
//!
//! # What belongs here, and what belongs in a memory
//!
//! This is for the handful of things that do not change. A name, a birthday,
//! a gender, roughly where somebody lives, and a paragraph in their own words
//! about their work, their family and what they care about.
//!
//! Facts that *do* change are memories -- "I moved to Boston", "Ravi is my
//! brother-in-law now", "I have stopped drinking". The assistant writes those
//! itself, and they are capped, evictable and dated. Nothing writes this: it
//! is typed in Settings, by hand, once. That split is why there is no
//! `set_profile` tool, and it is deliberate rather than an omission.
//!
//! # Why it is not part of [`AgentSettings`]
//!
//! Because it is about the person and not the model, and because more than
//! the assistant will want it. A birthday is a calendar's business. A
//! location is the weather's. Putting it in the assistant's settings would
//! mean a vault with the assistant switched off had no owner.

use crate::{Error, Result};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Longest the free-text section may be.
///
/// Generous, because it is read into every prompt and a person who has
/// something to say about their family should be able to say it. Not
/// unbounded, for the same reason.
pub const MAX_ABOUT_BYTES: usize = 4_000;

/// Longest any of the short fields may be.
pub const MAX_FIELD_BYTES: usize = 200;

/// The owner of a vault. Every field may be empty; most vaults will fill in
/// two or three.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub first_name: String,
    pub last_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub born: Option<Date>,
    /// Free text, not a closed set.
    ///
    /// A drop-down here would be a small cruelty and a large argument, and
    /// nothing in the application branches on the value: it is one word in a
    /// sentence the assistant reads.
    pub gender: String,
    /// Roughly where they live. A city is the useful grain.
    pub location: String,
    /// Anything else worth knowing, in their own words.
    pub about: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        if self.about.len() > MAX_ABOUT_BYTES {
            return Err(Error::Invalid(format!(
                "the about section must be under {MAX_ABOUT_BYTES} bytes"
            )));
        }
        for (name, value) in [
            ("first name", &self.first_name),
            ("last name", &self.last_name),
            ("gender", &self.gender),
            ("location", &self.location),
        ] {
            if value.len() > MAX_FIELD_BYTES {
                return Err(Error::Invalid(format!(
                    "the {name} must be under {MAX_FIELD_BYTES} bytes"
                )));
            }
        }
        // A birthday in the future is a typo every time, and it would make
        // `age` negative in a sentence the model reads as fact.
        if let Some(born) = self.born
            && born > crate::model::today_local()
        {
            return Err(Error::Invalid("a date of birth cannot be in the future".into()));
        }
        Ok(())
    }

    /// Whether there is anything here worth putting in a prompt.
    pub fn is_empty(&self) -> bool {
        self.first_name.trim().is_empty()
            && self.last_name.trim().is_empty()
            && self.born.is_none()
            && self.gender.trim().is_empty()
            && self.location.trim().is_empty()
            && self.about.trim().is_empty()
    }

    pub fn name(&self) -> String {
        [self.first_name.trim(), self.last_name.trim()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Age in whole years on `today`.
    ///
    /// Computed rather than left for the model to work out. Asked to subtract
    /// two years a model will usually be right and will occasionally be a
    /// year out, and there is no reason to find out which in a sentence that
    /// reads as fact.
    pub fn age(&self, today: Date) -> Option<u32> {
        let born = self.born?;
        if born > today {
            return None;
        }
        let mut years = today.year() - born.year();
        // Not there yet this year. Comparing (month, day) rather than
        // constructing this year's birthday, which would panic on 29 February
        // in a year that has no such day.
        if (today.month(), today.day()) < (born.month(), born.day()) {
            years -= 1;
        }
        Some(years.max(0) as u32)
    }

    /// The sentence the assistant is told, or `None` if there is nothing to
    /// say. Kept here so the prompt builder and any test agree on the wording.
    pub fn describe(&self, today: Date) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        // "Its owner is", not "you are talking to": the house rules in
        // `system_prompt` have already said who the assistant is talking to,
        // and this section answers who that is.
        let mut out = String::from("Its owner is ");
        let name = self.name();
        out.push_str(if name.is_empty() { "someone who has not given a name" } else { &name });

        let mut facts: Vec<String> = Vec::new();
        if let Some(age) = self.age(today) {
            facts.push(format!("{age}"));
        }
        if !self.gender.trim().is_empty() {
            facts.push(self.gender.trim().to_string());
        }
        if !self.location.trim().is_empty() {
            facts.push(format!("in {}", self.location.trim()));
        }
        if !facts.is_empty() {
            out.push_str(", ");
            out.push_str(&facts.join(", "));
        }
        out.push('.');

        if !self.about.trim().is_empty() {
            out.push_str(" In their words: ");
            out.push_str(self.about.trim());
            if !self.about.trim().ends_with(['.', '!', '?']) {
                out.push('.');
            }
        }
        Some(out)
    }
}

/// Additional authenticated data for the sealed profile row.
pub fn profile_aad() -> Vec<u8> {
    b"everyday.profile.v1".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const TODAY: Date = date(2026, 9, 9);

    fn hari() -> Profile {
        Profile {
            first_name: "Hari".into(),
            last_name: "Govardhanam".into(),
            born: Some(date(1985, 3, 14)),
            gender: "male".into(),
            location: "Seattle".into(),
            about: "Software, two children, sailing at weekends".into(),
            updated_at: None,
        }
    }

    #[test]
    fn an_empty_profile_says_nothing_at_all() {
        let empty = Profile::default();
        assert!(empty.is_empty());
        assert_eq!(empty.describe(TODAY), None, "there is nothing to tell a model");
    }

    #[test]
    fn the_sentence_names_the_person_and_their_age() {
        let said = hari().describe(TODAY).expect("something to say");
        assert!(said.starts_with("Its owner is Hari Govardhanam"), "{said}");
        assert!(said.contains("41"), "the age is computed, not left to the model: {said}");
        assert!(said.contains("in Seattle"), "{said}");
        assert!(said.ends_with("sailing at weekends."), "a full stop is added: {said}");
    }

    #[test]
    fn a_birthday_that_has_not_come_round_yet_is_a_year_younger() {
        let mut p = Profile { born: Some(date(1985, 12, 25)), ..Profile::default() };
        assert_eq!(p.age(TODAY), Some(40), "December has not happened yet in September");
        p.born = Some(date(1985, 9, 9));
        assert_eq!(p.age(TODAY), Some(41), "a birthday today counts");
        p.born = Some(date(1985, 9, 10));
        assert_eq!(p.age(TODAY), Some(40), "tomorrow does not");
    }

    #[test]
    fn a_leap_day_birthday_has_an_age_in_every_year() {
        // The case that panics if this year's birthday is constructed rather
        // than compared: 2026 has no 29 February.
        let p = Profile { born: Some(date(2000, 2, 29)), ..Profile::default() };
        assert_eq!(p.age(date(2026, 2, 28)), Some(25));
        assert_eq!(p.age(date(2026, 3, 1)), Some(26));
    }

    #[test]
    fn a_profile_with_only_a_name_still_says_something() {
        let p = Profile { first_name: "Alex".into(), ..Profile::default() };
        assert_eq!(p.describe(TODAY).as_deref(), Some("Its owner is Alex."));
    }

    #[test]
    fn a_nameless_profile_is_described_without_inventing_one() {
        let p = Profile { location: "Chennai".into(), ..Profile::default() };
        let said = p.describe(TODAY).unwrap();
        assert!(
            said.starts_with("Its owner is someone who has not given a name, in Chennai"),
            "{said}"
        );
    }

    #[test]
    fn a_birthday_in_the_future_is_refused() {
        let p = Profile { born: Some(date(3000, 1, 1)), ..Profile::default() };
        assert!(p.validate().is_err(), "it is a typo every time");
        assert_eq!(p.age(TODAY), None, "and it must not read as a negative age");
    }

    #[test]
    fn an_about_section_longer_than_a_paragraph_or_two_is_refused() {
        let mut p = Profile { about: "x".repeat(MAX_ABOUT_BYTES), ..Profile::default() };
        p.validate().expect("at the limit");
        p.about.push('x');
        assert!(p.validate().is_err(), "it is read into every prompt");
    }
}
