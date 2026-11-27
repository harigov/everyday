//! The metric catalogue: every kind of sample Health keeps, its canonical
//! unit, how it adds up, and what each connector calls it.
//!
//! A closed enum rather than free strings, so every match over it is
//! checked: adding a metric is a variant here, a row in [`Metric::info`],
//! and nothing else -- the connectors find it through
//! [`Metric::from_apple`] and [`Metric::from_hae`], the interface through
//! the `health_catalog` command.

use super::Stat;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    // activity
    Steps,
    WalkingRunningDistance,
    CyclingDistance,
    SwimmingDistance,
    ActiveEnergy,
    BasalEnergy,
    ExerciseTime,
    StandTime,
    /// One sample per hour stood in (value 1), Apple's stand ring.
    StandHour,
    FlightsClimbed,
    TimeInDaylight,
    // heart
    HeartRate,
    RestingHeartRate,
    WalkingHeartRate,
    /// SDNN, milliseconds -- what Apple Watch measures.
    HeartRateVariability,
    HeartRateRecovery,
    Vo2Max,
    // respiratory
    RespiratoryRate,
    OxygenSaturation,
    // body
    Weight,
    BodyFat,
    LeanBodyMass,
    Bmi,
    Height,
    WaistCircumference,
    BodyTemperature,
    /// The wrist's temperature while asleep, °C, as Apple Watch records it.
    WristTemperature,
    // vitals
    BloodPressureSystolic,
    BloodPressureDiastolic,
    // metabolic
    BloodGlucose,
    InsulinBasal,
    InsulinBolus,
    // nutrition other apps wrote to the phone
    DietaryEnergy,
    DietaryCarbs,
    DietaryProtein,
    DietaryFat,
    DietaryFiber,
    DietarySugar,
    DietarySodium,
    DietaryCaffeine,
    Water,
    // sleep and mind
    /// A category metric: each sample is one stretch of one
    /// [`SleepStage`](super::SleepStage), its value the stage's
    /// [`code`](super::SleepStage::code).
    SleepStage,
    MindfulMinutes,
}

/// How a metric's samples add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum MetricKind {
    /// Amounts over intervals that add up: steps, energy, distance. Two
    /// sources counting the same walk would double it, so a quarter hour
    /// counts only the highest-priority source in it.
    Cumulative,
    /// A measurement at an instant (or averaged over a short interval):
    /// heart rate, weight, glucose. Sources are pooled.
    Discrete,
    /// Coded states over intervals: sleep stages, stand hours.
    Category,
}

/// Where a metric is drawn in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum MetricGroup {
    Activity,
    Heart,
    Respiratory,
    Body,
    Vitals,
    Metabolic,
    Nutrition,
    Sleep,
    Mind,
}

/// Everything the catalogue knows about one metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricInfo {
    pub metric: Metric,
    /// What the interface calls it: "Resting heart rate".
    pub name: &'static str,
    /// The canonical unit every stored value is in: `count`, `m`, `kcal`,
    /// `min`, `count/min`, `ms`, `mL/kg/min`, `%` (as 0..=100), `kg`, `m`,
    /// `degC`, `mmHg`, `mg/dL`, `IU`, `g`, `mg`, `mL`, `stage`.
    pub unit: &'static str,
    pub kind: MetricKind,
    pub group: MetricGroup,
    /// How a day of it is summed up when nobody says: steps are a sum,
    /// resting heart rate a mean, weight the last of the day.
    pub stat: Stat,
    /// Plausible canonical range. A value outside it is refused on the way
    /// in as a sensor glitch or a unit mix-up (a weight of 180 "kg" that was
    /// pounds is inside; 1800 is not).
    pub min: f64,
    pub max: f64,
    /// Apple HealthKit identifiers, as `export.xml` spells them
    /// (`HKQuantityTypeIdentifierHeartRate`).
    pub apple: &'static [&'static str],
    /// Health Auto Export metric names (`heart_rate`, `step_count`).
    pub hae: &'static [&'static str],
}

