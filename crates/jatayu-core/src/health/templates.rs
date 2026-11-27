//! Condition templates: diabetes (type 1 and type 2) and hypothyroidism.
//!
//! Data, not code paths. A template is a [`TemplateInfo`] -- what to watch,
//! default targets with where each came from, tests and when they come due,
//! medication timing rules by tag, symptom trackers to offer. Applying one
//! copies its plan onto a [`super::Condition`]; from then on the plan is the
//! person's. Every default is a suggestion to confirm with a clinician.
//!
//! A test is named by its LOINC code when we are sure of one; an exam with no
//! code, or a panel whose code depends on the laboratory, is named by a word
//! ("eye-exam", "lipid-panel"). A wrong code would file results under
//! someone else's test, which is worse than a word.

use super::metric::Metric;
use super::{
    CarePlan, CareTarget, Code, LabDue, LabResult, LabSchedule, Measure, MedRule, Medication, Stat,
    TargetStanding, TemplateInfo, TemplateKey,
};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use std::collections::BTreeMap;

/// The version every template here is at. A plan records the version it was
/// copied from, so a newer template can be offered later and never forced.
const VERSION: u32 = 1;

/// Every template, for the "Add a condition" screen.
pub fn all() -> Vec<TemplateInfo> {
    [TemplateKey::Type1Diabetes, TemplateKey::Type2Diabetes, TemplateKey::Hypothyroidism]
        .into_iter()
        .map(get)
        .collect()
}

/// One template.
pub fn get(key: TemplateKey) -> TemplateInfo {
    match key {
        TemplateKey::Type1Diabetes | TemplateKey::Type2Diabetes => diabetes(key),
        TemplateKey::Hypothyroidism => hypothyroidism(),
    }
}

fn metric(metric: Metric, stat: Stat) -> Measure {
    Measure::Metric { metric, stat }
}

fn lab(code: &str) -> Measure {
    Measure::Lab { code: code.to_string() }
}

fn care_target(
    label: &str,
    measure: Measure,
    low: Option<f64>,
    high: Option<f64>,
    window_days: u32,
    note: &str,
) -> CareTarget {
    CareTarget {
        label: label.to_string(),
        measure,
        low,
        high,
        use_reference_range: false,
        window_days,
        note: note.to_string(),
    }
}

/// A target for a lab result judged against the range each report prints.
fn reference_target(label: &str, code: &str, note: &str) -> CareTarget {
    CareTarget { use_reference_range: true, ..care_target(label, lab(code), None, None, 0, note) }
}

fn lab_test(code: &str, name: &str, every_days: u32) -> LabSchedule {
    LabSchedule {
        code: code.to_string(),
        name: name.to_string(),
        every_days,
        after_change_days: None,
        after_change_tag: String::new(),
    }
}

fn care_plan(
    watch: Vec<Measure>,
    targets: Vec<CareTarget>,
    labs: Vec<LabSchedule>,
    med_rules: BTreeMap<String, Vec<MedRule>>,
) -> CarePlan {
    CarePlan { watch, targets, labs, med_rules, tracker_ids: Vec::new(), template_version: VERSION }
}

fn icd10(code: &str, display: &str) -> Code {
    Code { system: "icd10".to_string(), code: code.to_string(), display: display.to_string() }
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| word.to_string()).collect()
}

