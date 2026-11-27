//! Finding what repeats: subscriptions, rent, a paycheck.
//!
//! Pure. A run of transactions to the same payee (the shown payee,
//! case-folded), in the same account, at a steady interval and a steady
//! amount is a recurring charge. Steady means: at least three occurrences
//! (two for yearly), intervals within a tolerance of one of the cadences
//! below, and amounts within 20% of their median. The next date is the last
//! plus the cadence. A series whose next date is more than one full cadence
//! in the past has stopped and is not reported.
//!
//! # How a run is judged
//!
//! Transactions are grouped by account, shown payee (trimmed, lower-cased),
//! direction and currency -- a refund from a shop is not part of its
//! subscription, and a paycheck is not a bill. Within a group:
//!
//! 1. Amounts: the group's median amount is taken, and the transactions
//!    within a fifth of it are the series. At least three in four of the
//!    group must be; a payee whose amounts wander (a supermarket) is not a
//!    subscription however often it is visited, while one odd charge at a
//!    payee that is otherwise steady (an annual upgrade beside a monthly
//!    plan) is left out rather than hiding the plan.
//! 2. Intervals: the days between consecutive members of the series. Their
//!    median picks the cadence whose tolerance holds it, and at least three
//!    in four intervals must fall within that tolerance -- one skipped month
//!    does not end a subscription, but a haircut every four to seven weeks
//!    is not monthly.
//!
//! Everything is integer arithmetic over dates and minor units, and every
//! ordering is total, so the same transactions always give the same answer.

use super::Txn;
use crate::id::{BankAccountId, MoneyCategoryId};
use jiff::Span;
use jiff::civil::Date;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Cadence {
    Weekly,
    Biweekly,
    Monthly,
    Quarterly,
    Yearly,
}

impl Cadence {
    const ALL: [Cadence; 5] =
        [Cadence::Weekly, Cadence::Biweekly, Cadence::Monthly, Cadence::Quarterly, Cadence::Yearly];

    /// The days between two occurrences that still count as this cadence,
    /// inclusive. Wide enough for a weekend or a short month moving a
    /// charge, and no two overlap, so an interval names at most one.
    fn tolerance(self) -> (i32, i32) {
        match self {
            Cadence::Weekly => (6, 8),
            Cadence::Biweekly => (13, 16),
            Cadence::Monthly => (26, 35),
            Cadence::Quarterly => (85, 97),
            Cadence::Yearly => (350, 380),
        }
    }

    fn holds(self, days: i32) -> bool {
        let (low, high) = self.tolerance();
        (low..=high).contains(&days)
    }

    /// One step of this cadence, in calendar units: a month from the 31st
    /// lands on the last day of a shorter month rather than spilling over.
    fn step(self) -> Span {
        match self {
            Cadence::Weekly => Span::new().days(7),
            Cadence::Biweekly => Span::new().days(14),
            Cadence::Monthly => Span::new().months(1),
            Cadence::Quarterly => Span::new().months(3),
            Cadence::Yearly => Span::new().years(1),
        }
    }

    /// How many occurrences it takes to call a run this cadence. Two for a
    /// yearly charge, or nobody would see one until its third year.
    fn min_count(self) -> usize {
        match self {
            Cadence::Yearly => 2,
            _ => 3,
        }
    }

    fn for_interval(days: i32) -> Option<Cadence> {
        Cadence::ALL.into_iter().find(|c| c.holds(days))
    }
}

/// One thing that repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recurring {
    /// As shown: the person's name for the payee, else the bank's.
    pub payee: String,
    pub account_id: BankAccountId,
    /// The median amount, signed as transactions are.
    pub amount: i64,
    pub currency: String,
    pub cadence: Cadence,
    pub count: u32,
    pub last: Date,
    pub next: Date,
    /// The category the most recent one has.
    pub category: Option<MoneyCategoryId>,
}

/// Which transactions might be one series: account, shown payee folded,
/// money out or in, currency.
type GroupKey = (BankAccountId, String, bool, String);

