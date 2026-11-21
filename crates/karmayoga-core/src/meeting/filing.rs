//! Where a meeting note is filed: under its calendar's area, automatically.
//!
//! `docs/plans/meeting-notes.md`'s Phase 3 settles this once, plainly: "a
//! meeting note is filed under its calendar's area automatically". A
//! calendar already carries [`Calendar::area_id`] -- the same field a
//! [`Trigger::BeforeEvent`](crate::routine::Trigger::BeforeEvent) area
//! narrows on -- so there is nothing to ask the person and nothing to
//! infer: the note's [`Purpose`] is the calendar's area, or none at all
//! when the calendar has never been given one.
//!
//! Deliberately not a [`Purpose::Goal`]: a calendar is a whole feed, not one
//! outcome under an area, the same argument [`Calendar::area_id`]'s own doc
//! makes for why a feed is attributed by area rather than by goal.

use crate::calendar::Calendar;
use crate::purpose::Purpose;

/// The purpose a meeting note on this calendar should be filed under.
///
/// `None` for no calendar (a call that was not on the calendar) or a
/// calendar with no area set -- both leave the note unfiled, exactly as a
/// note written by hand starts out.
pub fn purpose_for(calendar: Option<&Calendar>) -> Option<Purpose> {
    let id = calendar?.area_id?;
    Some(Purpose::Area { id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::AreaId;

    fn calendar(area_id: Option<AreaId>) -> Calendar {
        let mut c = Calendar::subscribed("Work", "https://example.com/work.ics");
        c.area_id = area_id;
        c
    }

    #[test]
    fn a_calendar_with_a_area_files_a_note_under_it() {
        let area = AreaId::new();
        let purpose = purpose_for(Some(&calendar(Some(area))));
        assert_eq!(purpose, Some(Purpose::Area { id: area }));
    }

    #[test]
    fn a_calendar_with_no_area_files_nothing() {
        assert_eq!(purpose_for(Some(&calendar(None))), None);
    }

    #[test]
    fn no_calendar_at_all_files_nothing() {
        assert_eq!(purpose_for(None), None);
    }
}