/// Type 1 and type 2 differ in what is worth watching (insulin, for the
/// first) and in the one medication rule a template can safely suggest
/// (metformin with food, for the second); the targets and tests are the same.
fn diabetes(key: TemplateKey) -> TemplateInfo {
    let type1 = key == TemplateKey::Type1Diabetes;

    let mut watch = vec![
        metric(Metric::BloodGlucose, Stat::Mean),
        metric(Metric::BloodGlucose, Stat::InRange { low: 70, high: 180 }),
    ];
    if type1 {
        watch.push(metric(Metric::InsulinBasal, Stat::Sum));
        watch.push(metric(Metric::InsulinBolus, Stat::Sum));
    }
    // Carbohydrate is not watched here: it comes from the meals logged, which
    // the day's view already sets beside glucose.
    watch.extend([
        metric(Metric::Weight, Stat::Last),
        metric(Metric::BloodPressureSystolic, Stat::Mean),
        metric(Metric::BloodPressureDiastolic, Stat::Mean),
        metric(Metric::Steps, Stat::Sum),
        lab("4548-4"),
    ]);

    let consensus = "International consensus on time in range, 2019.";
    let ada = "ADA Standards of Care.";
    let targets = vec![
        care_target(
            "Time in range",
            metric(Metric::BloodGlucose, Stat::InRange { low: 70, high: 180 }),
            Some(0.70),
            None,
            14,
            &format!("At least 70% of the time in 70-180 mg/dL, over 14 days. {consensus}"),
        ),
        care_target(
            "Time below 70 mg/dL",
            metric(Metric::BloodGlucose, Stat::InRange { low: 0, high: 69 }),
            None,
            Some(0.04),
            14,
            &format!("Under 4% of the time, over 14 days. {consensus}"),
        ),
        care_target(
            "Time below 54 mg/dL",
            metric(Metric::BloodGlucose, Stat::InRange { low: 0, high: 53 }),
            None,
            Some(0.01),
            14,
            &format!("Under 1% of the time, over 14 days. {consensus}"),
        ),
        care_target(
            "A1c",
            lab("4548-4"),
            None,
            Some(7.0),
            0,
            &format!("Below 7% is the usual aim for an adult; yours may be set differently. {ada}"),
        ),
        care_target(
            "Blood pressure, top number",
            metric(Metric::BloodPressureSystolic, Stat::Mean),
            None,
            Some(130.0),
            14,
            &format!("Below 130 mmHg, averaged over 14 days. {ada}"),
        ),
        care_target(
            "Blood pressure, bottom number",
            metric(Metric::BloodPressureDiastolic, Stat::Mean),
            None,
            Some(80.0),
            14,
            &format!("Below 80 mmHg, averaged over 14 days. {ada}"),
        ),
    ];

    let labs = vec![
        lab_test("4548-4", "Hemoglobin A1c", 90),
        lab_test("9318-7", "Urine albumin/creatinine ratio", 365),
        lab_test("lipid-panel", "Lipid panel", 365),
        lab_test("egfr", "eGFR", 365),
        lab_test("eye-exam", "Dilated eye exam", 365),
        lab_test("foot-exam", "Foot exam", 365),
    ];

    let mut med_rules = BTreeMap::new();
    if !type1 {
        med_rules.insert("metformin".to_string(), vec![MedRule::WithFood { minutes: 30 }]);
    }

    let (name, summary, code) = if type1 {
        (
            "Type 1 diabetes",
            "Watches glucose, time in range, insulin, weight, blood pressure and activity, \
             with the usual targets and tests for you to confirm with your care team.",
            icd10("E10.9", "Type 1 diabetes mellitus without complications"),
        )
    } else {
        (
            "Type 2 diabetes",
            "Watches glucose, time in range, weight, blood pressure and activity, with the \
             usual targets and tests, and metformin taken with food, for you to confirm with \
             your care team.",
            icd10("E11.9", "Type 2 diabetes mellitus without complications"),
        )
    };
    TemplateInfo {
        key,
        name: name.to_string(),
        summary: summary.to_string(),
        version: VERSION,
        plan: care_plan(watch, targets, labs, med_rules),
        symptoms: words(&["Feeling low", "Thirst", "Fatigue"]),
        codes: vec![code],
    }
}

fn hypothyroidism() -> TemplateInfo {
    let watch = vec![
        lab("3016-3"),
        lab("3024-7"),
        metric(Metric::RestingHeartRate, Stat::Mean),
        metric(Metric::Weight, Stat::Last),
    ];

    let note = "Inside the range printed on each laboratory report. \
                Your clinician may aim for a narrower one.";
    let targets =
        vec![reference_target("TSH", "3016-3", note), reference_target("Free T4", "3024-7", note)];

    // Thyroid hormone takes about six weeks to settle after a change, so that
    // is when a recheck says something; otherwise a yearly one will do.
    let recheck = |code: &str, name: &str| LabSchedule {
        after_change_days: Some(42),
        after_change_tag: "thyroid".to_string(),
        ..lab_test(code, name, 365)
    };
    let labs = vec![recheck("3016-3", "TSH"), recheck("3024-7", "Free T4")];

    let mut med_rules = BTreeMap::new();
    med_rules.insert(
        "thyroid".to_string(),
        vec![
            MedRule::EmptyStomach { minutes: 30 },
            MedRule::ApartFrom { tags: words(&["calcium", "iron"]), hours: 4.0 },
        ],
    );

    TemplateInfo {
        key: TemplateKey::Hypothyroidism,
        name: "Hypothyroidism".to_string(),
        summary: "Tracks thyroid blood tests against each report's own range, with a recheck \
                  six weeks after any dose change and reminders of how thyroid medicine is \
                  best taken."
            .to_string(),
        version: VERSION,
        plan: care_plan(watch, targets, labs, med_rules),
        symptoms: words(&["Fatigue", "Feeling cold", "Low mood", "Constipation"]),
        codes: vec![icd10("E03.9", "Hypothyroidism, unspecified")],
    }
}

