//! Nutrition arithmetic: a food at an amount, a meal's total, a day's.
//!
//! Foods are kept per 100 g (or 100 ml for a drink, which for this purpose
//! weighs what it measures). A meal item records its own nutrients, worked
//! out when it was logged, so correcting a food later does not rewrite what
//! was eaten.

use super::metric::Metric;
use super::{Food, Meal, MealItem, Nutrients, NutritionDay};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::collections::BTreeMap;

/// `per_100g` scaled to `grams`.
pub fn scale(per_100g: &Nutrients, grams: f64) -> Nutrients {
    let factor = grams / 100.0;
    let by = |value: f64| value * factor;
    Nutrients {
        kcal: by(per_100g.kcal),
        protein_g: by(per_100g.protein_g),
        carbs_g: by(per_100g.carbs_g),
        fat_g: by(per_100g.fat_g),
        fiber_g: per_100g.fiber_g.map(by),
        sugar_g: per_100g.sugar_g.map(by),
        sat_fat_g: per_100g.sat_fat_g.map(by),
        sodium_mg: per_100g.sodium_mg.map(by),
        extra: per_100g.extra.iter().map(|(name, value)| (name.clone(), by(*value))).collect(),
    }
}

/// How many grams `amount` of `unit` is for `food`: `g` and `ml` as
/// themselves, a serving by its grams. `None` for a unit it has no serving
/// called, and for an amount that is not a number or is below nothing.
pub fn grams_of(food: &Food, amount: f64, unit: &str) -> Option<f64> {
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    let (grams_each, _) = resolve(food, unit)?;
    Some(amount * grams_each)
}

/// A meal item for `amount` of `unit` of `food`, nutrients worked out now.
pub fn item(food: &Food, amount: f64, unit: &str) -> Option<MealItem> {
    let grams = grams_of(food, amount, unit)?;
    let (_, unit) = resolve(food, unit)?;
    Some(MealItem {
        food_id: Some(food.id),
        name: display_name(food),
        amount,
        unit,
        grams: Some(grams),
        nutrients: scale(&food.per_100g, grams),
        estimated: false,
        doubt: String::new(),
    })
}

/// What one of `unit` weighs for `food`, and the unit spelt the way it is
/// kept: `g` and `ml` lower case, a serving under its own name.
fn resolve(food: &Food, unit: &str) -> Option<(f64, String)> {
    let unit = unit.trim().to_lowercase();
    if unit == "g" || unit == "ml" {
        return Some((1.0, unit));
    }
    food.servings
        .iter()
        .find(|serving| serving.name.trim().to_lowercase() == unit)
        .map(|serving| (serving.grams, serving.name.clone()))
}

/// "Greek yogurt (Fage)": the name, and the brand when there is one.
fn display_name(food: &Food) -> String {
    let brand = food.brand.trim();
    if brand.is_empty() { food.name.clone() } else { format!("{} ({brand})", food.name) }
}

/// The sum. Optional nutrients are summed where any item has them.
pub fn sum<'a>(all: impl IntoIterator<Item = &'a Nutrients>) -> Nutrients {
    let mut total = Nutrients::default();
    for each in all {
        total.kcal += each.kcal;
        total.protein_g += each.protein_g;
        total.carbs_g += each.carbs_g;
        total.fat_g += each.fat_g;
        add(&mut total.fiber_g, each.fiber_g);
        add(&mut total.sugar_g, each.sugar_g);
        add(&mut total.sat_fat_g, each.sat_fat_g);
        add(&mut total.sodium_mg, each.sodium_mg);
        for (name, value) in &each.extra {
            *total.extra.entry(name.clone()).or_insert(0.0) += value;
        }
    }
    total
}

/// Add `value` to `total`, which stays unknown until something knows it.
fn add(total: &mut Option<f64>, value: Option<f64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0.0) + value);
    }
}

/// What a meal came to: its items summed.
pub fn meal_total(meal: &Meal) -> Nutrients {
    sum(meal.items.iter().map(|i| &i.nutrients))
}

/// Each local day `from..=to`: what was logged here, and what other apps
/// wrote to the phone (`imported`: each dietary metric's daily values).
/// A day with meals logged here counts only those -- imported dietary
/// samples are for days nothing was logged here, so nothing is counted
/// twice.
pub fn days(
    meals: &[Meal],
    imported: &BTreeMap<Metric, BTreeMap<Date, f64>>,
    from: Date,
    to: Date,
    tz: &TimeZone,
) -> Vec<NutritionDay> {
    let mut by_day: BTreeMap<Date, Vec<&Meal>> = BTreeMap::new();
    for meal in meals {
        by_day.entry(tz.to_datetime(meal.at).date()).or_default().push(meal);
    }

    let mut out = Vec::new();
    let mut date = from;
    while date <= to {
        let ate = by_day.get(&date).map(Vec::as_slice).unwrap_or_default();
        let totals: Vec<Nutrients> = ate.iter().map(|meal| meal_total(meal)).collect();
        let from_phone =
            if ate.is_empty() { imported_on(imported, date) } else { Nutrients::default() };
        out.push(NutritionDay {
            date,
            logged: sum(&totals),
            imported: from_phone,
            meals: u32::try_from(ate.len()).unwrap_or(u32::MAX),
        });
        match date.tomorrow() {
            Ok(next) => date = next,
            Err(_) => break,
        }
    }
    out
}

