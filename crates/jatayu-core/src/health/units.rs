//! Units: every value into its metric's canonical unit on the way in, and
//! out into the unit a person chose on the way out.
//!
//! Sources spell units every way there is -- HealthKit's `count/min` and
//! `mg/dL`, Health Auto Export's `bpm` and `kcal`, a lab's `mmol/L`, a
//! person's `lb`. [`to_canonical`] knows them; anything it does not know is
//! refused rather than guessed at, because a glucose of 5.5 filed as mg/dL
//! when it was mmol/L is a reading eighteen times too low.
//!
//! What a metric may be measured in is decided by its canonical unit
//! ([`super::metric::MetricInfo::unit`]): a mass, a length, an energy. A
//! unit of another kind is refused for it, so a glucose in kilograms is
//! `None` and not passed through.

use super::{HealthUnits, Metric};

/// Milligrams per millimole of glucose divided by ten: mg/dL = mmol/L
/// times this. From the molar mass, 180.156 g/mol (HealthKit spells it
/// inside the unit, `mmol<180.1558800000541>/L`).
const MMOL_PER_L_TO_MG_PER_DL: f64 = 18.0156;

/// `value` in `unit`, converted to `metric`'s canonical unit
/// ([`super::metric::MetricInfo::unit`]). An empty `unit` is canonical
/// already. `None` for a unit this metric cannot be in, and for a value that
/// is not a number.
///
/// Percentages are stored 0..=100. HealthKit's export writes blood oxygen
/// and body fat as fractions (0.97) under the unit `%`, where Health Auto
/// Export sends 97, so for those a `%` value of 1 or less is a fraction and
/// is multiplied by 100. A real body fat of 1% or less does not exist.
pub fn to_canonical(metric: Metric, value: f64, unit: &str) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    if unit.trim().is_empty() {
        return Some(value);
    }
    let unit = key(unit)?;
    let canonical = metric.info().unit;
    match canonical {
        "degC" => match unit.as_str() {
            "degc" | "°c" | "c" | "celsius" => Some(value),
            "degf" | "°f" | "f" | "fahrenheit" => Some((value - 32.0) * 5.0 / 9.0),
            _ => None,
        },
        "%" => match unit.as_str() {
            "%" if (0.0..=1.0).contains(&value) => Some(value * 100.0),
            "%" | "percent" => Some(value),
            _ => None,
        },
        _ => scale(metric, canonical, &unit).map(|per_unit| value * per_unit),
    }
}

/// A canonical value in `unit` (one of the spellings [`display_unit`]
/// answers). Unknown units answer the value unchanged.
pub fn from_canonical(metric: Metric, value: f64, unit: &str) -> f64 {
    let Some(unit) = key(unit) else {
        return value;
    };
    let canonical = metric.info().unit;
    match canonical {
        "degC" => match unit.as_str() {
            "degf" | "°f" | "f" | "fahrenheit" => value * 9.0 / 5.0 + 32.0,
            _ => value,
        },
        // Percentages are shown as 0..=100, the way they are stored.
        "%" => value,
        _ => scale(metric, canonical, &unit).map_or(value, |per_unit| value / per_unit),
    }
}

/// The unit `metric` is shown in under `units`: `lb` for weight when the
/// person chose pounds, `mmol/L` for glucose, the canonical unit otherwise.
///
/// Where the canonical unit is not what anyone reads, the setting's other
/// choice stands in for it: distances are `km` (or `mi`), not metres, and
/// height and waist `cm` (or `in`), not metres. Temperatures answer
/// `degC` or `degF`, which the interface draws as °C and °F.
pub fn display_unit(metric: Metric, units: &HealthUnits) -> String {
    let chose = |setting: &str, spellings: &[&str]| {
        key(setting).is_some_and(|k| spellings.contains(&k.as_str()))
    };
    let shown = match metric {
        Metric::Weight | Metric::LeanBodyMass if chose(&units.weight, &["lb", "lbs"]) => "lb",
        Metric::WalkingRunningDistance | Metric::CyclingDistance | Metric::SwimmingDistance => {
            if chose(&units.distance, &["mi"]) {
                "mi"
            } else {
                "km"
            }
        }
        Metric::ActiveEnergy | Metric::BasalEnergy | Metric::DietaryEnergy
            if chose(&units.energy, &["kj"]) =>
        {
            "kJ"
        }
        Metric::BodyTemperature | Metric::WristTemperature
            if chose(&units.temperature, &["f", "degf", "°f"]) =>
        {
            "degF"
        }
        Metric::BloodGlucose if chose(&units.glucose, &["mmol/l"]) => "mmol/L",
        Metric::Height | Metric::WaistCircumference => {
            if chose(&units.length, &["in"]) {
                "in"
            } else {
                "cm"
            }
        }
        _ => metric.info().unit,
    };
    shown.to_string()
}