/// When each of the plan's tests comes due on `today`: after the last
/// result by `every_days`, or after the latest dose change of a medication
/// tagged `after_change_tag` by `after_change_days` when that is later
/// than the last result.
pub fn labs_due(
    plan: &CarePlan,
    labs: &[LabResult],
    medications: &[Medication],
    today: Date,
) -> Vec<LabDue> {
    plan.labs.iter().map(|schedule| lab_due(schedule, labs, medications, today)).collect()
}

fn lab_due(
    schedule: &LabSchedule,
    labs: &[LabResult],
    medications: &[Medication],
    today: Date,
) -> LabDue {
    let last = labs
        .iter()
        .filter(|result| is_result_of(result, &schedule.code, &schedule.name))
        .map(|result| result.collected)
        .max();

    // By time alone: never measured is due now; a test with no interval is
    // never due by the calendar.
    let mut due: Option<(Date, String)> = match (schedule.every_days, last) {
        (0, _) => None,
        (_, None) => Some((today, "No result yet".to_string())),
        (days, Some(last)) => plus_days(last, days)
            .map(|date| (date, format!("{} after the last result", span_phrase(days)))),
    };

    // By a dose change since the last result: counted from the latest one,
    // as the level settles from the last change, not the first. Starting the
    // medication is a change too. A change since the last result decides the
    // date outright, sooner or later than the calendar's: a TSH a year
    // overdue taken the day after a dose change would measure a level that
    // has not settled, so the recheck six weeks on is the one worth having.
    let tag = schedule.after_change_tag.trim();
    if let Some(days) = schedule.after_change_days
        && !tag.is_empty()
        && let Some(changed) = medications
            .iter()
            .filter(|medication| medication.tags.iter().any(|t| t.trim().eq_ignore_ascii_case(tag)))
            .flat_map(|medication| {
                medication.history.iter().map(|change| change.on).chain(medication.started)
            })
            .filter(|on| last.is_none_or(|last_result| *on > last_result))
            .max()
        && let Some(date) = plus_days(changed, days)
    {
        let when = day_month(changed, today);
        due = Some((date, format!("{} after the dose change on {when}", span_phrase(days))));
    }

    let overdue = due.as_ref().is_some_and(|(date, _)| *date < today);
    let (due, reason) = match due {
        Some((date, reason)) => (Some(date), reason),
        None => (None, String::new()),
    };
    LabDue { schedule: schedule.clone(), last, due, reason, overdue }
}

/// Where a lab target stands, from the latest result of its code.
pub fn lab_standing(target: &CareTarget, labs: &[LabResult]) -> TargetStanding {
    let unknown = |value: Option<f64>, at: Option<Timestamp>| TargetStanding {
        target: target.clone(),
        value,
        at,
        state: "unknown".to_string(),
    };
    let Measure::Lab { code } = &target.measure else { return unknown(None, None) };
    let latest = labs
        .iter()
        .filter(|result| result.value.is_some() && is_result_of(result, code, ""))
        .max_by_key(|result| (result.collected, result.collected_at, result.created_at));
    let Some(latest) = latest else { return unknown(None, None) };
    let Some(value) = latest.value else { return unknown(None, None) };
    // A result of a date alone has no instant; midday UTC is the same date
    // anywhere a person is likely to be.
    let at = latest.collected_at.or_else(|| {
        latest.collected.at(12, 0, 0, 0).to_zoned(TimeZone::UTC).ok().map(|zoned| zoned.timestamp())
    });

    // The report's own range when the target asks for it and the report
    // printed one; otherwise the target's.
    let printed = latest.ref_low.is_some() || latest.ref_high.is_some();
    let (low, high) = if target.use_reference_range && printed {
        (latest.ref_low, latest.ref_high)
    } else {
        (target.low, target.high)
    };
    if low.is_none() && high.is_none() {
        return unknown(Some(value), at);
    }
    let met = low.is_none_or(|low| value >= low) && high.is_none_or(|high| value <= high);
    TargetStanding {
        target: target.clone(),
        value: Some(value),
        at,
        state: if met { "met" } else { "notMet" }.to_string(),
    }
}

