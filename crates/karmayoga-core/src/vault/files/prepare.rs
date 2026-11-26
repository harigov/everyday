//! Changing what the person added: one [`FileUpdate`] checked once, then applied to each file.

use super::helpers::same;
use super::{MAX_PLACE_LEN, MAX_PROPERTY_LIST, PROP_TAKEN_BY_FILE};
use crate::error::{Error, Result};
use crate::files::{
    FileEntry, FileUpdate, MAX_CAPTION_LEN, MAX_PEOPLE, MAX_PROPERTIES, MAX_PROPERTY_TEXT_LEN,
    MAX_TAG_LEN, MAX_TAGS, PropValue, valid_property_key,
};
use jiff::Timestamp;
use jiff::civil;
use jiff::tz::TimeZone;

/// A [`FileUpdate`] that has been checked on its own, ready to apply to each
/// file: what does not depend on a file is decided once.
pub(super) struct Prepared<'a> {
    update: &'a FileUpdate,
    tags: Option<Vec<String>>,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
    people: Option<Vec<String>>,
    add_people: Vec<String>,
    remove_people: Vec<String>,
    place: Option<Option<String>>,
    rating: Option<Option<u8>>,
    /// `Some(None)` is "back to what the file says".
    taken_at: Option<Option<Timestamp>>,
}

impl<'a> Prepared<'a> {
    pub(super) fn new(update: &'a FileUpdate) -> Result<Self> {
        if let Some(caption) = &update.caption
            && caption.chars().count() > MAX_CAPTION_LEN
        {
            return Err(Error::Invalid(format!(
                "a caption is at most {MAX_CAPTION_LEN} characters"
            )));
        }
        let place = match &update.place {
            None => None,
            Some(p) => {
                let p = p.trim();
                if p.chars().count() > MAX_PLACE_LEN {
                    return Err(Error::Invalid(format!(
                        "a place's name is at most {MAX_PLACE_LEN} characters"
                    )));
                }
                Some((!p.is_empty()).then(|| p.to_string()))
            }
        };
        let rating = match update.rating {
            None => None,
            Some(0) => Some(None),
            Some(r @ 1..=5) => Some(Some(r)),
            Some(r) => {
                return Err(Error::Invalid(format!("a rating is 1 to 5, or 0 to clear; not {r}")));
            }
        };
        for (key, value) in &update.properties {
            if !valid_property_key(key) {
                return Err(Error::Invalid(format!(
                    "{key:?} is not a property key: lowercase words joined by dots, at least two"
                )));
            }
            if let Some(value) = value {
                check_property(key, value)?;
            }
        }
        let taken_at = match update.taken_at.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(text) => Some(Some(parse_taken(text)?)),
        };
        Ok(Self {
            update,
            tags: update
                .tags
                .as_deref()
                .map(|l| clean_list(l, MAX_TAG_LEN, "a tag"))
                .transpose()?,
            add_tags: clean_list(&update.add_tags, MAX_TAG_LEN, "a tag")?,
            remove_tags: clean_list(&update.remove_tags, MAX_TAG_LEN, "a tag")?,
            people: update
                .people
                .as_deref()
                .map(|l| clean_list(l, MAX_TAG_LEN, "a person's name"))
                .transpose()?,
            add_people: clean_list(&update.add_people, MAX_TAG_LEN, "a person's name")?,
            remove_people: clean_list(&update.remove_people, MAX_TAG_LEN, "a person's name")?,
            place,
            rating,
            taken_at,
        })
    }

    pub(super) fn apply(&self, f: &mut FileEntry) -> Result<()> {
        let u = self.update;
        if let Some(caption) = &u.caption {
            f.caption.clone_from(caption);
        }
        f.tags = merged(&f.tags, &self.tags, &self.add_tags, &self.remove_tags);
        if f.tags.len() > MAX_TAGS {
            return Err(Error::Invalid(format!("a file has at most {MAX_TAGS} tags")));
        }
        f.people = merged(&f.people, &self.people, &self.add_people, &self.remove_people);
        if f.people.len() > MAX_PEOPLE {
            return Err(Error::Invalid(format!("a file has at most {MAX_PEOPLE} people")));
        }
        if let Some(rating) = self.rating {
            f.rating = rating;
        }
        if let Some(favourite) = u.favourite {
            f.favourite = favourite;
        }
        if let Some(hidden) = u.hidden {
            f.hidden = hidden;
        }
        if let Some(place) = &self.place {
            f.place.clone_from(place);
        }
        for (key, value) in &u.properties {
            match value {
                Some(value) => {
                    f.properties.insert(key.clone(), value.clone());
                }
                None => {
                    f.properties.remove(key);
                }
            }
        }
        if f.properties.len() > MAX_PROPERTIES {
            return Err(Error::Invalid(format!("a file has at most {MAX_PROPERTIES} properties")));
        }
        match self.taken_at {
            Some(Some(corrected)) => {
                // The file's own time is kept the first time it is
                // corrected, so clearing the correction can go back to it.
                f.properties.entry(PROP_TAKEN_BY_FILE.into()).or_insert_with(|| {
                    PropValue::Text(f.media.taken_at.map(|t| t.to_string()).unwrap_or_default())
                });
                f.media.taken_at = Some(corrected);
                f.media.taken_local = None;
            }
            Some(None) => {
                if let Some(PropValue::Text(by_file)) = f.properties.remove(PROP_TAKEN_BY_FILE) {
                    f.media.taken_at = by_file.parse().ok();
                    f.media.taken_local = None;
                }
            }
            None => {}
        }
        Ok(())
    }
}