/// A unit spelling as a lookup key: lowercase, without spaces, with the
/// micro sign and the middle dot every spelling of them folded together
/// (`µg` and `μg` to `mcg`, `mL/(kg·min)` to `ml/kg/min`).
///
/// Two spellings differ only by case and mean different things: `Cal` is a
/// kilocalorie and `cal` a thousandth of one, and `Mg` is a megagram, which
/// no metric here is in, so it is refused rather than read as milligrams.
fn key(unit: &str) -> Option<String> {
    let unit = unit.trim();
    match unit {
        "Cal" => return Some("kcal".to_string()),
        "Mg" => return None,
        _ => {}
    }
    let mut folded = String::with_capacity(unit.len());
    for c in unit.chars() {
        match c {
            c if c.is_whitespace() || c == '(' || c == ')' => {}
            '\u{b5}' | '\u{3bc}' => folded.push_str("mc"),
            '\u{b7}' | '\u{22c5}' | '*' => folded.push('/'),
            c => folded.extend(c.to_lowercase()),
        }
    }
    // HealthKit writes a molar unit with the molar mass inside it. Glucose is
    // the only molar quantity here, and the factor is the one constant.
    if folded.starts_with("mmol<") && folded.ends_with(">/l") {
        return Some("mmol/l".to_string());
    }
    Some(folded)
}

/// How many of `metric`'s canonical units one `unit` (a [`key`]) is, for the
/// units that are a plain multiple. Temperature and percent are not and are
/// handled by the callers.
fn scale(metric: Metric, canonical: &str, unit: &str) -> Option<f64> {
    match canonical {
        "count" => match unit {
            "count" => Some(1.0),
            "kg/m2" | "kg/m\u{b2}" | "kg/m^2" if metric == Metric::Bmi => Some(1.0),
            _ => None,
        },
        "count/min" => match unit {
            "count/min" | "bpm" | "/min" | "beats/min" | "breaths/min" => Some(1.0),
            "count/s" | "count/sec" | "/s" => Some(60.0),
            _ => None,
        },
        "ms" => match unit {
            "ms" => Some(1.0),
            "s" | "sec" => Some(1000.0),
            _ => None,
        },
        "mL/kg/min" => match unit {
            "ml/kg/min" | "ml/min/kg" => Some(1.0),
            _ => None,
        },
        "min" => match unit {
            "min" | "mins" | "minute" | "minutes" => Some(1.0),
            "hr" | "h" | "hour" | "hours" => Some(60.0),
            "s" | "sec" | "second" | "seconds" => Some(1.0 / 60.0),
            _ => None,
        },
        "m" => metres(unit),
        "kcal" => match unit {
            "kcal" => Some(1.0),
            "cal" => Some(0.001),
            "kj" => Some(1.0 / 4.184),
            "j" => Some(1.0 / 4184.0),
            _ => None,
        },
        "kg" => milligrams(unit).map(|mg| mg / 1e6),
        "g" => milligrams(unit).map(|mg| mg / 1e3),
        "mg" => milligrams(unit),
        "mL" => match unit {
            "ml" => Some(1.0),
            "l" => Some(1000.0),
            "fl_oz_us" | "fl_oz" | "floz" => Some(29.5735295625),
            "fl_oz_imp" => Some(28.4130625),
            _ => None,
        },
        "mmHg" => match unit {
            "mmhg" => Some(1.0),
            "kpa" => Some(1000.0 / 133.322387415),
            _ => None,
        },
        "mg/dL" => match unit {
            "mg/dl" => Some(1.0),
            "mmol/l" => Some(MMOL_PER_L_TO_MG_PER_DL),
            _ => None,
        },
        "IU" => match unit {
            "iu" | "u" | "unit" | "units" => Some(1.0),
            _ => None,
        },
        "stage" => match unit {
            "stage" => Some(1.0),
            _ => None,
        },
        _ => None,
    }
}