/// Whether `result` is a result of the test named by `code` (and `name`, if
/// given): by code; by the name printed on it, the schedule's or the one
/// [`LOINC`] knows the code by; or, for a test with a word for a code, by
/// that word.
fn is_result_of(result: &LabResult, code: &str, name: &str) -> bool {
    let same = |a: &str, b: &str| !b.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim());
    same(&result.code, code)
        || same(&result.name, name)
        || same(&result.name, code)
        || loinc_name(code).is_some_and(|known| same(&result.name, known))
}

fn loinc_name(code: &str) -> Option<&'static str> {
    LOINC.iter().find(|(known, _)| known.eq_ignore_ascii_case(code.trim())).map(|(_, name)| *name)
}

fn plus_days(date: Date, days: u32) -> Option<Date> {
    date.checked_add(Span::new().days(days)).ok()
}

/// A number of days as a person would say it: "A year", "3 months", "6 weeks".
fn span_phrase(days: u32) -> String {
    if days == 0 {
        return "0 days".to_string();
    }
    let (count, unit) = if days.is_multiple_of(365) {
        (days / 365, "year")
    } else if days.is_multiple_of(30) {
        (days / 30, "month")
    } else if days.is_multiple_of(7) {
        (days / 7, "week")
    } else {
        (days, "day")
    };
    if count == 1 { format!("A {unit}") } else { format!("{count} {unit}s") }
}

/// "3 Oct", with the year when it is not `today`'s.
fn day_month(date: Date, today: Date) -> String {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = MONTHS[usize::from(date.month().clamp(1, 12) as u8 - 1)];
    if date.year() == today.year() {
        format!("{} {month}", date.day())
    } else {
        format!("{} {month} {}", date.day(), date.year())
    }
}