/// Trimmed, empty ones dropped, each within `max` characters, repeats
/// (without regard to case) dropped -- the first spelling wins.
pub(super) fn clean_list(items: &[String], max: usize, what: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.chars().count() > max {
            return Err(Error::Invalid(format!("{what} is at most {max} characters")));
        }
        if !out.iter().any(|x| same(x, item)) {
            out.push(item.to_string());
        }
    }
    Ok(out)
}

/// The list a change leaves: the replacement (or what the file has), with
/// `add` appended where not already there, and `remove` taken out.
pub(super) fn merged(
    current: &[String],
    replace: &Option<Vec<String>>,
    add: &[String],
    remove: &[String],
) -> Vec<String> {
    let mut list = replace.clone().unwrap_or_else(|| current.to_vec());
    for item in add {
        if !list.iter().any(|x| same(x, item)) {
            list.push(item.clone());
        }
    }
    list.retain(|x| !remove.iter().any(|r| same(x, r)));
    list
}

pub(super) fn check_property(key: &str, value: &PropValue) -> Result<()> {
    let too_long = |text: &str| text.chars().count() > MAX_PROPERTY_TEXT_LEN;
    let bad = match value {
        PropValue::Text(t) => too_long(t),
        PropValue::Number(n) => !n.is_finite(),
        PropValue::Bool(_) => false,
        PropValue::Date(d) => {
            if d.parse::<civil::Date>().is_err() && d.parse::<Timestamp>().is_err() {
                return Err(Error::Invalid(format!(
                    "{key}: a date is YYYY-MM-DD or an RFC 3339 time, not {d:?}"
                )));
            }
            false
        }
        PropValue::List(items) => {
            items.len() > MAX_PROPERTY_LIST || items.iter().any(|i| too_long(i))
        }
    };
    if bad {
        return Err(Error::Invalid(format!("{key}: that value is not allowed")));
    }
    Ok(())
}

/// When a picture was taken, as somebody types it: an RFC 3339 time, a local
/// date and time (`2024-03-01T14:22:05`), or a date alone, which is noon of
/// that day so no time zone can move it to another.
pub(super) fn parse_taken(text: &str) -> Result<Timestamp> {
    let zone = TimeZone::system();
    if let Ok(t) = text.parse::<Timestamp>() {
        return Ok(t);
    }
    if let Ok(dt) = text.parse::<civil::DateTime>()
        && let Ok(z) = dt.to_zoned(zone.clone())
    {
        return Ok(z.timestamp());
    }
    if let Ok(date) = text.parse::<civil::Date>()
        && let Ok(z) = date.at(12, 0, 0, 0).to_zoned(zone)
    {
        return Ok(z.timestamp());
    }
    Err(Error::Invalid(format!(
        "{text:?} is not a time: use YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS or an RFC 3339 time"
    )))
}
