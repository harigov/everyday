//! Medication timing rules against what was actually taken and eaten.
//!
//! Only the rules a person confirmed on a medication ([`MedRule`]); nothing
//! here knows pharmacology. The levothyroxine rules a template suggests --
//! empty stomach, apart from calcium and iron -- are checked the same way
//! as any other.
//!
//! Only doses marked taken are held to a rule: a skipped dose broke nothing,
//! and neither did one the reminder sweep wrote as missed. An alert is a
//! notice after the fact, with the time the rule was broken -- the meal's for
//! an empty stomach, the dose's for a missing meal, the later dose's for two
//! medications too close together.

use super::{Dose, DoseStatus, Meal, MedRule, Medication, RuleAlert};
use crate::id::{DoseId, MedicationId};
use std::collections::{BTreeMap, BTreeSet};

/// Every rule broken by `doses` (taken ones only) and `meals`.
pub fn check(medications: &[Medication], doses: &[Dose], meals: &[Meal]) -> Vec<RuleAlert> {
    let by_id: BTreeMap<MedicationId, &Medication> =
        medications.iter().map(|m| (m.id, m)).collect();
    let mut taken: Vec<&Dose> = doses
        .iter()
        .filter(|dose| dose.status == DoseStatus::Taken && by_id.contains_key(&dose.medication_id))
        .collect();
    taken.sort_by_key(|dose| dose.at);
    let mut meals: Vec<&Meal> = meals.iter().collect();
    meals.sort_by_key(|meal| meal.at);
    let facts = Facts { by_id, taken, meals };

    let mut alerts = Vec::new();
    // Two medications can each carry a rule about the other; the pair of
    // doses is one breach, however many rules see it.
    let mut reported: BTreeSet<(DoseId, DoseId)> = BTreeSet::new();
    for dose in &facts.taken {
        let Some(medication) = facts.by_id.get(&dose.medication_id) else { continue };
        for rule in &medication.rules {
            match rule {
                MedRule::EmptyStomach { minutes } => {
                    alerts.extend(facts.empty_stomach(medication, dose, *minutes));
                }
                MedRule::WithFood { minutes } => {
                    alerts.extend(facts.with_food(medication, dose, *minutes));
                }
                MedRule::ApartFrom { tags, hours } => {
                    alerts.extend(facts.apart_from(medication, dose, tags, *hours, &mut reported));
                }
            }
        }
    }
    // Stable, so alerts at one moment stay in the order the doses gave them.
    alerts.sort_by_key(|alert| alert.at);
    alerts
}

/// A rule in a sentence, for the medication screen: "Empty stomach: nothing
/// to eat for 30 minutes after".
pub fn describe(rule: &MedRule) -> String {
    match rule {
        MedRule::EmptyStomach { minutes } => {
            format!("Empty stomach: nothing to eat for {} after", how_long(i64::from(*minutes)))
        }
        MedRule::WithFood { minutes } => {
            format!("With food: a meal within {} of the dose", how_long(i64::from(*minutes)))
        }
        MedRule::ApartFrom { tags, hours } => {
            let what =
                if tags.is_empty() { "other medications".to_string() } else { tags.join(", ") };
            let gap = how_long(hours_to_minutes(*hours));
            format!("Apart from {what}: at least {gap} either side")
        }
    }
}

/// What the rules are held against: the medications by id, the doses taken
/// and the meals eaten, both in time order so a window is a slice.
struct Facts<'a> {
    by_id: BTreeMap<MedicationId, &'a Medication>,
    taken: Vec<&'a Dose>,
    meals: Vec<&'a Meal>,
}

