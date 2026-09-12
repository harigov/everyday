//! Subscribed calendars, as the `.ics` files they arrived as.
//!
//! ```text
//!   calendar/
//!     Priya-Work.ics
//!     Term-dates.ics
//!     calendars.csv     where each one came from, and how often it refreshes
//! ```
//!
//! This is the one part whose format was never a choice. These records came
//! out of iCalendar, the core already has a reader for it, and writing them
//! back out as anything else would be translating a document into a second
//! language on the way out and back on the way in.
//!
//! What the `.ics` cannot say is where the calendar came from -- a URL that is
//! re-fetched every half hour, or a file somebody dropped in once -- and what
//! colour it is drawn in. That is `calendars.csv`, and it is what makes an
//! import restore a *subscription* rather than a frozen copy of one afternoon.

use super::index::{Namer, index_map};
use crate::text::{Csv, safe_name};
use crate::{Files, Landing, Mode, Options, Part, Portable, Report, Spec, land};
use everyday_core::calendar::{Calendar, CalendarOrigin, CalendarProvider};
use everyday_core::store::JournalStore;
use everyday_core::store::calendars::{CalendarStore, EventQuery};
use everyday_core::{CalendarId, Result, RoleId};
use std::collections::BTreeMap;

pub struct CalendarPart;
pub static CALENDAR: CalendarPart = CalendarPart;

static SPEC: Spec = Spec {
    id: "calendar",
    label: "Calendar",
    summary: "The calendars you subscribe to and the events read out of them, as the \
              iCalendar files any calendar program opens.",
    format: "iCalendar (.ics), one file per calendar",
    media: false,
    imports: true,
};

const INDEX: &str = "calendars.csv";
const COLUMNS: &[&str] = &[
    "name",
    "file",
    "origin",
    "url",
    "color",
    "provider",
    "visible",
    "refresh_minutes",
    "role_id",
    "id",
];

impl Portable for CalendarPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(calendars) = store.calendars() else { return Ok(None) };
        let mut total = 0;
        for calendar in calendars.list_calendars()? {
            total += calendars.count_events(calendar.id)?;
        }
        Ok(Some(total))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(calendars) = store.calendars() else { return Ok(()) };
        let mut index = Csv::new(COLUMNS);
        let mut namer = Namer::new();

        for calendar in calendars.list_calendars()? {
            let base = safe_name(&calendar.name);
            let stem = namer.unique(&base, |b, _| format!("{b}-{}", calendar.id.short()));
            let file = format!("{stem}.ics");

            let events = calendars.list_events(&EventQuery {
                calendar_id: Some(calendar.id),
                ..Default::default()
            })?;
            let mut ics = everyday_core::ics::Ics::new(&calendar.name);
            for event in &events {
                ics.event(event);
            }
            out.records(&file, ics.finish(), events.len() as u64)?;

            let (origin, url) = match &calendar.origin {
                CalendarOrigin::Url { url } => ("url", url.clone()),
                CalendarOrigin::File { label } => ("file", label.clone()),
            };
            index.row(&[
                calendar.name.clone(),
                file,
                origin.to_string(),
                url,
                calendar.color.clone(),
                calendar.provider.as_str().to_string(),
                calendar.visible.to_string(),
                calendar.refresh_minutes.to_string(),
                calendar.role_id.map(|r| r.to_string()).unwrap_or_default(),
                calendar.id.to_string(),
            ]);
        }
        if index.rows() > 0 {
            out.text(INDEX, index.finish())?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(calendars) = store.calendars() else {
            report.problem(SPEC.label, "this vault's backend does not store calendars");
            return Ok(report);
        };

        // The index tells each `.ics` what it is. A folder of `.ics` files
        // with no index -- somebody's export from another program -- still
        // imports: every file becomes a calendar named after itself.
        let described: BTreeMap<String, Calendar> = index_map(src.text(INDEX), |row| {
            Some((row.get("file").to_string(), calendar_from(row)))
        });

        for (name, body) in src.files() {
            if !name.ends_with(".ics") || name.starts_with("media/") {
                continue;
            }
            let calendar = described.get(name).cloned().unwrap_or_else(|| {
                Calendar::imported(name.trim_end_matches(".ics"), name.to_string())
            });
            if let Err(e) = read(calendars, calendar, body, mode, &mut report) {
                report.problem(name, e);
            }
        }
        Ok(report)
    }
}