/// Metres in one `unit` of length.
fn metres(unit: &str) -> Option<f64> {
    match unit {
        "m" | "meter" | "meters" | "metre" | "metres" => Some(1.0),
        "km" => Some(1000.0),
        "cm" => Some(0.01),
        "mm" => Some(0.001),
        "mi" | "mile" | "miles" => Some(1609.344),
        "yd" => Some(0.9144),
        "ft" => Some(0.3048),
        "in" => Some(0.0254),
        _ => None,
    }
}

/// Milligrams in one `unit` of mass.
fn milligrams(unit: &str) -> Option<f64> {
    match unit {
        "mg" => Some(1.0),
        "mcg" | "ug" => Some(0.001),
        "g" => Some(1e3),
        "kg" => Some(1e6),
        "oz" => Some(28_349.523125),
        "lb" | "lbs" => Some(453_592.37),
        "st" => Some(6_350_293.18),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    /// Every choice the settings offer that is not the default.
    fn imperial() -> HealthUnits {
        HealthUnits {
            glucose: "mmol/L".into(),
            weight: "lb".into(),
            distance: "mi".into(),
            energy: "kJ".into(),
            temperature: "F".into(),
            length: "in".into(),
        }
    }

    fn assert_converts(metric: Metric, value: f64, unit: &str, expected: f64) {
        let got = to_canonical(metric, value, unit)
            .unwrap_or_else(|| panic!("{unit} was refused for {metric:?}"));
        assert!(close(got, expected), "{value} {unit} as {metric:?}: {got}, not {expected}");
    }

    #[test]
    fn empty_unit_and_the_canonical_unit_pass_the_value_through() {
        for m in Metric::ALL {
            let info = m.info();
            // 50 is inside every percentage's real range, so the fraction
            // rule leaves it alone.
            assert_eq!(to_canonical(*m, 50.0, ""), Some(50.0), "{m:?}");
            assert_eq!(to_canonical(*m, 50.0, info.unit), Some(50.0), "{m:?} in {}", info.unit);
            assert_eq!(from_canonical(*m, 50.0, info.unit), 50.0, "{m:?}");
        }
    }

    #[test]
    fn a_value_that_is_not_a_number_is_refused() {
        assert_eq!(to_canonical(Metric::Weight, f64::NAN, "kg"), None);
        assert_eq!(to_canonical(Metric::Weight, f64::INFINITY, ""), None);
    }

    #[test]
    fn heart_and_breath_rates_in_every_spelling() {
        for unit in ["count/min", "bpm", "/min", "BPM"] {
            assert_converts(Metric::HeartRate, 62.0, unit, 62.0);
        }
        assert_converts(Metric::RespiratoryRate, 15.0, "count/min", 15.0);
        assert_converts(Metric::HeartRate, 1.0, "count/s", 60.0);
        assert_eq!(to_canonical(Metric::HeartRate, 62.0, "kg"), None);
    }

    #[test]
    fn heart_rate_variability_in_milliseconds_or_seconds() {
        assert_converts(Metric::HeartRateVariability, 48.0, "ms", 48.0);
        assert_converts(Metric::HeartRateVariability, 0.048, "s", 48.0);
    }

    #[test]
    fn mass_in_every_unit_a_weight_comes_in() {
        assert_converts(Metric::Weight, 80.0, "kg", 80.0);
        assert_converts(Metric::Weight, 80_000.0, "g", 80.0);
        assert_converts(Metric::Weight, 180.0, "lb", 81.646_626_6);
        assert_converts(Metric::Weight, 1.0, "lb", 0.453_592_37);
        assert_converts(Metric::Weight, 16.0, "oz", 0.453_592_37);
        assert_converts(Metric::Weight, 12.0, "st", 76.203_518_16);
        assert_converts(Metric::LeanBodyMass, 150.0, "lbs", 68.038_855_5);
        assert_eq!(to_canonical(Metric::Weight, 80.0, "mmol/L"), None);
        assert_eq!(to_canonical(Metric::Weight, 80.0, "m"), None);
    }

    #[test]
    fn length_in_every_unit_a_distance_or_height_comes_in() {
        assert_converts(Metric::WalkingRunningDistance, 5.0, "km", 5000.0);
        assert_converts(Metric::WalkingRunningDistance, 3.0, "mi", 4828.032);
        assert_converts(Metric::CyclingDistance, 1200.0, "m", 1200.0);
        assert_converts(Metric::CyclingDistance, 1.0, "yd", 0.9144);
        assert_converts(Metric::Height, 175.0, "cm", 1.75);
        assert_converts(Metric::Height, 5.0, "ft", 1.524);
        assert_converts(Metric::Height, 70.0, "in", 1.778);
        assert_converts(Metric::WaistCircumference, 900.0, "mm", 0.9);
        assert_converts(Metric::WaistCircumference, 34.0, "in", 0.8636);
        assert_eq!(to_canonical(Metric::Height, 175.0, "kg"), None);
    }

    #[test]
    fn energy_in_calories_and_joules() {
        assert_converts(Metric::ActiveEnergy, 300.0, "kcal", 300.0);
        assert_converts(Metric::ActiveEnergy, 300.0, "Cal", 300.0);
        assert_converts(Metric::ActiveEnergy, 300_000.0, "cal", 300.0);
        assert_converts(Metric::ActiveEnergy, 4184.0, "kJ", 1000.0);
        assert_converts(Metric::ActiveEnergy, 4184.0, "kj", 1000.0);
        assert_converts(Metric::DietaryEnergy, 4_184_000.0, "J", 1000.0);
        assert_eq!(to_canonical(Metric::BasalEnergy, 1.0, "g"), None);
    }

    #[test]
    fn time_in_minutes_hours_and_seconds() {
        assert_converts(Metric::ExerciseTime, 30.0, "min", 30.0);
        assert_converts(Metric::ExerciseTime, 1.5, "hr", 90.0);
        assert_converts(Metric::StandTime, 2.0, "h", 120.0);
        assert_converts(Metric::MindfulMinutes, 600.0, "s", 10.0);
        assert_eq!(to_canonical(Metric::ExerciseTime, 1.0, "kcal"), None);
    }

    #[test]
    fn temperature_in_celsius_and_fahrenheit() {
        for unit in ["degC", "°C", "C"] {
            assert_converts(Metric::BodyTemperature, 37.0, unit, 37.0);
        }
        for unit in ["degF", "°F", "F"] {
            assert_converts(Metric::BodyTemperature, 98.6, unit, 37.0);
        }
        assert_converts(Metric::WristTemperature, 95.0, "degF", 35.0);
        assert_eq!(to_canonical(Metric::BodyTemperature, 37.0, "kg"), None);
    }

    #[test]
    fn pressure_in_mmhg() {
        assert_converts(Metric::BloodPressureSystolic, 120.0, "mmHg", 120.0);
        assert_converts(Metric::BloodPressureDiastolic, 80.0, "mmhg", 80.0);
        assert_eq!(to_canonical(Metric::BloodPressureSystolic, 120.0, "bpm"), None);
    }

    #[test]
    fn glucose_converts_between_mg_dl_and_mmol_l_both_ways() {
        assert_converts(Metric::BloodGlucose, 99.0, "mg/dL", 99.0);
        assert_converts(Metric::BloodGlucose, 5.5, "mmol/L", 99.0858);
        assert_converts(Metric::BloodGlucose, 5.5, "mmol/l", 99.0858);
        assert_converts(Metric::BloodGlucose, 10.0, "mmol<180.1558800000541>/L", 180.156);
        assert!(close(from_canonical(Metric::BloodGlucose, 180.156, "mmol/L"), 10.0));
        assert!(close(from_canonical(Metric::BloodGlucose, 99.0858, "mmol/L"), 5.5));
        assert_eq!(from_canonical(Metric::BloodGlucose, 99.0, "mg/dL"), 99.0);
        // The 18-fold mistake the module doc warns of is a refusal: a glucose
        // is never a mass, and never in an unknown unit.
        assert_eq!(to_canonical(Metric::BloodGlucose, 5.5, "kg"), None);
        assert_eq!(to_canonical(Metric::BloodGlucose, 5.5, "mmol"), None);
    }

    #[test]
    fn insulin_in_units() {
        assert_converts(Metric::InsulinBolus, 4.0, "IU", 4.0);
        assert_converts(Metric::InsulinBasal, 20.0, "units", 20.0);
        assert_eq!(to_canonical(Metric::InsulinBolus, 4.0, "mg"), None);
    }

    #[test]
    fn dietary_mass_in_grams_milligrams_and_micrograms() {
        assert_converts(Metric::DietaryCarbs, 45.0, "g", 45.0);
        assert_converts(Metric::DietaryProtein, 25_000.0, "mg", 25.0);
        assert_converts(Metric::DietaryFiber, 1.0, "oz", 28.349_523_125);
        assert_converts(Metric::DietarySodium, 2.3, "g", 2300.0);
        assert_converts(Metric::DietarySodium, 500.0, "mg", 500.0);
        assert_converts(Metric::DietaryCaffeine, 95_000.0, "mcg", 95.0);
        assert_converts(Metric::DietaryCaffeine, 95_000.0, "\u{b5}g", 95.0);
        assert_converts(Metric::DietaryCaffeine, 95_000.0, "\u{3bc}g", 95.0);
        assert_eq!(to_canonical(Metric::DietaryCarbs, 45.0, "kcal"), None);
    }

    #[test]
    fn mg_and_megagram_are_not_the_same_spelling() {
        assert_converts(Metric::DietarySodium, 500.0, "mg", 500.0);
        assert_eq!(to_canonical(Metric::DietarySodium, 500.0, "Mg"), None);
    }

    #[test]
    fn volume_in_millilitres_litres_and_ounces() {
        assert_converts(Metric::Water, 250.0, "mL", 250.0);
        assert_converts(Metric::Water, 250.0, "ml", 250.0);
        assert_converts(Metric::Water, 1.5, "L", 1500.0);
        assert_converts(Metric::Water, 8.0, "fl_oz_us", 236.588_236_5);
        assert_eq!(to_canonical(Metric::Water, 8.0, "kg"), None);
    }

    #[test]
    fn vo2_max_in_both_healthkit_and_plain_spellings() {
        for unit in ["mL/(kg\u{b7}min)", "ml/(kg*min)", "mL/kg/min", "ml/kg/min"] {
            assert_converts(Metric::Vo2Max, 42.0, unit, 42.0);
        }
        assert_eq!(to_canonical(Metric::Vo2Max, 42.0, "mL"), None);
    }

    #[test]
    fn counts_and_the_body_mass_index() {
        assert_converts(Metric::Steps, 1200.0, "count", 1200.0);
        assert_converts(Metric::FlightsClimbed, 4.0, "count", 4.0);
        assert_converts(Metric::Bmi, 24.5, "count", 24.5);
        assert_converts(Metric::Bmi, 24.5, "kg/m\u{b2}", 24.5);
        assert_eq!(to_canonical(Metric::Steps, 1200.0, "kg/m2"), None);
        assert_eq!(to_canonical(Metric::Steps, 1200.0, "km"), None);
    }

    #[test]
    fn a_percent_value_of_one_or_less_is_a_fraction_for_oxygen_and_body_fat() {
        // HealthKit's export: 0.97 under `%`.
        assert_converts(Metric::OxygenSaturation, 0.97, "%", 97.0);
        assert_converts(Metric::BodyFat, 0.225, "%", 22.5);
        // Health Auto Export: 97 under `%`.
        assert_converts(Metric::OxygenSaturation, 97.0, "%", 97.0);
        assert_converts(Metric::BodyFat, 22.5, "%", 22.5);
        // Exactly one is a fraction; the boundary is inclusive.
        assert_converts(Metric::OxygenSaturation, 1.0, "%", 100.0);
        // No unit means canonical already, fraction rule or not.
        assert_eq!(to_canonical(Metric::OxygenSaturation, 0.97, ""), Some(0.97));
        // A spelled-out unit is taken at its word.
        assert_converts(Metric::BodyFat, 0.5, "percent", 0.5);
        assert_eq!(to_canonical(Metric::OxygenSaturation, 97.0, "kg"), None);
    }

    #[test]
    fn stage_values_are_taken_as_they_come() {
        assert_converts(Metric::SleepStage, 3.0, "stage", 3.0);
        assert_eq!(to_canonical(Metric::SleepStage, 3.0, "hr"), None);
    }

    #[test]
    fn display_follows_the_persons_units() {
        let defaults = HealthUnits::default();
        for m in Metric::ALL {
            let shown = display_unit(*m, &defaults);
            let expected = match m {
                Metric::WalkingRunningDistance
                | Metric::CyclingDistance
                | Metric::SwimmingDistance => "km",
                Metric::Height | Metric::WaistCircumference => "cm",
                other => other.info().unit,
            };
            assert_eq!(shown, expected, "{m:?} under the defaults");
        }

        let units = imperial();
        let shown = |m| display_unit(m, &units);
        assert_eq!(shown(Metric::Weight), "lb");
        assert_eq!(shown(Metric::LeanBodyMass), "lb");
        assert_eq!(shown(Metric::WalkingRunningDistance), "mi");
        assert_eq!(shown(Metric::CyclingDistance), "mi");
        assert_eq!(shown(Metric::ActiveEnergy), "kJ");
        assert_eq!(shown(Metric::BasalEnergy), "kJ");
        assert_eq!(shown(Metric::DietaryEnergy), "kJ");
        assert_eq!(shown(Metric::BodyTemperature), "degF");
        assert_eq!(shown(Metric::WristTemperature), "degF");
        assert_eq!(shown(Metric::BloodGlucose), "mmol/L");
        assert_eq!(shown(Metric::Height), "in");
        assert_eq!(shown(Metric::WaistCircumference), "in");
        // Nothing else moves.
        assert_eq!(shown(Metric::HeartRate), "count/min");
        assert_eq!(shown(Metric::Steps), "count");
        assert_eq!(shown(Metric::BloodPressureSystolic), "mmHg");
    }

    #[test]
    fn from_canonical_mirrors_every_unit_display_answers() {
        let units = imperial();
        let in_display = |m: Metric, v: f64| from_canonical(m, v, &display_unit(m, &units));
        assert!(close(in_display(Metric::Weight, 81.646_626_6), 180.0));
        assert!(close(in_display(Metric::WalkingRunningDistance, 4828.032), 3.0));
        assert!(close(in_display(Metric::ActiveEnergy, 1000.0), 4184.0));
        assert!(close(in_display(Metric::BodyTemperature, 37.0), 98.6));
        assert!(close(in_display(Metric::BloodGlucose, 99.0858), 5.5));
        assert!(close(in_display(Metric::Height, 1.778), 70.0));

        let defaults = HealthUnits::default();
        let in_default = |m: Metric, v: f64| from_canonical(m, v, &display_unit(m, &defaults));
        assert!(close(in_default(Metric::Height, 1.75), 175.0));
        assert!(close(in_default(Metric::WalkingRunningDistance, 5000.0), 5.0));
        assert!(close(in_default(Metric::Weight, 80.0), 80.0));
        assert!(close(in_default(Metric::BloodGlucose, 99.0), 99.0));
    }

    #[test]
    fn a_unit_from_canonical_does_not_know_leaves_the_value_alone() {
        assert_eq!(from_canonical(Metric::Weight, 80.0, "furlongs"), 80.0);
        assert_eq!(from_canonical(Metric::Weight, 80.0, ""), 80.0);
        assert_eq!(from_canonical(Metric::Weight, 80.0, "Mg"), 80.0);
        assert_eq!(from_canonical(Metric::OxygenSaturation, 97.0, "%"), 97.0);
    }

    #[test]
    fn every_conversion_round_trips() {
        let cases = [
            (Metric::Weight, 81.2, "lb"),
            (Metric::Height, 1.83, "in"),
            (Metric::Height, 1.83, "ft"),
            (Metric::WalkingRunningDistance, 8046.72, "mi"),
            (Metric::ActiveEnergy, 640.0, "kJ"),
            (Metric::BodyTemperature, 36.6, "degF"),
            (Metric::BloodGlucose, 126.0, "mmol/L"),
            (Metric::ExerciseTime, 45.0, "h"),
            (Metric::DietaryCaffeine, 80.0, "g"),
            (Metric::Water, 500.0, "fl_oz_us"),
        ];
        for (metric, canonical, unit) in cases {
            let shown = from_canonical(metric, canonical, unit);
            let back = to_canonical(metric, shown, unit).unwrap();
            assert!(close(back, canonical), "{metric:?} through {unit}: {back} vs {canonical}");
        }
    }
}