impl Facts<'_> {
    /// Each meal after `dose` and before `minutes` have passed breaks the
    /// rule. A meal at the very moment of the dose, or exactly `minutes`
    /// later, does not.
    fn empty_stomach(&self, medication: &Medication, dose: &Dose, minutes: u32) -> Vec<RuleAlert> {
        let start = dose.at.as_second();
        let end = start + i64::from(minutes) * 60;
        let first = self.meals.partition_point(|meal| meal.at.as_second() <= start);
        self.meals[first..]
            .iter()
            .take_while(|meal| meal.at.as_second() < end)
            .map(|meal| {
                // A meal always comes at least a second after, never "0 minutes".
                let after = ((meal.at.as_second() - start) / 60).max(1);
                RuleAlert {
                    medication_id: medication.id,
                    at: meal.at,
                    rule: MedRule::EmptyStomach { minutes },
                    message: format!(
                        "A meal {} after {}: the rule is nothing to eat for {} after",
                        how_long(after),
                        medication.name,
                        how_long(i64::from(minutes)),
                    ),
                    meal_id: Some(meal.id),
                    other_medication_id: None,
                }
            })
            .collect()
    }

    /// A dose with no meal within `minutes` either side (inclusive) breaks it.
    fn with_food(&self, medication: &Medication, dose: &Dose, minutes: u32) -> Option<RuleAlert> {
        let at = dose.at.as_second();
        let reach = i64::from(minutes) * 60;
        let first = self.meals.partition_point(|meal| meal.at.as_second() < at - reach);
        if self.meals.get(first).is_some_and(|meal| meal.at.as_second() <= at + reach) {
            return None;
        }
        Some(RuleAlert {
            medication_id: medication.id,
            at: dose.at,
            rule: MedRule::WithFood { minutes },
            message: format!(
                "No meal within {} of {}: the rule is to take it with food",
                how_long(i64::from(minutes)),
                medication.name,
            ),
            meal_id: None,
            other_medication_id: None,
        })
    }

    /// A taken dose of another medication carrying one of `tags` closer than
    /// `hours` to `dose`, before or after, breaks it. Exactly `hours` apart
    /// is far enough.
    fn apart_from(
        &self,
        medication: &Medication,
        dose: &Dose,
        tags: &[String],
        hours: f64,
        reported: &mut BTreeSet<(DoseId, DoseId)>,
    ) -> Vec<RuleAlert> {
        let wanted: Vec<String> = tags
            .iter()
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .collect();
        if wanted.is_empty() || !hours.is_finite() || hours <= 0.0 {
            return Vec::new();
        }
        let at = dose.at.as_second();
        let gap = hours_to_minutes(hours) * 60;
        let first = self.taken.partition_point(|other| other.at.as_second() <= at - gap);
        let mut alerts = Vec::new();
        let near = &self.taken[first..];
        for other in near.iter().take_while(|other| other.at.as_second() < at + gap) {
            if other.medication_id == medication.id {
                continue;
            }
            let Some(other_medication) = self.by_id.get(&other.medication_id) else { continue };
            let carries = |tag: &String| wanted.contains(&tag.trim().to_lowercase());
            if !other_medication.tags.iter().any(carries) {
                continue;
            }
            let pair = if dose.id <= other.id { (dose.id, other.id) } else { (other.id, dose.id) };
            if !reported.insert(pair) {
                continue;
            }
            alerts.push(RuleAlert {
                medication_id: medication.id,
                at: dose.at.max(other.at),
                rule: MedRule::ApartFrom { tags: tags.to_vec(), hours },
                message: format!(
                    "{} and {} were {} apart: the rule is at least {}",
                    medication.name,
                    other_medication.name,
                    how_long((other.at.as_second() - at).abs() / 60),
                    how_long(gap / 60),
                ),
                meal_id: None,
                other_medication_id: Some(other.medication_id),
            });
        }
        alerts
    }
}

fn hours_to_minutes(hours: f64) -> i64 {
    if hours.is_finite() { (hours * 60.0).round() as i64 } else { 0 }
}