/// The dietary values other apps wrote for `date`. Energy and the three
/// macronutrients are zero when nothing was written; the rest stay unknown.
fn imported_on(imported: &BTreeMap<Metric, BTreeMap<Date, f64>>, date: Date) -> Nutrients {
    let value = |metric: Metric| imported.get(&metric).and_then(|days| days.get(&date)).copied();
    Nutrients {
        kcal: value(Metric::DietaryEnergy).unwrap_or(0.0),
        protein_g: value(Metric::DietaryProtein).unwrap_or(0.0),
        carbs_g: value(Metric::DietaryCarbs).unwrap_or(0.0),
        fat_g: value(Metric::DietaryFat).unwrap_or(0.0),
        fiber_g: value(Metric::DietaryFiber),
        sugar_g: value(Metric::DietarySugar),
        sat_fat_g: None,
        sodium_mg: value(Metric::DietarySodium),
        extra: BTreeMap::new(),
    }
}

/// The dietary metrics [`days`] reads.
pub const DIETARY_METRICS: &[Metric] = &[
    Metric::DietaryEnergy,
    Metric::DietaryCarbs,
    Metric::DietaryProtein,
    Metric::DietaryFat,
    Metric::DietaryFiber,
    Metric::DietarySugar,
    Metric::DietarySodium,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::schedule::fixtures::meal;
    use crate::health::{FoodSource, Serving};
    use crate::id::FoodId;
    use jiff::Timestamp;
    use jiff::civil::date;

    fn nutrients(kcal: f64, protein_g: f64, carbs_g: f64, fat_g: f64) -> Nutrients {
        Nutrients { kcal, protein_g, carbs_g, fat_g, ..Nutrients::default() }
    }

    /// A food with its servings as (name, grams). The grams are kept to
    /// multiples of 25 so every scaled value is exact in binary.
    fn food(name: &str, brand: &str, per_100g: Nutrients, servings: &[(&str, f64)]) -> Food {
        Food {
            id: FoodId::new(),
            name: name.to_string(),
            brand: brand.to_string(),
            barcode: String::new(),
            source: FoodSource::Own,
            per_100g,
            servings: servings
                .iter()
                .map(|(name, grams)| Serving { name: name.to_string(), grams: *grams })
                .collect(),
            favorite: false,
            last_used: None,
            use_count: 0,
            created_at: Timestamp::UNIX_EPOCH,
            updated_at: Timestamp::UNIX_EPOCH,
        }
    }

    fn yogurt() -> Food {
        food(
            "Greek yogurt",
            "Fage",
            nutrients(60.0, 10.0, 4.0, 0.5),
            &[("1 cup", 250.0), ("1 spoon", 25.0)],
        )
    }

    fn eaten(at: &str, kcal: f64, carbs_g: f64) -> Meal {
        let mut meal = meal(at.parse().unwrap());
        meal.items = vec![MealItem {
            food_id: None,
            name: "Something".to_string(),
            amount: 1.0,
            unit: "g".to_string(),
            grams: None,
            nutrients: nutrients(kcal, 0.0, carbs_g, 0.0),
            estimated: false,
            doubt: String::new(),
        }];
        meal
    }

    #[test]
    fn scaling_multiplies_every_field_including_the_optional_and_the_extra() {
        let mut per_100g = nutrients(200.0, 10.0, 20.0, 8.0);
        per_100g.fiber_g = Some(4.0);
        per_100g.sat_fat_g = Some(2.0);
        per_100g.sodium_mg = Some(100.0);
        per_100g.extra.insert("calcium_mg".to_string(), 120.0);

        let half = scale(&per_100g, 50.0);
        assert_eq!((half.kcal, half.protein_g, half.carbs_g, half.fat_g), (100.0, 5.0, 10.0, 4.0));
        assert_eq!(
            (half.fiber_g, half.sugar_g, half.sat_fat_g, half.sodium_mg),
            (Some(2.0), None, Some(1.0), Some(50.0))
        );
        assert_eq!(half.extra.get("calcium_mg"), Some(&60.0));

        let more = scale(&per_100g, 250.0);
        assert_eq!((more.kcal, more.fiber_g), (500.0, Some(10.0)));
        assert_eq!(more.extra.get("calcium_mg"), Some(&300.0));

        // Nothing eaten is nothing, though what was known stays known.
        let none = scale(&per_100g, 0.0);
        assert_eq!((none.kcal, none.fiber_g, none.sugar_g), (0.0, Some(0.0), None));

        // A hundred grams is the food itself.
        assert_eq!(scale(&per_100g, 100.0), per_100g);
    }

    #[test]
    fn grams_come_from_g_ml_or_a_named_serving() {
        let food = yogurt();
        assert_eq!(grams_of(&food, 150.0, "g"), Some(150.0));
        assert_eq!(grams_of(&food, 150.0, "G"), Some(150.0));
        assert_eq!(grams_of(&food, 200.0, "ml"), Some(200.0));
        assert_eq!(grams_of(&food, 0.5, "1 CUP"), Some(125.0));
        assert_eq!(grams_of(&food, 2.0, "  1 spoon "), Some(50.0));
        assert_eq!(grams_of(&food, 1.0, "bushel"), None);
        assert_eq!(grams_of(&food, 1.0, ""), None);
        // Not an amount of anything.
        assert_eq!(grams_of(&food, -1.0, "g"), None);
        assert_eq!(grams_of(&food, f64::NAN, "g"), None);
        assert_eq!(grams_of(&food, f64::INFINITY, "1 cup"), None);
    }

    #[test]
    fn an_item_names_the_food_keeps_its_own_unit_and_works_out_its_nutrients() {
        let food = yogurt();
        let cup = item(&food, 1.0, "1 CUP").expect("a serving it has");
        assert_eq!(cup.food_id, Some(food.id));
        assert_eq!(cup.name, "Greek yogurt (Fage)");
        assert_eq!((cup.amount, cup.unit.as_str(), cup.grams), (1.0, "1 cup", Some(250.0)));
        assert_eq!((cup.nutrients.kcal, cup.nutrients.protein_g), (150.0, 25.0));
        assert_eq!(cup.nutrients.carbs_g, 10.0);
        assert!(!cup.estimated);

        let grams = item(&food, 50.0, "G").unwrap();
        assert_eq!(
            (grams.unit.as_str(), grams.grams, grams.nutrients.kcal),
            ("g", Some(50.0), 30.0)
        );

        assert!(item(&food, 1.0, "bushel").is_none());
        assert!(item(&food, -2.0, "g").is_none());
    }

    #[test]
    fn a_food_with_no_brand_is_named_plain() {
        let plain = food("Rice", "", nutrients(130.0, 2.0, 28.0, 0.0), &[]);
        assert_eq!(item(&plain, 100.0, "g").unwrap().name, "Rice");
        let blank = food("Rice", "  ", nutrients(130.0, 2.0, 28.0, 0.0), &[]);
        assert_eq!(item(&blank, 100.0, "g").unwrap().name, "Rice");
    }

    #[test]
    fn a_sum_adds_the_optional_nutrients_wherever_any_item_has_them() {
        let mut a = nutrients(100.0, 1.0, 10.0, 2.0);
        a.fiber_g = Some(1.5);
        a.sodium_mg = Some(10.0);
        a.extra.insert("calcium_mg".to_string(), 10.0);
        let mut b = nutrients(50.0, 2.0, 5.0, 1.0);
        b.sugar_g = Some(2.0);
        b.sodium_mg = Some(5.0);
        b.extra.insert("calcium_mg".to_string(), 5.0);
        b.extra.insert("caffeine_mg".to_string(), 1.0);
        let c = nutrients(25.0, 0.0, 0.0, 0.0);

        let total = sum([&a, &b, &c]);
        assert_eq!(
            (total.kcal, total.protein_g, total.carbs_g, total.fat_g),
            (175.0, 3.0, 15.0, 3.0)
        );
        assert_eq!(total.fiber_g, Some(1.5));
        assert_eq!(total.sugar_g, Some(2.0));
        assert_eq!(total.sat_fat_g, None);
        assert_eq!(total.sodium_mg, Some(15.0));
        assert_eq!(total.extra.get("calcium_mg"), Some(&15.0));
        assert_eq!(total.extra.get("caffeine_mg"), Some(&1.0));

        assert_eq!(sum([]), Nutrients::default());
    }

    #[test]
    fn a_meal_is_the_sum_of_its_items() {
        let food = yogurt();
        let mut breakfast = meal(Timestamp::UNIX_EPOCH);
        breakfast.items =
            vec![item(&food, 1.0, "1 cup").unwrap(), item(&food, 2.0, "1 spoon").unwrap()];
        let total = meal_total(&breakfast);
        // 250 g and 50 g of a 60-kcal-per-100-g food.
        assert_eq!(total.kcal, 180.0);
        assert_eq!(total.protein_g, 30.0);
        assert_eq!(meal_total(&meal(Timestamp::UNIX_EPOCH)), Nutrients::default());
    }

    #[test]
    fn a_day_is_its_meals_by_local_date_and_imported_only_when_none_were_logged() {
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let (nov14, nov15, nov16, nov17) =
            (date(2026, 11, 14), date(2026, 11, 15), date(2026, 11, 16), date(2026, 11, 17));
        let meals = [
            // 19:00 on the 15th in Los Angeles, already the 16th in UTC.
            eaten("2026-11-16T03:00:00Z", 600.0, 70.0),
            // Noon on the 16th, and another an hour later.
            eaten("2026-11-16T20:00:00Z", 400.0, 40.0),
            eaten("2026-11-16T21:00:00Z", 100.0, 10.0),
        ];
        let mut imported: BTreeMap<Metric, BTreeMap<Date, f64>> = BTreeMap::new();
        imported.entry(Metric::DietaryEnergy).or_default().extend([
            (nov14, 1800.0),
            (nov15, 999.0),
            (nov17, 2000.0),
        ]);
        imported.entry(Metric::DietaryCarbs).or_default().insert(nov14, 200.0);
        imported.entry(Metric::DietaryProtein).or_default().insert(nov14, 90.0);
        imported.entry(Metric::DietaryFat).or_default().insert(nov14, 60.0);
        imported.entry(Metric::DietaryFiber).or_default().insert(nov14, 25.0);
        imported.entry(Metric::DietarySugar).or_default().insert(nov14, 50.0);
        imported.entry(Metric::DietarySodium).or_default().insert(nov17, 2300.0);
        // Not a dietary metric the day reads.
        imported.entry(Metric::Steps).or_default().insert(nov14, 9000.0);

        let range = days(&meals, &imported, nov14, nov17, &la);
        let dates: Vec<Date> = range.iter().map(|d| d.date).collect();
        assert_eq!(dates, [nov14, nov15, nov16, nov17]);

        // Nothing logged: the phone's numbers, the optional ones unknown unless written.
        let first = &range[0];
        assert_eq!(first.meals, 0);
        assert_eq!(first.logged, Nutrients::default());
        assert_eq!((first.imported.kcal, first.imported.carbs_g), (1800.0, 200.0));
        assert_eq!((first.imported.protein_g, first.imported.fat_g), (90.0, 60.0));
        assert_eq!((first.imported.fiber_g, first.imported.sugar_g), (Some(25.0), Some(50.0)));
        assert_eq!(first.imported.sodium_mg, None);

        // Logged here: the meal alone, though the phone wrote 999 for the day.
        let second = &range[1];
        assert_eq!(second.meals, 1);
        assert_eq!((second.logged.kcal, second.logged.carbs_g), (600.0, 70.0));
        assert_eq!(second.imported, Nutrients::default());

        // Two meals the same local day add up.
        let third = &range[2];
        assert_eq!(third.meals, 2);
        assert_eq!((third.logged.kcal, third.logged.carbs_g), (500.0, 50.0));
        assert_eq!(third.imported, Nutrients::default());

        // Nothing logged, and only some of the phone's values.
        let last = &range[3];
        assert_eq!((last.meals, last.imported.kcal, last.imported.carbs_g), (0, 2000.0, 0.0));
        assert_eq!((last.imported.sodium_mg, last.imported.fiber_g), (Some(2300.0), None));
    }

    #[test]
    fn every_day_in_the_range_is_there_even_when_empty_and_a_backwards_range_is_none() {
        let utc = TimeZone::UTC;
        let empty = BTreeMap::new();
        let week = days(&[], &empty, date(2026, 11, 9), date(2026, 11, 15), &utc);
        assert_eq!(week.len(), 7);
        assert!(week.iter().all(|d| d.meals == 0
            && d.logged == Nutrients::default()
            && d.imported == Nutrients::default()));

        let one = days(&[], &empty, date(2026, 11, 9), date(2026, 11, 9), &utc);
        assert_eq!(one.len(), 1);
        assert!(days(&[], &empty, date(2026, 11, 10), date(2026, 11, 9), &utc).is_empty());
    }

    #[test]
    fn the_dietary_metrics_are_the_ones_a_day_reads() {
        let mut imported: BTreeMap<Metric, BTreeMap<Date, f64>> = BTreeMap::new();
        let day = date(2026, 11, 14);
        for metric in DIETARY_METRICS {
            imported.entry(*metric).or_default().insert(day, 1.0);
        }
        let read = imported_on(&imported, day);
        assert_eq!((read.kcal, read.protein_g, read.carbs_g, read.fat_g), (1.0, 1.0, 1.0, 1.0));
        assert_eq!((read.fiber_g, read.sugar_g, read.sodium_mg), (Some(1.0), Some(1.0), Some(1.0)));
    }
}