/// Every recurring series in `txns` still going as of `today`, soonest
/// `next` first. Excluded transactions are ignored.
pub fn detect(txns: &[Txn], today: Date) -> Vec<Recurring> {
    let mut groups: BTreeMap<GroupKey, Vec<&Txn>> = BTreeMap::new();
    for txn in txns {
        if txn.excluded || txn.amount == 0 {
            continue;
        }
        let payee = txn.shown_payee().trim().to_lowercase();
        if payee.is_empty() {
            continue;
        }
        let key = (txn.account_id, payee, txn.amount < 0, txn.currency.trim().to_ascii_uppercase());
        groups.entry(key).or_default().push(txn);
    }

    let mut found: Vec<Recurring> =
        groups.into_values().filter_map(|mut run| series(&mut run, today)).collect();
    found.sort_by(|a, b| {
        a.next
            .cmp(&b.next)
            .then_with(|| a.payee.cmp(&b.payee))
            .then(a.account_id.cmp(&b.account_id))
            .then(a.amount.cmp(&b.amount))
    });
    found
}

/// Judge one group, as the module doc describes.
fn series(run: &mut [&Txn], today: Date) -> Option<Recurring> {
    if run.len() < 2 {
        return None;
    }
    run.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));

    let typical = median(run.iter().map(|t| t.amount).collect())?;
    let steady: Vec<&Txn> =
        run.iter().copied().filter(|t| within_a_fifth(t.amount, typical)).collect();
    if steady.len() < 2 || !most(steady.len(), run.len()) {
        return None;
    }

    let intervals: Vec<i32> =
        steady.windows(2).map(|pair| (pair[1].date - pair[0].date).get_days()).collect();
    let cadence = Cadence::for_interval(median_days(&intervals)?)?;
    if steady.len() < cadence.min_count() {
        return None;
    }
    let regular = intervals.iter().filter(|days| cadence.holds(**days)).count();
    if !most(regular, intervals.len()) {
        return None;
    }

    let latest = *steady.last()?;
    let next = latest.date.checked_add(cadence.step()).ok()?;
    let given_up = next.checked_add(cadence.step()).ok()?;
    if given_up < today {
        return None;
    }
    Some(Recurring {
        payee: latest.shown_payee().trim().to_string(),
        account_id: latest.account_id,
        amount: median(steady.iter().map(|t| t.amount).collect())?,
        currency: latest.currency.trim().to_ascii_uppercase(),
        cadence,
        count: u32::try_from(steady.len()).unwrap_or(u32::MAX),
        last: latest.date,
        next,
        category: latest.category,
    })
}

/// At least three in four.
fn most(part: usize, whole: usize) -> bool {
    whole > 0 && part.saturating_mul(4) >= whole.saturating_mul(3)
}

/// Is `amount` within 20% of `typical`? In 128 bits, so no amount a bank
/// could send overflows the comparison.
fn within_a_fifth(amount: i64, typical: i64) -> bool {
    let difference = (i128::from(amount) - i128::from(typical)).abs();
    difference * 5 <= i128::from(typical).abs()
}

/// The median, the lower-middle mean for an even count. `None` when empty.
fn median(mut values: Vec<i64>) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if !values.len().is_multiple_of(2) {
        return values.get(mid).copied();
    }
    let (low, high) = (i128::from(*values.get(mid - 1)?), i128::from(*values.get(mid)?));
    // The mean of two i64s is always an i64.
    i64::try_from((low + high).div_euclid(2)).ok()
}