fn calendar_from(row: &crate::text::Row<'_>) -> Calendar {
    // An id that will not parse mints a fresh one rather than dropping the
    // row, matching every other part: a hand-edited index is the ordinary
    // case for something a person is expected to open in a spreadsheet, and
    // an id column left blank or mistyped should cost that row its identity
    // across re-imports, not the calendar it names.
    let id = CalendarId::parse(row.get("id")).unwrap_or_else(|_| CalendarId::new());
    let name = row.get("name");
    let mut calendar = match row.get("origin") {
        "url" => Calendar::subscribed(name, row.get("url")),
        _ => Calendar::imported(name, row.get("url").to_string()),
    };
    calendar.id = id;
    calendar.color = row.get("color").to_string();
    calendar.provider = CalendarProvider::parse(row.get("provider")).unwrap_or_default();
    calendar.visible = row.get("visible").is_empty() || row.flag("visible");
    if let Some(minutes) = row.parse("refresh_minutes") {
        calendar.refresh_minutes = minutes;
    }
    calendar.role_id = RoleId::parse(row.get("role_id")).ok();
    calendar
}

/// Save the calendar and replace its events with what the file holds.
///
/// Events are replaced wholesale rather than merged, because that is what a
/// calendar *is* here: [`CalendarStore::replace_events`] is the same call a
/// refresh makes, and an import is a refresh from a file rather than from a
/// URL. Merging them would leave an event that the publisher has since
/// cancelled sitting on the grid for ever.
///
/// A calendar counts as one record and its events are not counted again
/// individually against it -- `report` gets both, through [`land`] for the
/// calendar itself and by arithmetic for the events, because there is no
/// per-event id in a file for `land` to look up.
fn read(
    calendars: &dyn CalendarStore,
    mut calendar: Calendar,
    body: &[u8],
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let text = std::str::from_utf8(body)
        .map_err(|_| everyday_core::Error::Invalid("this is not an iCalendar file".into()))?;

    let existing = calendars.get_calendar(calendar.id).ok();
    let landing = land(existing, || calendar.clone(), mode);
    match &landing {
        Landing::Skipped => {
            // The calendar was already here and the mode said to leave it,
            // so the file's events were never looked at. Counting them would
            // be reporting on work that did not happen.
            report.landed(&landing);
            return Ok(());
        }
        Landing::Existing(stored) => {
            // Keep what the vault knows and the file cannot: when it last
            // synced, and whether it was failing.
            calendar.created_at = stored.created_at;
            calendar.last_synced_at = stored.last_synced_at;
        }
        Landing::Fresh(_) => {}
    }
    let feed = everyday_core::ics::parse(text);
    if calendar.name.trim().is_empty() {
        calendar.name = feed.name.clone().unwrap_or_else(|| "Imported calendar".into());
    }
    calendars.put_calendar(&calendar)?;

    // The same window a refresh uses, centred on today, so an import puts as
    // much of a recurring series on the grid as a subscription would.
    let today = jiff::Zoned::now().date();
    let window = (
        today.saturating_sub(jiff::Span::new().months(6)),
        today.saturating_add(jiff::Span::new().months(18)),
    );
    let (events, _skipped) = everyday_core::ics::events_for(&calendar, &feed, window, "UTC");
    let count = events.len() as u64;

    // What `replace_events` is about to overwrite, asked before it does. The
    // dry run exists to say what an import will change, and reporting every
    // event of a re-imported calendar as "added" was the one number in it that
    // was not true. `min` because the two sets are the same calendar in two
    // revisions rather than two independent lists.
    let before = calendars.count_events(calendar.id).unwrap_or(0);
    let replaced = before.min(count);
    calendars.replace_events(calendar.id, &events)?;

    report.landed(&landing);
    report.added += count - replaced;
    report.replaced += replaced;
    Ok(())
}