/// What the `health_catalog` command answers for one metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MetricEntry {
    pub metric: Metric,
    pub name: String,
    pub unit: String,
    pub kind: MetricKind,
    pub group: MetricGroup,
    pub stat: Stat,
}

impl Metric {
    /// Every metric, in catalogue order.
    pub const ALL: &'static [Metric] = &[
        Metric::Steps,
        Metric::WalkingRunningDistance,
        Metric::CyclingDistance,
        Metric::SwimmingDistance,
        Metric::ActiveEnergy,
        Metric::BasalEnergy,
        Metric::ExerciseTime,
        Metric::StandTime,
        Metric::StandHour,
        Metric::FlightsClimbed,
        Metric::TimeInDaylight,
        Metric::HeartRate,
        Metric::RestingHeartRate,
        Metric::WalkingHeartRate,
        Metric::HeartRateVariability,
        Metric::HeartRateRecovery,
        Metric::Vo2Max,
        Metric::RespiratoryRate,
        Metric::OxygenSaturation,
        Metric::Weight,
        Metric::BodyFat,
        Metric::LeanBodyMass,
        Metric::Bmi,
        Metric::Height,
        Metric::WaistCircumference,
        Metric::BodyTemperature,
        Metric::WristTemperature,
        Metric::BloodPressureSystolic,
        Metric::BloodPressureDiastolic,
        Metric::BloodGlucose,
        Metric::InsulinBasal,
        Metric::InsulinBolus,
        Metric::DietaryEnergy,
        Metric::DietaryCarbs,
        Metric::DietaryProtein,
        Metric::DietaryFat,
        Metric::DietaryFiber,
        Metric::DietarySugar,
        Metric::DietarySodium,
        Metric::DietaryCaffeine,
        Metric::Water,
        Metric::SleepStage,
        Metric::MindfulMinutes,
    ];

    /// The catalogue row. Exhaustive: a new variant does not compile until
    /// it has one.
    ///
    /// Ranges are canonical and per sample, generous enough for a real
    /// extreme and tight enough to catch the usual unit mix-ups: a glucose of
    /// 5.5 that was mmol/L, a body fat of 0.3 that was a fraction left
    /// unconverted. The wrist temperature range is wide on purpose: the
    /// variant's comment says deviation, but HealthKit writes the sleeping
    /// wrist temperature itself (around 35 degC), and both must pass.
    ///
    /// Blood pressure, sleep and insulin each arrive under one name that
    /// carries several things, and the connectors split them. HAE's
    /// `blood_pressure` is therefore listed on neither pressure variant.
    /// Insulin is one HealthKit type (`HKQuantityTypeIdentifierInsulinDelivery`)
    /// and one HAE metric (`insulin_delivery`) with the reason, basal or
    /// bolus, as metadata: it is listed on neither [`Metric::InsulinBasal`]
    /// nor [`Metric::InsulinBolus`], and each connector files a delivery under
    /// the one its reason names. `sleep_analysis` is the stage metric's own;
    /// the connector turns each segment into a stage code.
    pub fn info(self) -> MetricInfo {
        match self {
            Metric::Steps => MetricInfo {
                metric: self,
                name: "Steps",
                unit: "count",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 200_000.0,
                apple: &["HKQuantityTypeIdentifierStepCount"],
                hae: &["step_count"],
            },
            Metric::WalkingRunningDistance => MetricInfo {
                metric: self,
                name: "Walking and running distance",
                unit: "m",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 250_000.0,
                apple: &["HKQuantityTypeIdentifierDistanceWalkingRunning"],
                hae: &["walking_running_distance"],
            },
            Metric::CyclingDistance => MetricInfo {
                metric: self,
                name: "Cycling distance",
                unit: "m",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 500_000.0,
                apple: &["HKQuantityTypeIdentifierDistanceCycling"],
                hae: &["cycling_distance"],
            },
            Metric::SwimmingDistance => MetricInfo {
                metric: self,
                name: "Swimming distance",
                unit: "m",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 50_000.0,
                apple: &["HKQuantityTypeIdentifierDistanceSwimming"],
                hae: &["swimming_distance"],
            },
            Metric::ActiveEnergy => MetricInfo {
                metric: self,
                name: "Active energy",
                unit: "kcal",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 20_000.0,
                apple: &["HKQuantityTypeIdentifierActiveEnergyBurned"],
                hae: &["active_energy"],
            },
            Metric::BasalEnergy => MetricInfo {
                metric: self,
                name: "Resting energy",
                unit: "kcal",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 20_000.0,
                apple: &["HKQuantityTypeIdentifierBasalEnergyBurned"],
                hae: &["basal_energy_burned"],
            },
            Metric::ExerciseTime => MetricInfo {
                metric: self,
                name: "Exercise time",
                unit: "min",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 2_160.0,
                apple: &["HKQuantityTypeIdentifierAppleExerciseTime"],
                hae: &["apple_exercise_time"],
            },
            Metric::StandTime => MetricInfo {
                metric: self,
                name: "Stand time",
                unit: "min",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 2_160.0,
                apple: &["HKQuantityTypeIdentifierAppleStandTime"],
                hae: &["apple_stand_time"],
            },
            Metric::StandHour => MetricInfo {
                metric: self,
                name: "Stand hours",
                unit: "count",
                kind: MetricKind::Category,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 1.0,
                apple: &["HKCategoryTypeIdentifierAppleStandHour"],
                hae: &["apple_stand_hour"],
            },
            Metric::FlightsClimbed => MetricInfo {
                metric: self,
                name: "Flights climbed",
                unit: "count",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 2_000.0,
                apple: &["HKQuantityTypeIdentifierFlightsClimbed"],
                hae: &["flights_climbed"],
            },
            Metric::TimeInDaylight => MetricInfo {
                metric: self,
                name: "Time in daylight",
                unit: "min",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Activity,
                stat: Stat::Sum,
                min: 0.0,
                max: 1_440.0,
                apple: &["HKQuantityTypeIdentifierTimeInDaylight"],
                hae: &["time_in_daylight"],
            },
            Metric::HeartRate => MetricInfo {
                metric: self,
                name: "Heart rate",
                unit: "count/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Mean,
                min: 20.0,
                max: 300.0,
                apple: &["HKQuantityTypeIdentifierHeartRate"],
                hae: &["heart_rate"],
            },
            Metric::RestingHeartRate => MetricInfo {
                metric: self,
                name: "Resting heart rate",
                unit: "count/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Mean,
                min: 20.0,
                max: 200.0,
                apple: &["HKQuantityTypeIdentifierRestingHeartRate"],
                hae: &["resting_heart_rate"],
            },
            Metric::WalkingHeartRate => MetricInfo {
                metric: self,
                name: "Walking heart rate",
                unit: "count/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Mean,
                min: 30.0,
                max: 250.0,
                apple: &["HKQuantityTypeIdentifierWalkingHeartRateAverage"],
                hae: &["walking_heart_rate_average"],
            },
            Metric::HeartRateVariability => MetricInfo {
                metric: self,
                name: "Heart rate variability",
                unit: "ms",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Mean,
                min: 1.0,
                max: 1_000.0,
                apple: &["HKQuantityTypeIdentifierHeartRateVariabilitySDNN"],
                hae: &["heart_rate_variability"],
            },
            Metric::HeartRateRecovery => MetricInfo {
                metric: self,
                name: "Heart rate recovery",
                unit: "count/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Mean,
                min: 0.0,
                max: 200.0,
                apple: &["HKQuantityTypeIdentifierHeartRateRecoveryOneMinute"],
                hae: &["cardio_recovery"],
            },
            Metric::Vo2Max => MetricInfo {
                metric: self,
                name: "VO2 max",
                unit: "mL/kg/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Heart,
                stat: Stat::Last,
                min: 5.0,
                max: 100.0,
                apple: &["HKQuantityTypeIdentifierVO2Max"],
                hae: &["vo2_max"],
            },
            Metric::RespiratoryRate => MetricInfo {
                metric: self,
                name: "Respiratory rate",
                unit: "count/min",
                kind: MetricKind::Discrete,
                group: MetricGroup::Respiratory,
                stat: Stat::Mean,
                min: 1.0,
                max: 100.0,
                apple: &["HKQuantityTypeIdentifierRespiratoryRate"],
                hae: &["respiratory_rate"],
            },
            Metric::OxygenSaturation => MetricInfo {
                metric: self,
                name: "Blood oxygen",
                unit: "%",
                kind: MetricKind::Discrete,
                group: MetricGroup::Respiratory,
                stat: Stat::Mean,
                min: 50.0,
                max: 100.0,
                apple: &["HKQuantityTypeIdentifierOxygenSaturation"],
                hae: &["blood_oxygen_saturation"],
            },
            Metric::Weight => MetricInfo {
                metric: self,
                name: "Weight",
                unit: "kg",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 0.5,
                max: 700.0,
                apple: &["HKQuantityTypeIdentifierBodyMass"],
                hae: &["weight_body_mass"],
            },
            Metric::BodyFat => MetricInfo {
                metric: self,
                name: "Body fat",
                unit: "%",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 1.0,
                max: 80.0,
                apple: &["HKQuantityTypeIdentifierBodyFatPercentage"],
                hae: &["body_fat_percentage"],
            },
            Metric::LeanBodyMass => MetricInfo {
                metric: self,
                name: "Lean body mass",
                unit: "kg",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 1.0,
                max: 400.0,
                apple: &["HKQuantityTypeIdentifierLeanBodyMass"],
                hae: &["lean_body_mass"],
            },
            Metric::Bmi => MetricInfo {
                metric: self,
                name: "Body mass index",
                unit: "count",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 5.0,
                max: 100.0,
                apple: &["HKQuantityTypeIdentifierBodyMassIndex"],
                hae: &["body_mass_index"],
            },
            Metric::Height => MetricInfo {
                metric: self,
                name: "Height",
                unit: "m",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 0.3,
                max: 2.8,
                apple: &["HKQuantityTypeIdentifierHeight"],
                hae: &["height"],
            },
            Metric::WaistCircumference => MetricInfo {
                metric: self,
                name: "Waist circumference",
                unit: "m",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Last,
                min: 0.2,
                max: 3.0,
                apple: &["HKQuantityTypeIdentifierWaistCircumference"],
                hae: &["waist_circumference"],
            },
            Metric::BodyTemperature => MetricInfo {
                metric: self,
                name: "Body temperature",
                unit: "degC",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Mean,
                min: 25.0,
                max: 45.0,
                apple: &["HKQuantityTypeIdentifierBodyTemperature"],
                hae: &["body_temperature"],
            },
            Metric::WristTemperature => MetricInfo {
                metric: self,
                name: "Wrist temperature",
                unit: "degC",
                kind: MetricKind::Discrete,
                group: MetricGroup::Body,
                stat: Stat::Mean,
                min: -15.0,
                max: 45.0,
                apple: &["HKQuantityTypeIdentifierAppleSleepingWristTemperature"],
                hae: &["apple_sleeping_wrist_temperature"],
            },
            Metric::BloodPressureSystolic => MetricInfo {
                metric: self,
                name: "Systolic blood pressure",
                unit: "mmHg",
                kind: MetricKind::Discrete,
                group: MetricGroup::Vitals,
                stat: Stat::Mean,
                min: 40.0,
                max: 300.0,
                apple: &["HKQuantityTypeIdentifierBloodPressureSystolic"],
                hae: &[],
            },
            Metric::BloodPressureDiastolic => MetricInfo {
                metric: self,
                name: "Diastolic blood pressure",
                unit: "mmHg",
                kind: MetricKind::Discrete,
                group: MetricGroup::Vitals,
                stat: Stat::Mean,
                min: 20.0,
                max: 200.0,
                apple: &["HKQuantityTypeIdentifierBloodPressureDiastolic"],
                hae: &[],
            },
            Metric::BloodGlucose => MetricInfo {
                metric: self,
                name: "Blood glucose",
                unit: "mg/dL",
                kind: MetricKind::Discrete,
                group: MetricGroup::Metabolic,
                stat: Stat::Mean,
                min: 20.0,
                max: 1_500.0,
                apple: &["HKQuantityTypeIdentifierBloodGlucose"],
                hae: &["blood_glucose"],
            },
            Metric::InsulinBasal => MetricInfo {
                metric: self,
                name: "Basal insulin",
                unit: "IU",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Metabolic,
                stat: Stat::Sum,
                min: 0.0,
                max: 300.0,
                apple: &[],
                hae: &[],
            },
            Metric::InsulinBolus => MetricInfo {
                metric: self,
                name: "Bolus insulin",
                unit: "IU",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Metabolic,
                stat: Stat::Sum,
                min: 0.0,
                max: 300.0,
                apple: &[],
                hae: &[],
            },
            Metric::DietaryEnergy => MetricInfo {
                metric: self,
                name: "Dietary energy",
                unit: "kcal",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 20_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryEnergyConsumed"],
                hae: &["dietary_energy"],
            },
            Metric::DietaryCarbs => MetricInfo {
                metric: self,
                name: "Carbohydrates",
                unit: "g",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 2_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryCarbohydrates"],
                hae: &["carbohydrates"],
            },
            Metric::DietaryProtein => MetricInfo {
                metric: self,
                name: "Protein",
                unit: "g",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 1_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryProtein"],
                hae: &["protein"],
            },
            Metric::DietaryFat => MetricInfo {
                metric: self,
                name: "Fat",
                unit: "g",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 1_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryFatTotal"],
                hae: &["total_fat"],
            },
            Metric::DietaryFiber => MetricInfo {
                metric: self,
                name: "Fiber",
                unit: "g",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 500.0,
                apple: &["HKQuantityTypeIdentifierDietaryFiber"],
                hae: &["fiber"],
            },
            Metric::DietarySugar => MetricInfo {
                metric: self,
                name: "Sugar",
                unit: "g",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 1_500.0,
                apple: &["HKQuantityTypeIdentifierDietarySugar"],
                hae: &["dietary_sugar"],
            },
            Metric::DietarySodium => MetricInfo {
                metric: self,
                name: "Sodium",
                unit: "mg",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 100_000.0,
                apple: &["HKQuantityTypeIdentifierDietarySodium"],
                hae: &["sodium"],
            },
            Metric::DietaryCaffeine => MetricInfo {
                metric: self,
                name: "Caffeine",
                unit: "mg",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 5_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryCaffeine"],
                hae: &["caffeine"],
            },
            Metric::Water => MetricInfo {
                metric: self,
                name: "Water",
                unit: "mL",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Nutrition,
                stat: Stat::Sum,
                min: 0.0,
                max: 20_000.0,
                apple: &["HKQuantityTypeIdentifierDietaryWater"],
                hae: &["dietary_water"],
            },
            Metric::SleepStage => MetricInfo {
                metric: self,
                name: "Sleep stage",
                unit: "stage",
                kind: MetricKind::Category,
                group: MetricGroup::Sleep,
                stat: Stat::Count,
                min: 0.0,
                max: 5.0,
                apple: &["HKCategoryTypeIdentifierSleepAnalysis"],
                hae: &["sleep_analysis"],
            },
            Metric::MindfulMinutes => MetricInfo {
                metric: self,
                name: "Mindful minutes",
                unit: "min",
                kind: MetricKind::Cumulative,
                group: MetricGroup::Mind,
                stat: Stat::Sum,
                min: 0.0,
                max: 2_160.0,
                apple: &["HKCategoryTypeIdentifierMindfulSession"],
                hae: &["mindful_minutes"],
            },
        }
    }

    /// The snake_case name serde gives it -- what a series hash is computed
    /// from and what the export's file names use. Never changes for a
    /// variant: renaming one would orphan every stored day of it.
    pub fn key(self) -> &'static str {
        match self {
            Metric::Steps => "steps",
            Metric::WalkingRunningDistance => "walking_running_distance",
            Metric::CyclingDistance => "cycling_distance",
            Metric::SwimmingDistance => "swimming_distance",
            Metric::ActiveEnergy => "active_energy",
            Metric::BasalEnergy => "basal_energy",
            Metric::ExerciseTime => "exercise_time",
            Metric::StandTime => "stand_time",
            Metric::StandHour => "stand_hour",
            Metric::FlightsClimbed => "flights_climbed",
            Metric::TimeInDaylight => "time_in_daylight",
            Metric::HeartRate => "heart_rate",
            Metric::RestingHeartRate => "resting_heart_rate",
            Metric::WalkingHeartRate => "walking_heart_rate",
            Metric::HeartRateVariability => "heart_rate_variability",
            Metric::HeartRateRecovery => "heart_rate_recovery",
            Metric::Vo2Max => "vo2_max",
            Metric::RespiratoryRate => "respiratory_rate",
            Metric::OxygenSaturation => "oxygen_saturation",
            Metric::Weight => "weight",
            Metric::BodyFat => "body_fat",
            Metric::LeanBodyMass => "lean_body_mass",
            Metric::Bmi => "bmi",
            Metric::Height => "height",
            Metric::WaistCircumference => "waist_circumference",
            Metric::BodyTemperature => "body_temperature",
            Metric::WristTemperature => "wrist_temperature",
            Metric::BloodPressureSystolic => "blood_pressure_systolic",
            Metric::BloodPressureDiastolic => "blood_pressure_diastolic",
            Metric::BloodGlucose => "blood_glucose",
            Metric::InsulinBasal => "insulin_basal",
            Metric::InsulinBolus => "insulin_bolus",
            Metric::DietaryEnergy => "dietary_energy",
            Metric::DietaryCarbs => "dietary_carbs",
            Metric::DietaryProtein => "dietary_protein",
            Metric::DietaryFat => "dietary_fat",
            Metric::DietaryFiber => "dietary_fiber",
            Metric::DietarySugar => "dietary_sugar",
            Metric::DietarySodium => "dietary_sodium",
            Metric::DietaryCaffeine => "dietary_caffeine",
            Metric::Water => "water",
            Metric::SleepStage => "sleep_stage",
            Metric::MindfulMinutes => "mindful_minutes",
        }
    }

    pub fn from_key(key: &str) -> Option<Metric> {
        Metric::ALL.iter().copied().find(|m| m.key() == key)
    }

    /// The metric an `export.xml` record type is, if any.
    pub fn from_apple(identifier: &str) -> Option<Metric> {
        Metric::ALL.iter().copied().find(|m| m.info().apple.contains(&identifier))
    }

    /// The metric a Health Auto Export metric name is, if any. Blood
    /// pressure, sleep and insulin arrive as one name carrying several
    /// values; the connector splits those itself.
    pub fn from_hae(name: &str) -> Option<Metric> {
        Metric::ALL.iter().copied().find(|m| m.info().hae.contains(&name))
    }

    pub fn entry(self) -> MetricEntry {
        let info = self.info();
        MetricEntry {
            metric: self,
            name: info.name.to_string(),
            unit: info.unit.to_string(),
            kind: info.kind,
            group: info.group,
            stat: info.stat,
        }
    }
}

