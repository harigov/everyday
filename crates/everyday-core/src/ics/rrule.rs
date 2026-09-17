use jiff::civil::{Date, DateTime, Weekday};

use super::read::{Line, parse_moment};
use super::{EXPANSION_CAP, Freq, Rrule};

fn parse_weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}
/// Parse an `RRULE` value into the parts this reader expands.
pub fn parse_rrule(value: &str) -> Rrule {
    let mut rule = Rrule::default();
    let mut saw_freq = false;
    for part in value.split(';') {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        let val = val.trim();
        match key.as_str() {
            "FREQ" => {
                saw_freq = true;
                match val.to_ascii_uppercase().as_str() {
                    "DAILY" => rule.freq = Freq::Daily,
                    "WEEKLY" => rule.freq = Freq::Weekly,
                    "MONTHLY" => rule.freq = Freq::Monthly,
                    "YEARLY" => rule.freq = Freq::Yearly,
                    // HOURLY, MINUTELY, SECONDLY. Legal, and not a thing a
                    // human calendar contains; expanding one at the cap
                    // would be 1,500 rows of noise.
                    _ => rule.unsupported = true,
                }
            }
            "INTERVAL" => rule.interval = val.parse().unwrap_or(1).max(1),
            "COUNT" => rule.count = val.parse().ok(),
            "UNTIL" => {
                let line = Line { name: "UNTIL".into(), params: Vec::new(), value: val };
                rule.until = parse_moment(&line, val).map(|m| m.at);
            }
            "BYDAY" => {
                for token in val.split(',') {
                    let token = token.trim();
                    let split = token.len().saturating_sub(2);
                    let (ord, code) = token.split_at(split);
                    let Some(weekday) = parse_weekday(&code.to_ascii_uppercase()) else {
                        continue;
                    };
                    let nth = if ord.is_empty() { 0 } else { ord.parse::<i8>().unwrap_or(0) };
                    rule.by_day.push((nth, weekday));
                }
            }
            "BYMONTHDAY" => {
                rule.by_month_day.extend(val.split(',').filter_map(|d| d.trim().parse::<i8>().ok()))
            }
            "BYMONTH" => {
                rule.by_month.extend(val.split(',').filter_map(|d| d.trim().parse::<i8>().ok()))
            }
            // Legal parts this reader cannot expand faithfully. Flagged
            // rather than ignored, so the caller can be honest about it.
            "BYSETPOS" | "BYWEEKNO" | "BYYEARDAY" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                rule.unsupported = true
            }
            _ => {}
        }
    }
    if !saw_freq {
        rule.unsupported = true;
    }
    rule
}
// ── Expansion ────────────────────────────────────────────────────────────
/// Every local start this rule produces, from `seed`, clipped to `window`.
///
/// Walks candidate days forward from the seed rather than generating a
/// closed form. That is slower and very much clearer, and the cost is
/// bounded twice over: by the window, and by [`EXPANSION_CAP`].
pub(super) fn occurrences(
    seed: DateTime,
    rule: &Rrule,
    window: (DateTime, DateTime),
) -> Vec<DateTime> {
    let mut out = Vec::new();
    if rule.unsupported {
        return vec![seed];
    }
    let rule = &seeded(seed, rule);
    let (from, to) = window;
    let stop = match rule.until {
        Some(until) if until < to => until,
        _ => to,
    };
    let time = seed.time();
    let interval = i32::try_from(rule.interval).unwrap_or(1).max(1);

    // A rule with no COUNT and no UNTIL runs forever, so the window is the
    // only bound; with a COUNT, occurrences before the window still consume
    // it, so counting has to start at the seed either way.
    let mut emitted = 0u32;
    let mut cursor = seed.date();
    let mut steps = 0usize;

    let matches_by_day = |d: Date| -> bool {
        if rule.by_day.is_empty() {
            return true;
        }
        rule.by_day.iter().any(|(nth, weekday)| {
            if d.weekday() != *weekday {
                return false;
            }
            match nth {
                0 => true,
                n if *n > 0 => (d.day() - 1) / 7 + 1 == *n,
                n => {
                    let last = d.last_of_month().day();
                    -((last - d.day()) / 7 + 1) == *n
                }
            }
        })
    };
    let matches_month_day = |d: Date| -> bool {
        rule.by_month_day.is_empty()
            || rule.by_month_day.iter().any(|n| {
                let last = d.last_of_month().day();
                let want = if *n < 0 { last + n + 1 } else { *n };
                want == d.day()
            })
    };
    let matches_month =
        |d: Date| -> bool { rule.by_month.is_empty() || rule.by_month.contains(&d.month()) };

    // Days the cursor advances by between candidate windows, and how wide a
    // window each step opens.
    while steps < EXPANSION_CAP * 8 && out.len() < EXPANSION_CAP {
        steps += 1;
        if cursor.to_datetime(time) > stop {
            break;
        }
        if let Some(max) = rule.count
            && emitted >= max
        {
            break;
        }

        // Candidate days within this period of the rule.
        let candidates: Vec<Date> = match rule.freq {
            Freq::Daily => vec![cursor],
            Freq::Weekly => {
                if rule.by_day.is_empty() {
                    vec![cursor]
                } else {
                    // The whole week the cursor sits in, Monday first, so a
                    // `BYDAY=MO,WE,FR` rule emits in calendar order.
                    let back = i32::from(cursor.weekday().to_monday_zero_offset());
                    let Ok(monday) = cursor.checked_add(days(-back)) else {
                        break;
                    };
                    (0..7).filter_map(|i| monday.checked_add(days(i)).ok()).collect()
                }
            }
            Freq::Monthly => {
                let first = cursor.first_of_month();
                (0..first.days_in_month())
                    .filter_map(|i| first.checked_add(days(i.into())).ok())
                    .collect()
            }
            Freq::Yearly => {
                let first = cursor.first_of_year();
                (0..first.days_in_year())
                    .filter_map(|i| first.checked_add(days(i.into())).ok())
                    .collect()
            }
        };

        for day in candidates {
            if day < seed.date() {
                continue;
            }
            let at = day.to_datetime(time);
            if at > stop {
                break;
            }
            if !(matches_by_day(day) && matches_month_day(day) && matches_month(day)) {
                continue;
            }
            emitted += 1;
            if let Some(max) = rule.count
                && emitted > max
            {
                break;
            }
            if at >= from {
                out.push(at);
            }
            if out.len() >= EXPANSION_CAP {
                break;
            }
        }

        // Advance one period.
        let next = match rule.freq {
            Freq::Daily => cursor.checked_add(days(interval)),
            Freq::Weekly => cursor.checked_add(days(interval * 7)),
            Freq::Monthly => {
                cursor.first_of_month().checked_add(jiff::Span::new().months(interval))
            }
            Freq::Yearly => cursor.first_of_year().checked_add(jiff::Span::new().years(interval)),
        };
        let Ok(next) = next else { break };
        cursor = next;
    }

    out.sort();
    out.dedup();
    out
}
/// Fill in the parts of a rule that RFC 5545 says come from `DTSTART`.
///
/// "`FREQ=MONTHLY`" alone means *this day of every month*, not *every day of
/// every month*, and "`FREQ=YEARLY`" means *this date every year*, not *the
/// first of January*. Both read as "no constraint" if the missing parts are
/// left empty, which is how a monthly one-to-one turns into thirty meetings.
/// The defaults are applied once, here, rather than in each arm of the
/// expansion, so there is one place to check them against the spec.
fn seeded(seed: DateTime, rule: &Rrule) -> Rrule {
    let mut rule = rule.clone();
    let day = seed.date().day();
    match rule.freq {
        Freq::Monthly if rule.by_day.is_empty() && rule.by_month_day.is_empty() => {
            rule.by_month_day = vec![day];
        }
        Freq::Yearly if rule.by_day.is_empty() && rule.by_month_day.is_empty() => {
            rule.by_month_day = vec![day];
            if rule.by_month.is_empty() {
                rule.by_month = vec![seed.date().month()];
            }
        }
        // DAILY and WEEKLY already walk day by day and week by week from the
        // seed, so an absent BYDAY genuinely does mean "no constraint".
        _ => {}
    }
    rule
}
fn days(n: i32) -> jiff::Span {
    jiff::Span::new().days(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{Time, date, datetime};

    // ── recurrence ───────────────────────────────────────────────────────

    fn expand(rrule: &str, seed: DateTime, from: Date, to: Date) -> Vec<Date> {
        let rule = parse_rrule(rrule);
        occurrences(seed, &rule, (from.to_datetime(Time::midnight()), to.to_datetime(Time::MAX)))
            .into_iter()
            .map(|d| d.date())
            .collect()
    }

    #[test]
    fn a_weekly_rule_lands_on_the_days_it_names() {
        let out = expand(
            "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            datetime(2026, 9, 7, 9, 0, 0, 0), // a Monday
            date(2026, 9, 7),
            date(2026, 9, 20),
        );
        assert_eq!(
            out,
            [
                date(2026, 9, 7),
                date(2026, 9, 9),
                date(2026, 9, 11),
                date(2026, 9, 14),
                date(2026, 9, 16),
                date(2026, 9, 18),
            ],
        );
    }

    #[test]
    fn interval_skips_periods_rather_than_occurrences() {
        let out = expand(
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU",
            datetime(2026, 9, 8, 9, 0, 0, 0), // a Tuesday
            date(2026, 9, 1),
            date(2026, 10, 15),
        );
        assert_eq!(out, [date(2026, 9, 8), date(2026, 9, 22), date(2026, 10, 6)]);
    }

    #[test]
    fn an_ordinal_byday_finds_the_third_thursday_and_the_last_friday() {
        let third = expand(
            "FREQ=MONTHLY;BYDAY=3TH",
            datetime(2026, 9, 17, 18, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(
            third,
            [date(2026, 9, 17), date(2026, 10, 15), date(2026, 11, 19), date(2026, 12, 17)]
        );

        let last = expand(
            "FREQ=MONTHLY;BYDAY=-1FR",
            datetime(2026, 9, 25, 16, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 11, 30),
        );
        assert_eq!(last, [date(2026, 9, 25), date(2026, 10, 30), date(2026, 11, 27)]);
    }

    #[test]
    fn count_is_consumed_by_occurrences_before_the_window_too() {
        // The bug this guards: asking for October and getting five more
        // dailies out of a rule that was only ever meant to fire three times
        // in September.
        let out = expand(
            "FREQ=DAILY;COUNT=3",
            datetime(2026, 9, 1, 9, 0, 0, 0),
            date(2026, 9, 3),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 3)]);
    }

    #[test]
    fn until_is_inclusive_and_stops_the_series() {
        let out = expand(
            "FREQ=DAILY;UNTIL=20260904T090000Z",
            datetime(2026, 9, 1, 9, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 1), date(2026, 9, 2), date(2026, 9, 3), date(2026, 9, 4)]);
    }

    #[test]
    fn a_yearly_rule_keeps_its_day() {
        let out = expand(
            "FREQ=YEARLY",
            datetime(2026, 3, 14, 0, 0, 0, 0),
            date(2026, 1, 1),
            date(2029, 12, 31),
        );
        assert_eq!(
            out,
            [date(2026, 3, 14), date(2027, 3, 14), date(2028, 3, 14), date(2029, 3, 14)]
        );
    }

    #[test]
    fn a_bare_monthly_rule_means_this_day_of_the_month_not_every_day_of_it() {
        // The bug this guards: `FREQ=MONTHLY` with no BY parts reading as
        // "no constraint", which turns a monthly one-to-one into thirty
        // meetings.
        let out = expand(
            "FREQ=MONTHLY",
            datetime(2026, 9, 15, 11, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(
            out,
            [date(2026, 9, 15), date(2026, 10, 15), date(2026, 11, 15), date(2026, 12, 15)],
        );
    }

    #[test]
    fn a_monthly_rule_on_the_31st_skips_the_months_that_have_no_31st() {
        let out = expand(
            "FREQ=MONTHLY",
            datetime(2026, 1, 31, 9, 0, 0, 0),
            date(2026, 1, 1),
            date(2026, 5, 31),
        );
        assert_eq!(
            out,
            [date(2026, 1, 31), date(2026, 3, 31), date(2026, 5, 31)],
            "February and April have no 31st, and inventing one is worse than skipping it",
        );
    }

    #[test]
    fn a_bare_yearly_rule_keeps_the_month_as_well_as_the_day() {
        // The bug this guards: the cursor jumping to the first of January
        // and the rule then matching it, so a birthday moves to New Year.
        let out = expand(
            "FREQ=YEARLY",
            datetime(2026, 3, 14, 0, 0, 0, 0),
            date(2026, 1, 1),
            date(2028, 12, 31),
        );
        assert_eq!(out, [date(2026, 3, 14), date(2027, 3, 14), date(2028, 3, 14)]);
    }

    #[test]
    fn a_rule_this_reader_cannot_expand_yields_its_seed_and_says_so() {
        let rule = parse_rrule("FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO,TU,WE,TH,FR");
        assert!(rule.unsupported, "BYSETPOS is not expanded and must be flagged");
        let out = expand(
            "FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO",
            datetime(2026, 9, 28, 9, 0, 0, 0),
            date(2026, 1, 1),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 28)], "one honest occurrence beats twelve invented ones");
    }

    #[test]
    fn an_endless_daily_rule_is_bounded_by_the_window_not_by_time() {
        let out = expand(
            "FREQ=DAILY",
            datetime(2026, 1, 1, 9, 0, 0, 0),
            date(2026, 3, 1),
            date(2026, 3, 31),
        );
        assert_eq!(out.len(), 31);
        assert_eq!(out[0], date(2026, 3, 1));
    }
}