/// LOINC codes the templates use, with the name each is shown under.
pub const LOINC: &[(&str, &str)] = &[
    ("4548-4", "Hemoglobin A1c"),
    ("9318-7", "Urine albumin/creatinine ratio"),
    ("3016-3", "TSH"),
    ("3024-7", "Free T4"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::schedule::fixtures::medication;
    use crate::health::{DoseChange, LabFlag, LabOrigin, MedSchedule};
    use crate::id::LabResultId;
    use jiff::civil::date;

    fn result(code: &str, name: &str, value: Option<f64>, collected: Date) -> LabResult {
        LabResult {
            id: LabResultId::new(),
            code: code.to_string(),
            name: name.to_string(),
            value,
            text: String::new(),
            unit: String::new(),
            ref_low: None,
            ref_high: None,
            ref_text: String::new(),
            flag: LabFlag::None,
            collected,
            collected_at: None,
            lab: String::new(),
            panel: String::new(),
            document_id: None,
            origin: LabOrigin::Manual,
            note: String::new(),
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
        }
    }

    /// A thyroid medication whose dose changed on each of `changes`.
    fn levothyroxine(changes: &[Date]) -> Medication {
        let mut med = medication("Levothyroxine", MedSchedule::default());
        med.tags = vec!["thyroid".to_string()];
        med.history = changes
            .iter()
            .map(|on| DoseChange {
                on: *on,
                dose: 1.0,
                strength: "50 mcg".to_string(),
                note: String::new(),
            })
            .collect();
        med
    }

    fn due_of<'a>(due: &'a [LabDue], code: &str) -> &'a LabDue {
        due.iter().find(|d| d.schedule.code == code).expect("the plan schedules it")
    }

    #[test]
    fn there_are_three_templates_at_version_one_with_their_codes() {
        let all = all();
        let keys: Vec<TemplateKey> = all.iter().map(|t| t.key).collect();
        assert_eq!(
            keys,
            [TemplateKey::Type1Diabetes, TemplateKey::Type2Diabetes, TemplateKey::Hypothyroidism]
        );
        for template in &all {
            assert_eq!(template.version, 1);
            assert_eq!(template.plan.template_version, 1);
            assert!(!template.name.is_empty());
            assert!(!template.summary.is_empty());
            assert_eq!(template.codes.len(), 1);
            assert_eq!(template.codes[0].system, "icd10");
            assert!(template.plan.tracker_ids.is_empty());
        }
        let codes: Vec<&str> = all.iter().map(|t| t.codes[0].code.as_str()).collect();
        assert_eq!(codes, ["E10.9", "E11.9", "E03.9"]);
        assert_eq!(get(TemplateKey::Type2Diabetes).name, "Type 2 diabetes");
    }

    #[test]
    fn every_template_survives_a_round_trip_through_json() {
        for template in all() {
            let json = serde_json::to_string(&template).unwrap();
            let back: TemplateInfo = serde_json::from_str(&json).unwrap();
            assert_eq!(back, template);
        }
    }

    #[test]
    fn the_diabetes_templates_watch_and_aim_for_the_consensus_values() {
        let t1 = get(TemplateKey::Type1Diabetes);
        let t2 = get(TemplateKey::Type2Diabetes);

        let tir = metric(Metric::BloodGlucose, Stat::InRange { low: 70, high: 180 });
        assert!(t1.plan.watch.contains(&tir) && t2.plan.watch.contains(&tir));
        assert!(t1.plan.watch.contains(&metric(Metric::InsulinBasal, Stat::Sum)));
        assert!(t1.plan.watch.contains(&metric(Metric::InsulinBolus, Stat::Sum)));
        assert!(!t2.plan.watch.contains(&metric(Metric::InsulinBasal, Stat::Sum)));
        assert!(t2.plan.watch.contains(&lab("4548-4")));

        let by_label = |t: &TemplateInfo, label: &str| {
            t.plan.targets.iter().find(|target| target.label == label).cloned().expect("a target")
        };
        let range = by_label(&t1, "Time in range");
        assert_eq!((range.low, range.high, range.window_days), (Some(0.70), None, 14));
        let low = by_label(&t1, "Time below 70 mg/dL");
        assert_eq!(low.measure, metric(Metric::BloodGlucose, Stat::InRange { low: 0, high: 69 }));
        assert_eq!((low.low, low.high), (None, Some(0.04)));
        let lower = by_label(&t1, "Time below 54 mg/dL");
        assert_eq!(lower.measure, metric(Metric::BloodGlucose, Stat::InRange { low: 0, high: 53 }));
        assert_eq!((lower.low, lower.high), (None, Some(0.01)));
        assert_eq!(by_label(&t1, "A1c").high, Some(7.0));
        assert_eq!(by_label(&t2, "Blood pressure, top number").high, Some(130.0));
        assert_eq!(by_label(&t2, "Blood pressure, bottom number").high, Some(80.0));
        assert!(range.note.contains("International consensus on time in range, 2019"));
        assert!(by_label(&t1, "A1c").note.contains("ADA Standards of Care"));
    }

    #[test]
    fn the_diabetes_tests_are_a1c_every_three_months_and_the_rest_yearly() {
        let plan = get(TemplateKey::Type1Diabetes).plan;
        let every: Vec<(&str, u32)> =
            plan.labs.iter().map(|l| (l.code.as_str(), l.every_days)).collect();
        assert_eq!(
            every,
            [
                ("4548-4", 90),
                ("9318-7", 365),
                ("lipid-panel", 365),
                ("egfr", 365),
                ("eye-exam", 365),
                ("foot-exam", 365),
            ]
        );
        assert!(
            plan.labs
                .iter()
                .all(|l| l.after_change_days.is_none() && l.after_change_tag.is_empty())
        );
    }

    #[test]
    fn only_type_two_gets_the_metformin_rule_and_both_get_the_symptoms() {
        let t1 = get(TemplateKey::Type1Diabetes);
        let t2 = get(TemplateKey::Type2Diabetes);
        assert!(t1.plan.med_rules.is_empty());
        assert_eq!(
            t2.plan.med_rules.get("metformin"),
            Some(&vec![MedRule::WithFood { minutes: 30 }])
        );
        assert_eq!(t1.symptoms, ["Feeling low", "Thirst", "Fatigue"]);
        assert_eq!(t2.symptoms, t1.symptoms);
    }

    #[test]
    fn hypothyroidism_judges_by_each_reports_range_and_rechecks_after_a_dose_change() {
        let t = get(TemplateKey::Hypothyroidism);
        assert_eq!(
            t.plan.watch,
            [
                lab("3016-3"),
                lab("3024-7"),
                metric(Metric::RestingHeartRate, Stat::Mean),
                metric(Metric::Weight, Stat::Last),
            ]
        );
        assert_eq!(t.plan.targets.len(), 2);
        for target in &t.plan.targets {
            assert!(target.use_reference_range);
            assert_eq!((target.low, target.high), (None, None));
            assert!(!target.note.is_empty());
        }
        for schedule in &t.plan.labs {
            assert_eq!(schedule.every_days, 365);
            assert_eq!(schedule.after_change_days, Some(42));
            assert_eq!(schedule.after_change_tag, "thyroid");
        }
        assert_eq!(
            t.plan.med_rules.get("thyroid"),
            Some(&vec![
                MedRule::EmptyStomach { minutes: 30 },
                MedRule::ApartFrom { tags: vec!["calcium".into(), "iron".into()], hours: 4.0 },
            ])
        );
        assert_eq!(t.symptoms, ["Fatigue", "Feeling cold", "Low mood", "Constipation"]);
    }

    #[test]
    fn every_target_has_its_test_and_every_code_that_looks_like_loinc_is_in_the_table() {
        for template in all() {
            let plan = &template.plan;
            let mut codes: Vec<&str> = Vec::new();
            for measure in plan.targets.iter().map(|t| &t.measure).chain(&plan.watch) {
                if let Measure::Lab { code } = measure {
                    codes.push(code);
                    assert!(
                        plan.labs.iter().any(|schedule| schedule.code == *code),
                        "{} watches {code} but never schedules it",
                        template.name
                    );
                }
            }
            codes.extend(plan.labs.iter().map(|schedule| schedule.code.as_str()));
            for code in codes {
                let numeric = code.chars().next().is_some_and(|c| c.is_ascii_digit());
                assert_eq!(numeric, loinc_name(code).is_some(), "{code}");
            }
            // A coded test is shown under the name the table knows it by, so a
            // result printed under either matches.
            for schedule in &plan.labs {
                if let Some(known) = loinc_name(&schedule.code) {
                    assert_eq!(schedule.name, known);
                }
            }
        }
    }

    #[test]
    fn a_test_never_done_is_due_today_and_not_yet_overdue() {
        let plan = get(TemplateKey::Type2Diabetes).plan;
        let today = date(2026, 11, 15);
        let due = labs_due(&plan, &[], &[], today);
        assert_eq!(due.len(), 6);
        for d in &due {
            assert_eq!(d.last, None);
            assert_eq!(d.due, Some(today));
            assert!(!d.overdue);
            assert_eq!(d.reason, "No result yet");
        }
    }

    #[test]
    fn a_test_comes_due_an_interval_after_the_last_result() {
        let plan = get(TemplateKey::Type2Diabetes).plan;
        let today = date(2026, 11, 15);
        let labs = [
            result("4548-4", "Hemoglobin A1c", Some(6.8), date(2026, 8, 1)),
            // An earlier one, which must not be the one counted from.
            result("4548-4", "Hemoglobin A1c", Some(7.4), date(2026, 2, 1)),
            // No code on it, but the name is the schedule's.
            result("", "Dilated eye exam", None, date(2026, 6, 1)),
            result("lipid-panel", "Lipid panel", None, date(2025, 11, 14)),
        ];
        let due = labs_due(&plan, &labs, &[], today);

        let a1c = due_of(&due, "4548-4");
        assert_eq!(a1c.last, Some(date(2026, 8, 1)));
        assert_eq!(a1c.due, Some(date(2026, 10, 30)));
        assert_eq!(a1c.reason, "3 months after the last result");
        assert!(a1c.overdue);

        let eye = due_of(&due, "eye-exam");
        assert_eq!(eye.last, Some(date(2026, 6, 1)));
        assert_eq!(eye.due, Some(date(2027, 6, 1)));
        assert_eq!(eye.reason, "A year after the last result");
        assert!(!eye.overdue);

        // A year and a day ago is a day late.
        let lipids = due_of(&due, "lipid-panel");
        assert_eq!(lipids.due, Some(date(2026, 11, 14)));
        assert!(lipids.overdue);
        // And due on the day itself is not late yet.
        let on_the_day =
            labs_due(&plan, &[result("lipid-panel", "", None, date(2025, 11, 15))], &[], today);
        assert_eq!(due_of(&on_the_day, "lipid-panel").due, Some(today));
        assert!(!due_of(&on_the_day, "lipid-panel").overdue);

        // Matched by the name a LOINC code is known by, with a different code.
        let by_name = labs_due(
            &plan,
            &[result("17856-6", "hemoglobin a1c", Some(7.0), date(2026, 11, 1))],
            &[],
            today,
        );
        assert_eq!(due_of(&by_name, "4548-4").last, Some(date(2026, 11, 1)));
    }

    #[test]
    fn a_dose_change_since_the_last_result_brings_the_recheck_forward() {
        let plan = get(TemplateKey::Hypothyroidism).plan;
        let today = date(2026, 11, 15);
        let labs = [
            result("3016-3", "Thyroid stimulating hormone", Some(2.1), date(2026, 1, 10)),
            result("3024-7", "Free T4", Some(1.2), date(2026, 1, 10)),
        ];
        let meds = [levothyroxine(&[date(2026, 10, 3)])];

        let due = labs_due(&plan, &labs, &meds, today);
        let tsh = due_of(&due, "3016-3");
        assert_eq!(tsh.last, Some(date(2026, 1, 10)));
        assert_eq!(tsh.due, Some(date(2026, 11, 14)));
        assert_eq!(tsh.reason, "6 weeks after the dose change on 3 Oct");
        assert!(tsh.overdue);
        assert_eq!(due_of(&due, "3024-7").due, Some(date(2026, 11, 14)));

        // Not yet six weeks: due, but not late, and still the change's date.
        let recent = [levothyroxine(&[date(2026, 11, 1)])];
        let due = labs_due(&plan, &labs, &recent, today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2026, 12, 13)));
        assert!(!due_of(&due, "3016-3").overdue);
    }

    #[test]
    fn a_dose_change_is_counted_from_the_latest_and_only_if_after_the_last_result() {
        let plan = get(TemplateKey::Hypothyroidism).plan;
        let today = date(2026, 11, 15);
        let labs = [result("3016-3", "TSH", Some(2.1), date(2026, 5, 1))];

        // Two changes since: six weeks from the second.
        let meds = [levothyroxine(&[date(2026, 6, 1), date(2026, 9, 1)])];
        let due = labs_due(&plan, &labs, &meds, today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2026, 10, 13)));

        // A change before the result, or on the very day, changes nothing.
        let old = [levothyroxine(&[date(2026, 4, 1), date(2026, 5, 1)])];
        let due = labs_due(&plan, &labs, &old, today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2027, 5, 1)));
        assert_eq!(due_of(&due, "3016-3").reason, "A year after the last result");

        // A medication not tagged for it is none of the schedule's business.
        let mut other = levothyroxine(&[date(2026, 10, 3)]);
        other.tags = vec!["statin".to_string()];
        let due = labs_due(&plan, &labs, &[other], today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2027, 5, 1)));

        // The tag matches whatever its case.
        let mut shouted = levothyroxine(&[date(2026, 10, 3)]);
        shouted.tags = vec!["Thyroid".to_string()];
        let due = labs_due(&plan, &labs, &[shouted], today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2026, 11, 14)));
    }

    #[test]
    fn a_change_a_year_back_names_its_year() {
        let plan = get(TemplateKey::Hypothyroidism).plan;
        let meds = [levothyroxine(&[date(2025, 10, 3)])];
        let due = labs_due(&plan, &[], &meds, date(2026, 11, 15));
        // Never measured: due since the change, not merely today.
        let tsh = due_of(&due, "3016-3");
        assert_eq!(tsh.due, Some(date(2025, 11, 14)));
        assert_eq!(tsh.reason, "6 weeks after the dose change on 3 Oct 2025");
        assert!(tsh.overdue);
    }

    #[test]
    fn a_test_with_no_interval_is_due_only_by_a_dose_change() {
        let mut plan = get(TemplateKey::Hypothyroidism).plan;
        plan.labs[0].every_days = 0;
        let today = date(2026, 11, 15);
        let labs = [result("3016-3", "TSH", Some(2.1), date(2026, 1, 10))];

        let due = labs_due(&plan, &labs, &[], today);
        let tsh = due_of(&due, "3016-3");
        assert_eq!((tsh.last, tsh.due), (Some(date(2026, 1, 10)), None));
        assert!(tsh.reason.is_empty());
        assert!(!tsh.overdue);

        let due = labs_due(&plan, &labs, &[levothyroxine(&[date(2026, 10, 3)])], today);
        assert_eq!(due_of(&due, "3016-3").due, Some(date(2026, 11, 14)));
    }

    #[test]
    fn spans_read_as_a_person_would_say_them() {
        assert_eq!(span_phrase(365), "A year");
        assert_eq!(span_phrase(730), "2 years");
        assert_eq!(span_phrase(90), "3 months");
        assert_eq!(span_phrase(30), "A month");
        assert_eq!(span_phrase(42), "6 weeks");
        assert_eq!(span_phrase(7), "A week");
        assert_eq!(span_phrase(10), "10 days");
        assert_eq!(span_phrase(1), "A day");
    }

    fn a1c_target() -> CareTarget {
        get(TemplateKey::Type1Diabetes).plan.targets.into_iter().find(|t| t.label == "A1c").unwrap()
    }

    fn tsh_target() -> CareTarget {
        get(TemplateKey::Hypothyroidism)
            .plan
            .targets
            .into_iter()
            .find(|t| t.label == "TSH")
            .unwrap()
    }

    #[test]
    fn a_target_with_bounds_is_met_inside_them_and_not_outside() {
        let target = a1c_target();
        let met = lab_standing(
            &target,
            &[result("4548-4", "Hemoglobin A1c", Some(6.8), date(2026, 8, 1))],
        );
        assert_eq!((met.value, met.state.as_str()), (Some(6.8), "met"));
        assert_eq!(met.target, target);

        // The bound itself is inside it.
        let edge = lab_standing(&target, &[result("4548-4", "", Some(7.0), date(2026, 8, 1))]);
        assert_eq!(edge.state, "met");

        let not_met = lab_standing(&target, &[result("4548-4", "", Some(7.5), date(2026, 8, 1))]);
        assert_eq!((not_met.value, not_met.state.as_str()), (Some(7.5), "notMet"));
    }

    #[test]
    fn the_latest_result_with_a_value_is_the_one_judged() {
        let target = a1c_target();
        let labs = [
            result("4548-4", "", Some(8.2), date(2026, 2, 1)),
            result("4548-4", "", Some(6.9), date(2026, 8, 1)),
            // Later, but nothing to judge: skipped.
            result("4548-4", "", None, date(2026, 10, 1)),
            // Another test altogether.
            result("3016-3", "TSH", Some(99.0), date(2026, 11, 1)),
        ];
        let standing = lab_standing(&target, &labs);
        assert_eq!(standing.value, Some(6.9));
        assert_eq!(standing.state, "met");
        // No instant on the result: midday UTC on its day.
        assert_eq!(standing.at, Some("2026-08-01T12:00:00Z".parse().unwrap()));

        let mut timed = result("4548-4", "", Some(7.1), date(2026, 8, 1));
        timed.collected_at = Some("2026-08-01T16:30:00Z".parse().unwrap());
        assert_eq!(
            lab_standing(&target, &[timed]).at,
            Some("2026-08-01T16:30:00Z".parse().unwrap())
        );
    }

    #[test]
    fn a_reference_range_target_uses_the_ranges_on_the_result() {
        let target = tsh_target();
        let mut high = result("3016-3", "TSH", Some(5.1), date(2026, 8, 1));
        high.ref_low = Some(0.4);
        high.ref_high = Some(4.0);
        assert_eq!(lab_standing(&target, &[high]).state, "notMet");

        let mut inside = result("3016-3", "TSH", Some(2.0), date(2026, 8, 1));
        inside.ref_low = Some(0.4);
        inside.ref_high = Some(4.0);
        assert_eq!(lab_standing(&target, &[inside]).state, "met");

        // An open-ended range: nothing below the floor is wrong.
        let mut floor = result("3016-3", "TSH", Some(0.2), date(2026, 8, 1));
        floor.ref_low = Some(0.4);
        assert_eq!(lab_standing(&target, &[floor]).state, "notMet");
    }

    #[test]
    fn it_is_unknown_when_there_is_nothing_to_judge_by() {
        // A reference-range target and a result that printed no range.
        let bare =
            lab_standing(&tsh_target(), &[result("3016-3", "TSH", Some(2.0), date(2026, 8, 1))]);
        assert_eq!((bare.value, bare.state.as_str()), (Some(2.0), "unknown"));

        // No result at all, or none with a number in it.
        let none = lab_standing(&a1c_target(), &[]);
        assert_eq!((none.value, none.at, none.state.as_str()), (None, None, "unknown"));
        let text = result("4548-4", "", None, date(2026, 8, 1));
        assert_eq!(lab_standing(&a1c_target(), &[text]).state, "unknown");

        // A target that is not a lab's is not judged here.
        let glucose = get(TemplateKey::Type1Diabetes).plan.targets.into_iter().next().unwrap();
        assert_eq!(
            lab_standing(&glucose, &[result("4548-4", "", Some(6.0), date(2026, 8, 1))]).state,
            "unknown"
        );
    }
}