/// The whole catalogue, for the interface.
pub fn catalog() -> Vec<MetricEntry> {
    Metric::ALL.iter().map(|m| m.entry()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A match with no wildcard, so a new variant stops this file compiling
    /// until it is numbered here and listed in [`Metric::ALL`].
    fn ordinal(metric: Metric) -> usize {
        match metric {
            Metric::Steps => 0,
            Metric::WalkingRunningDistance => 1,
            Metric::CyclingDistance => 2,
            Metric::SwimmingDistance => 3,
            Metric::ActiveEnergy => 4,
            Metric::BasalEnergy => 5,
            Metric::ExerciseTime => 6,
            Metric::StandTime => 7,
            Metric::StandHour => 8,
            Metric::FlightsClimbed => 9,
            Metric::TimeInDaylight => 10,
            Metric::HeartRate => 11,
            Metric::RestingHeartRate => 12,
            Metric::WalkingHeartRate => 13,
            Metric::HeartRateVariability => 14,
            Metric::HeartRateRecovery => 15,
            Metric::Vo2Max => 16,
            Metric::RespiratoryRate => 17,
            Metric::OxygenSaturation => 18,
            Metric::Weight => 19,
            Metric::BodyFat => 20,
            Metric::LeanBodyMass => 21,
            Metric::Bmi => 22,
            Metric::Height => 23,
            Metric::WaistCircumference => 24,
            Metric::BodyTemperature => 25,
            Metric::WristTemperature => 26,
            Metric::BloodPressureSystolic => 27,
            Metric::BloodPressureDiastolic => 28,
            Metric::BloodGlucose => 29,
            Metric::InsulinBasal => 30,
            Metric::InsulinBolus => 31,
            Metric::DietaryEnergy => 32,
            Metric::DietaryCarbs => 33,
            Metric::DietaryProtein => 34,
            Metric::DietaryFat => 35,
            Metric::DietaryFiber => 36,
            Metric::DietarySugar => 37,
            Metric::DietarySodium => 38,
            Metric::DietaryCaffeine => 39,
            Metric::Water => 40,
            Metric::SleepStage => 41,
            Metric::MindfulMinutes => 42,
        }
    }

    #[test]
    fn every_key_is_what_serde_writes_and_round_trips() {
        for m in Metric::ALL {
            let json = serde_json::to_string(m).unwrap();
            assert_eq!(json, format!("\"{}\"", m.key()));
            assert_eq!(Metric::from_key(m.key()), Some(*m));
        }
    }

    #[test]
    fn all_lists_every_variant_exactly_once_in_catalogue_order() {
        // Each variant has its own ordinal, 0..43, so ordinals that match the
        // positions and a list of 43 can only be every variant once.
        for (position, m) in Metric::ALL.iter().enumerate() {
            assert_eq!(ordinal(*m), position, "{m:?} is out of place in ALL");
        }
        assert_eq!(Metric::ALL.len(), 43);
    }

    #[test]
    fn every_row_describes_its_own_metric_with_a_real_range() {
        for m in Metric::ALL {
            let info = m.info();
            assert_eq!(info.metric, *m);
            assert!(!info.name.is_empty(), "{m:?} has no name");
            assert!(!info.unit.is_empty(), "{m:?} has no unit");
            assert!(info.min.is_finite() && info.max.is_finite(), "{m:?} has an open range");
            assert!(info.min < info.max, "{m:?} has an empty range");
        }
    }

    #[test]
    fn every_default_stat_is_a_concrete_one() {
        for m in Metric::ALL {
            let info = m.info();
            assert!(!matches!(info.stat, Stat::Auto | Stat::InRange { .. }), "{m:?}");
            // Amounts that add up are summed unless they are the counted states.
            if info.kind == MetricKind::Cumulative {
                assert_eq!(info.stat, Stat::Sum, "{m:?}");
            }
        }
        assert_eq!(Metric::StandHour.info().kind, MetricKind::Category);
        assert_eq!(Metric::StandHour.info().stat, Stat::Sum);
        assert_eq!(Metric::SleepStage.info().kind, MetricKind::Category);
        assert_eq!(Metric::SleepStage.info().stat, Stat::Count);
        assert_eq!(Metric::RestingHeartRate.info().stat, Stat::Mean);
        assert_eq!(Metric::Weight.info().stat, Stat::Last);
        assert_eq!(Metric::Vo2Max.info().stat, Stat::Last);
        assert_eq!(Metric::BloodGlucose.info().kind, MetricKind::Discrete);
        assert_eq!(Metric::Steps.info().kind, MetricKind::Cumulative);
    }

    #[test]
    fn no_apple_or_hae_name_belongs_to_two_metrics() {
        let mut apple = HashSet::new();
        let mut hae = HashSet::new();
        for m in Metric::ALL {
            let info = m.info();
            for name in info.apple {
                assert!(name.starts_with("HK"), "{name} is not a HealthKit identifier");
                assert!(apple.insert(*name), "{name} is listed twice");
                assert_eq!(Metric::from_apple(name), Some(*m));
            }
            for name in info.hae {
                assert!(
                    name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{name} is not snake_case"
                );
                assert!(hae.insert(*name), "{name} is listed twice");
                assert_eq!(Metric::from_hae(name), Some(*m));
            }
        }
    }

    #[test]
    fn the_names_that_connectors_split_are_listed_where_they_belong() {
        // One HealthKit type and one HAE metric each for these, split by the
        // connectors, so no single variant may claim them.
        assert_eq!(Metric::from_hae("blood_pressure"), None);
        assert_eq!(Metric::from_hae("insulin_delivery"), None);
        assert_eq!(Metric::from_apple("HKQuantityTypeIdentifierInsulinDelivery"), None);
        assert_eq!(Metric::from_hae("sleep_analysis"), Some(Metric::SleepStage));
        assert_eq!(
            Metric::from_apple("HKCategoryTypeIdentifierSleepAnalysis"),
            Some(Metric::SleepStage)
        );
        assert_eq!(
            Metric::from_apple("HKCategoryTypeIdentifierAppleStandHour"),
            Some(Metric::StandHour)
        );
        assert_eq!(
            Metric::from_apple("HKQuantityTypeIdentifierBloodPressureSystolic"),
            Some(Metric::BloodPressureSystolic)
        );
        assert_eq!(
            Metric::from_apple("HKQuantityTypeIdentifierBloodPressureDiastolic"),
            Some(Metric::BloodPressureDiastolic)
        );
        assert_eq!(Metric::from_hae("step_count"), Some(Metric::Steps));
        assert_eq!(Metric::from_hae("weight_body_mass"), Some(Metric::Weight));
        assert_eq!(Metric::from_hae("dietary_water"), Some(Metric::Water));
        assert_eq!(Metric::from_apple("HKQuantityTypeIdentifierNoSuchThing"), None);
    }

    #[test]
    fn the_ranges_refuse_the_usual_unit_mix_ups_and_keep_the_real_extremes() {
        let inside = |m: Metric, v: f64| (m.info().min..=m.info().max).contains(&v);
        // Pounds filed as kilograms still look like a weight; grams do not.
        assert!(inside(Metric::Weight, 180.0));
        assert!(!inside(Metric::Weight, 1800.0));
        // mmol/L filed as mg/dL is far below any glucose.
        assert!(!inside(Metric::BloodGlucose, 5.5));
        assert!(inside(Metric::BloodGlucose, 40.0));
        assert!(inside(Metric::BloodGlucose, 400.0));
        // A fraction left as a fraction, and Fahrenheit left as Celsius.
        assert!(!inside(Metric::OxygenSaturation, 0.97));
        assert!(inside(Metric::OxygenSaturation, 97.0));
        assert!(!inside(Metric::BodyTemperature, 98.6));
        assert!(inside(Metric::BodyTemperature, 37.0));
        // Both a deviation and the temperature itself pass for the wrist.
        assert!(inside(Metric::WristTemperature, -0.4));
        assert!(inside(Metric::WristTemperature, 35.2));
        // Sleep stage codes are 0..=5.
        assert!(inside(Metric::SleepStage, 5.0) && !inside(Metric::SleepStage, 6.0));
        assert!(!inside(Metric::Steps, -1.0));
    }

    #[test]
    fn the_catalogue_answers_one_entry_per_metric() {
        let entries = catalog();
        assert_eq!(entries.len(), Metric::ALL.len());
        let weight = Metric::Weight.entry();
        assert_eq!((weight.name.as_str(), weight.unit.as_str()), ("Weight", "kg"));
        assert_eq!(weight.group, MetricGroup::Body);
    }
}