/// A length of time in words: "10 minutes", "1 hour", "2 hours 30 minutes".
fn how_long(minutes: i64) -> String {
    let (hours, rest) = (minutes / 60, minutes % 60);
    let hours_part = match hours {
        1 => "1 hour".to_string(),
        n => format!("{n} hours"),
    };
    let minutes_part = match rest {
        1 => "1 minute".to_string(),
        n => format!("{n} minutes"),
    };
    match (hours, rest) {
        (0, _) => minutes_part,
        (_, 0) => hours_part,
        _ => format!("{hours_part} {minutes_part}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::MedSchedule;
    use crate::health::schedule::fixtures::{dose, meal, medication};
    use jiff::Timestamp;

    /// `h:m` UTC on 16 November 2026.
    fn t(h: u32, m: u32) -> Timestamp {
        format!("2026-11-16T{h:02}:{m:02}:00Z").parse().unwrap()
    }

    fn med(name: &str, tags: &[&str], rules: Vec<MedRule>) -> Medication {
        let mut med = medication(name, MedSchedule::AsNeeded);
        med.tags = tags.iter().map(|tag| tag.to_string()).collect();
        med.rules = rules;
        med
    }

    fn levothyroxine() -> Medication {
        med(
            "Levothyroxine",
            &["thyroid"],
            vec![
                MedRule::EmptyStomach { minutes: 30 },
                MedRule::ApartFrom { tags: vec!["calcium".into(), "iron".into()], hours: 4.0 },
            ],
        )
    }

    #[test]
    fn a_meal_inside_the_empty_stomach_window_is_an_alert() {
        let levo = med("Levothyroxine", &["thyroid"], vec![MedRule::EmptyStomach { minutes: 30 }]);
        let taken = dose(&levo, t(8, 0), DoseStatus::Taken);
        let before = meal(t(7, 50));
        let during = meal(t(8, 10));
        let edge = meal(t(8, 30));
        let after = meal(t(9, 0));

        let alerts =
            check(std::slice::from_ref(&levo), &[taken], &[before, during.clone(), edge, after]);
        assert_eq!(alerts.len(), 1);
        let alert = &alerts[0];
        assert_eq!(alert.medication_id, levo.id);
        assert_eq!(alert.at, t(8, 10));
        assert_eq!(alert.meal_id, Some(during.id));
        assert_eq!(alert.other_medication_id, None);
        assert_eq!(alert.rule, MedRule::EmptyStomach { minutes: 30 });
        assert_eq!(
            alert.message,
            "A meal 10 minutes after Levothyroxine: the rule is nothing to eat for 30 minutes after"
        );
    }

    #[test]
    fn every_meal_inside_the_window_is_its_own_alert() {
        let levo = med("Levothyroxine", &[], vec![MedRule::EmptyStomach { minutes: 60 }]);
        let taken = dose(&levo, t(8, 0), DoseStatus::Taken);
        // 40 seconds after reads as a minute, not zero; 90 minutes is outside.
        let quick = Meal { at: "2026-11-16T08:00:40Z".parse().unwrap(), ..meal(t(8, 0)) };
        let later = meal(t(8, 45));
        let outside = meal(t(9, 30));

        let alerts = check(&[levo], &[taken], &[outside, later, quick]);
        assert_eq!(alerts.len(), 2);
        assert!(alerts[0].message.starts_with("A meal 1 minute after Levothyroxine"));
        assert!(alerts[1].message.starts_with("A meal 45 minutes after Levothyroxine"));
        assert!(alerts[1].message.ends_with("nothing to eat for 1 hour after"));
    }

    #[test]
    fn a_dose_that_was_not_taken_breaks_no_rule() {
        let levo = levothyroxine();
        let calcium = med("Calcium", &["calcium"], Vec::new());
        let meals = [meal(t(8, 10))];
        let skipped = dose(&levo, t(8, 0), DoseStatus::Skipped);
        let missed = dose(&levo, t(8, 0), DoseStatus::Missed);
        let calcium_dose = dose(&calcium, t(9, 0), DoseStatus::Taken);

        assert!(
            check(&[levo.clone(), calcium.clone()], &[skipped, calcium_dose.clone()], &meals)
                .is_empty()
        );
        assert!(
            check(&[levo.clone(), calcium.clone()], &[missed, calcium_dose], &meals).is_empty()
        );

        // And a skipped calcium does not count against a taken levothyroxine.
        let taken = dose(&levo, t(8, 0), DoseStatus::Taken);
        let skipped_calcium = dose(&calcium, t(9, 0), DoseStatus::Skipped);
        let alerts = check(&[levo, calcium], &[taken, skipped_calcium], &[]);
        assert!(alerts.is_empty());
    }

    #[test]
    fn a_medication_with_no_rules_breaks_none() {
        let plain = med("Metformin", &[], Vec::new());
        let taken = dose(&plain, t(8, 0), DoseStatus::Taken);
        assert!(check(&[plain], &[taken], &[meal(t(8, 1))]).is_empty());
    }

    #[test]
    fn a_dose_with_no_meal_near_it_breaks_the_with_food_rule() {
        let metformin = med("Metformin", &[], vec![MedRule::WithFood { minutes: 30 }]);
        let with_lunch = dose(&metformin, t(12, 20), DoseStatus::Taken);
        let at_the_edge = dose(&metformin, t(20, 0), DoseStatus::Taken);
        let alone = dose(&metformin, t(22, 0), DoseStatus::Taken);
        let meals = [meal(t(12, 0)), meal(t(19, 30)), meal(t(18, 0))];

        let alerts = check(
            std::slice::from_ref(&metformin),
            &[with_lunch, at_the_edge, alone.clone()],
            &meals,
        );
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].at, alone.at);
        assert_eq!(alerts[0].meal_id, None);
        assert_eq!(alerts[0].rule, MedRule::WithFood { minutes: 30 });
        assert_eq!(
            alerts[0].message,
            "No meal within 30 minutes of Metformin: the rule is to take it with food"
        );

        // A meal after the dose counts as much as one before it.
        let after = dose(&metformin, t(11, 40), DoseStatus::Taken);
        assert!(check(std::slice::from_ref(&metformin), &[after], &meals).is_empty());
        // And with nothing logged at all, every dose is without one.
        let first = dose(&metformin, t(8, 0), DoseStatus::Taken);
        assert_eq!(check(&[metformin], &[first], &[]).len(), 1);
    }

    #[test]
    fn two_medications_too_close_are_an_alert_in_either_order() {
        let levo = levothyroxine();
        let calcium = med("Calcium", &["Calcium"], Vec::new());
        let levo_dose = dose(&levo, t(8, 0), DoseStatus::Taken);

        // Calcium two hours after: the alert is at the later of the two.
        let later = dose(&calcium, t(10, 0), DoseStatus::Taken);
        let alerts = check(&[levo.clone(), calcium.clone()], &[levo_dose.clone(), later], &[]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].medication_id, levo.id);
        assert_eq!(alerts[0].other_medication_id, Some(calcium.id));
        assert_eq!(alerts[0].at, t(10, 0));
        assert_eq!(
            alerts[0].message,
            "Levothyroxine and Calcium were 2 hours apart: the rule is at least 4 hours"
        );

        // Calcium two hours before.
        let earlier = dose(&calcium, t(6, 0), DoseStatus::Taken);
        let alerts = check(&[levo.clone(), calcium.clone()], &[levo_dose.clone(), earlier], &[]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].at, t(8, 0));
    }

    #[test]
    fn exactly_the_gap_apart_is_far_enough() {
        let levo = levothyroxine();
        let iron = med("Iron", &["iron"], Vec::new());
        let levo_dose = dose(&levo, t(8, 0), DoseStatus::Taken);

        let on_the_line = dose(&iron, t(12, 0), DoseStatus::Taken);
        assert!(
            check(&[levo.clone(), iron.clone()], &[levo_dose.clone(), on_the_line], &[]).is_empty()
        );
        let just_inside = dose(&iron, t(11, 59), DoseStatus::Taken);
        let alerts = check(&[levo, iron], &[levo_dose, just_inside], &[]);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].message.contains("3 hours 59 minutes apart"));
    }

    #[test]
    fn only_medications_carrying_a_listed_tag_count() {
        let levo = levothyroxine();
        let vitamin_d = med("Vitamin D", &["vitamin-d"], Vec::new());
        let untagged = med("Aspirin", &[], Vec::new());
        let levo_dose = dose(&levo, t(8, 0), DoseStatus::Taken);
        let doses = [
            levo_dose,
            dose(&vitamin_d, t(8, 30), DoseStatus::Taken),
            dose(&untagged, t(8, 45), DoseStatus::Taken),
        ];
        assert!(check(&[levo, vitamin_d, untagged], &doses, &[]).is_empty());
    }

    #[test]
    fn a_medication_is_never_apart_from_itself() {
        // Tagged with what its own rule names, and taken twice an hour apart.
        let both = med(
            "Calcium with iron",
            &["calcium", "iron"],
            vec![MedRule::ApartFrom { tags: vec!["calcium".into()], hours: 4.0 }],
        );
        let doses =
            [dose(&both, t(8, 0), DoseStatus::Taken), dose(&both, t(9, 0), DoseStatus::Taken)];
        assert!(check(&[both], &doses, &[]).is_empty());
    }

    #[test]
    fn a_pair_of_doses_is_one_alert_even_when_both_medications_have_the_rule() {
        let levo = levothyroxine();
        let calcium = med(
            "Calcium",
            &["calcium"],
            vec![MedRule::ApartFrom { tags: vec!["thyroid".into()], hours: 4.0 }],
        );
        let doses =
            [dose(&levo, t(8, 0), DoseStatus::Taken), dose(&calcium, t(9, 0), DoseStatus::Taken)];

        let alerts = check(&[levo, calcium], &doses, &[]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].at, t(9, 0));
    }

    #[test]
    fn alerts_come_in_time_order_across_rules() {
        let levo = levothyroxine();
        let calcium = med("Calcium", &["calcium"], Vec::new());
        let metformin = med("Metformin", &[], vec![MedRule::WithFood { minutes: 30 }]);
        let doses = [
            dose(&metformin, t(7, 0), DoseStatus::Taken),
            dose(&calcium, t(9, 0), DoseStatus::Taken),
            dose(&levo, t(8, 0), DoseStatus::Taken),
        ];
        let meals = [meal(t(8, 10))];

        let alerts = check(&[levo, calcium, metformin], &doses, &meals);
        let times: Vec<Timestamp> = alerts.iter().map(|alert| alert.at).collect();
        // No meal near the 07:00 metformin; the 08:10 meal after the 08:00
        // levothyroxine; the 09:00 calcium an hour from it.
        assert_eq!(times, [t(7, 0), t(8, 10), t(9, 0)]);
    }

    #[test]
    fn doses_of_a_medication_nobody_lists_are_ignored() {
        let levo = levothyroxine();
        let stranger = med("Unlisted", &["calcium"], Vec::new());
        let doses =
            [dose(&levo, t(8, 0), DoseStatus::Taken), dose(&stranger, t(9, 0), DoseStatus::Taken)];
        assert!(check(&[levo], &doses, &[]).is_empty());
    }

    #[test]
    fn rules_read_as_sentences() {
        assert_eq!(
            describe(&MedRule::EmptyStomach { minutes: 30 }),
            "Empty stomach: nothing to eat for 30 minutes after"
        );
        assert_eq!(
            describe(&MedRule::EmptyStomach { minutes: 60 }),
            "Empty stomach: nothing to eat for 1 hour after"
        );
        assert_eq!(
            describe(&MedRule::WithFood { minutes: 15 }),
            "With food: a meal within 15 minutes of the dose"
        );
        assert_eq!(
            describe(&MedRule::ApartFrom {
                tags: vec!["calcium".into(), "iron".into()],
                hours: 4.0
            }),
            "Apart from calcium, iron: at least 4 hours either side"
        );
        assert_eq!(
            describe(&MedRule::ApartFrom { tags: vec!["iron".into()], hours: 1.5 }),
            "Apart from iron: at least 1 hour 30 minutes either side"
        );
    }

    #[test]
    fn lengths_of_time_are_in_words() {
        assert_eq!(how_long(1), "1 minute");
        assert_eq!(how_long(30), "30 minutes");
        assert_eq!(how_long(60), "1 hour");
        assert_eq!(how_long(61), "1 hour 1 minute");
        assert_eq!(how_long(150), "2 hours 30 minutes");
        assert_eq!(how_long(240), "4 hours");
    }
}