fn median_days(intervals: &[i32]) -> Option<i32> {
    let as_wide = median(intervals.iter().map(|d| i64::from(*d)).collect())?;
    i32::try_from(as_wide).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Date = Date::constant(2026, 11, 5);

    fn charge(account: BankAccountId, payee: &str, date: Date, amount: i64) -> Txn {
        let mut t = Txn::new(account, "k", date, amount, "USD");
        t.payee = payee.into();
        t
    }

    fn run(payee: &str, amounts_and_dates: &[(i64, Date)]) -> Vec<Txn> {
        let account = BankAccountId::new();
        amounts_and_dates.iter().map(|(a, d)| charge(account, payee, *d, *a)).collect()
    }

    fn d(y: i16, m: i8, day: i8) -> Date {
        Date::constant(y, m, day)
    }

    #[test]
    fn a_monthly_subscription_is_found_with_its_next_date() {
        let txns = run(
            "Netflix",
            &[
                (-1599, d(2026, 7, 3)),
                (-1599, d(2026, 8, 3)),
                (-1599, d(2026, 9, 3)),
                (-1599, d(2026, 10, 3)),
            ],
        );
        let found = detect(&txns, TODAY);
        assert_eq!(found.len(), 1);
        let r = &found[0];
        assert_eq!(r.cadence, Cadence::Monthly);
        assert_eq!(r.amount, -1599);
        assert_eq!(r.count, 4);
        assert_eq!(r.last, d(2026, 10, 3));
        assert_eq!(r.next, d(2026, 11, 3));
        assert_eq!(r.payee, "Netflix");
    }

    #[test]
    fn weekly_biweekly_quarterly_and_yearly_cadences_are_told_apart() {
        let weekly = run(
            "Farm box",
            &[
                (-3000, d(2026, 10, 1)),
                (-3000, d(2026, 10, 8)),
                (-3000, d(2026, 10, 15)),
                (-3000, d(2026, 10, 22)),
                (-3000, d(2026, 10, 29)),
            ],
        );
        let biweekly = run(
            "ACME PAYROLL",
            &[
                (250_000, d(2026, 9, 4)),
                (250_000, d(2026, 9, 18)),
                (250_000, d(2026, 10, 2)),
                (250_000, d(2026, 10, 16)),
                (250_000, d(2026, 10, 30)),
            ],
        );
        let quarterly = run(
            "Water board",
            &[(-9000, d(2026, 2, 15)), (-9300, d(2026, 5, 15)), (-8800, d(2026, 8, 15))],
        );
        let yearly = run("Domain renewal", &[(-1200, d(2024, 11, 20)), (-1200, d(2025, 11, 20))]);
        let all: Vec<Txn> = [weekly, biweekly, quarterly, yearly].concat();
        let found = detect(&all, TODAY);
        let by_payee = |p: &str| found.iter().find(|r| r.payee == p).map(|r| (r.cadence, r.next));
        assert_eq!(by_payee("Farm box"), Some((Cadence::Weekly, d(2026, 11, 5))));
        assert_eq!(by_payee("ACME PAYROLL"), Some((Cadence::Biweekly, d(2026, 11, 13))));
        assert_eq!(by_payee("Water board"), Some((Cadence::Quarterly, d(2026, 11, 15))));
        assert_eq!(by_payee("Domain renewal"), Some((Cadence::Yearly, d(2026, 11, 20))));
        assert_eq!(found.len(), 4);
        let nexts: Vec<Date> = found.iter().map(|r| r.next).collect();
        let mut sorted = nexts.clone();
        sorted.sort();
        assert_eq!(nexts, sorted, "soonest first");
    }

    #[test]
    fn a_series_that_stopped_is_not_reported() {
        // Next would have been 3 June; a full month past that is 3 July,
        // long before today.
        let txns = run(
            "Old gym",
            &[(-4000, d(2026, 3, 3)), (-4000, d(2026, 4, 3)), (-4000, d(2026, 5, 3))],
        );
        assert!(detect(&txns, TODAY).is_empty());
    }

    #[test]
    fn a_late_charge_is_still_expected() {
        // Due 3 November, not here by the 5th: late, not stopped.
        let txns = run(
            "Phone",
            &[(-5000, d(2026, 8, 3)), (-5000, d(2026, 9, 3)), (-5000, d(2026, 10, 3))],
        );
        assert_eq!(detect(&txns, d(2026, 11, 20)).len(), 1);
    }

    #[test]
    fn amounts_more_than_a_fifth_apart_break_a_series() {
        let txns = run(
            "Corner shop",
            &[
                (-1000, d(2026, 7, 3)),
                (-2000, d(2026, 8, 3)),
                (-1000, d(2026, 9, 3)),
                (-2000, d(2026, 10, 3)),
            ],
        );
        assert!(detect(&txns, TODAY).is_empty());
    }

    #[test]
    fn one_odd_charge_is_left_out_rather_than_hiding_the_series() {
        let txns = run(
            "Cloud storage",
            &[
                (-299, d(2026, 6, 3)),
                (-299, d(2026, 7, 3)),
                (-9900, d(2026, 7, 20)),
                (-299, d(2026, 8, 3)),
                (-299, d(2026, 9, 3)),
                (-299, d(2026, 10, 3)),
            ],
        );
        let found = detect(&txns, TODAY);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].count, found[0].amount, found[0].cadence),
            (5, -299, Cadence::Monthly)
        );
    }

    #[test]
    fn irregular_intervals_are_not_a_cadence() {
        let txns = run(
            "Barber",
            &[
                (-3500, d(2026, 6, 1)),
                (-3500, d(2026, 6, 29)),
                (-3500, d(2026, 8, 10)),
                (-3500, d(2026, 9, 9)),
                (-3500, d(2026, 10, 24)),
            ],
        );
        // 28, 42, 30, 45 days: a median of 36 is no cadence at all.
        assert!(detect(&txns, TODAY).is_empty());
    }

    #[test]
    fn two_monthly_charges_are_not_yet_a_series() {
        let txns = run("New app", &[(-499, d(2026, 9, 3)), (-499, d(2026, 10, 3))]);
        assert!(detect(&txns, TODAY).is_empty());
    }

    #[test]
    fn excluded_ones_other_accounts_and_refunds_are_kept_apart() {
        let mut txns = run(
            "Spotify",
            &[(-1099, d(2026, 8, 3)), (-1099, d(2026, 9, 3)), (-1099, d(2026, 10, 3))],
        );
        for t in &mut txns {
            t.excluded = true;
        }
        assert!(detect(&txns, TODAY).is_empty(), "excluded transactions are ignored");

        // The same payee, one charge in each of three accounts: no series.
        let spread: Vec<Txn> = [d(2026, 8, 3), d(2026, 9, 3), d(2026, 10, 3)]
            .into_iter()
            .map(|day| charge(BankAccountId::new(), "Spotify", day, -1099))
            .collect();
        assert!(detect(&spread, TODAY).is_empty());

        // Two charges and a refund do not make three.
        let account = BankAccountId::new();
        let mixed = vec![
            charge(account, "Spotify", d(2026, 8, 3), -1099),
            charge(account, "Spotify", d(2026, 9, 3), 1099),
            charge(account, "Spotify", d(2026, 10, 3), -1099),
        ];
        assert!(detect(&mixed, TODAY).is_empty());
    }

    #[test]
    fn the_shown_payee_groups_whatever_the_bank_called_it() {
        let account = BankAccountId::new();
        let mut txns = vec![
            charge(account, "NETFLIX.COM 1234", d(2026, 8, 3), -1599),
            charge(account, "NETFLIX.COM 5678", d(2026, 9, 3), -1599),
            charge(account, "NETFLIX.COM 9012", d(2026, 10, 3), -1599),
        ];
        assert!(detect(&txns, TODAY).is_empty(), "three different bank payees");
        for t in &mut txns {
            t.payee_override = "Netflix".into();
        }
        let found = detect(&txns, TODAY);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].payee, "Netflix");
    }

    #[test]
    fn the_median_is_exact_in_integers() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5]), Some(5));
        assert_eq!(median(vec![1, 4]), Some(2));
        assert_eq!(median(vec![-1, -4]), Some(-3));
        assert_eq!(median(vec![i64::MAX, i64::MAX]), Some(i64::MAX));
        assert!(within_a_fifth(-1200, -1000));
        assert!(!within_a_fifth(-1201, -1000));
        assert!(!within_a_fifth(i64::MIN, i64::MAX));
    }
}
