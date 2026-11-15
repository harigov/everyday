//! Food lookup: USDA FoodData Central (the person's api.data.gov key, or
//! `DEMO_KEY`) for generic and branded foods, Open Food Facts for barcodes.
//!
//! Only the search text or the barcode leaves the machine. Answers are
//! cached as nothing: a hit becomes a [`Food`] only when somebody logs it.
//!
//! # Whose words these are
//!
//! A food's name, a brand and a serving's description come from a database
//! anybody can contribute to -- Open Food Facts is edited by its users, and a
//! brand owner writes its own label for FoodData Central. They are a
//! stranger's text that is shown on a screen and, once a food is logged,
//! quoted to the assistant. So every one is made [`clean_text`] on the way in:
//! control and invisible characters (the ones that make a name read as
//! something else) taken out, whitespace made single, and a length cap.
//! Numbers are held to what a food could be: a 100 g portion has no more than
//! 100 g of protein in it, whatever a label says.
//!
//! # Through the shared client
//!
//! Both hosts are fixed here and neither is an address somebody chose, which
//! is the line `crate::http` draws between its shared client and its
//! stranger-proof one. The shared client names the application in its
//! `User-Agent`, which Open Food Facts asks every caller to do. An error
//! never carries the address: the FoodData Central key is in it.

use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::{Service, blocking};
use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::health::{Food, FoodHit, FoodSource, Nutrients, Serving};
use karmayoga_core::id::FoodId;
use karmayoga_core::money::text::is_invisible;
use reqwest::Url;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

const USDA_SEARCH: &str = "https://api.nal.usda.gov/fdc/v1/foods/search";
const OPEN_FOOD_FACTS: &str = "https://world.openfoodfacts.org/api/v2/product";

/// What each service is called in a sentence.
const USDA: &str = "FoodData Central";
const OFF: &str = "Open Food Facts";

/// The key api.data.gov hands out for trying an API, hard rate-limited.
const DEMO_KEY: &str = "DEMO_KEY";

/// The most of an answer that is read. Twenty branded foods with every
/// nutrient they list are a few hundred kilobytes.
const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;

/// How many foods a search answers, from this vault and from FoodData
/// Central each.
const MAX_HITS: usize = 20;

/// The longest a search is, in characters: more is not a food's name.
const MAX_QUERY_CHARS: usize = 200;
const MAX_NAME_CHARS: usize = 200;
const MAX_BRAND_CHARS: usize = 100;
const MAX_SERVING_CHARS: usize = 60;

/// FoodData Central's nutrient ids.
const ENERGY: u64 = 1008;
const ENERGY_ATWATER_GENERAL: u64 = 2047;
const ENERGY_ATWATER_SPECIFIC: u64 = 2048;
const PROTEIN: u64 = 1003;
const FAT: u64 = 1004;
const CARBS: u64 = 1005;
const FIBER: u64 = 1079;
const SUGARS: u64 = 2000;
const SUGARS_NLEA: u64 = 1063;
const SAT_FAT: u64 = 1258;
const SODIUM: u64 = 1093;

/// This vault's own foods first, then FoodData Central's.
///
/// An empty search is this vault's foods alone, most recently used first --
/// there is nothing to ask a database. When FoodData Central cannot be
/// reached and some of this vault's own foods match, those are answered
/// rather than an error: what somebody eats every day must stay loggable
/// with the network down.
pub async fn search(
    svc: &Arc<Service>,
    vault: Arc<Vault>,
    query: &str,
) -> CommandResult<Vec<FoodHit>> {
    let query = clean_query(query);
    let own = {
        let (vault, query) = (vault.clone(), query.clone());
        blocking(move || Ok(vault.list_foods(&query)?)).await?
    };
    let own: Vec<Food> = own.into_iter().take(MAX_HITS).collect();
    if query.is_empty() {
        return Ok(merge(own, Vec::new()));
    }
    let key = fdc_key(&vault).await?;
    match usda_search(&query, "Foundation,SR Legacy,Branded", MAX_HITS, &key, svc.now()).await {
        Ok(foods) => Ok(merge(own, foods)),
        Err(e) if !own.is_empty() => {
            tracing::warn!(code = %e.code, "health: food search: FoodData Central did not answer");
            Ok(merge(own, Vec::new()))
        }
        Err(e) => Err(e),
    }
}

/// A barcode (EAN-13, UPC-A): this vault's own food with it, else Open Food
/// Facts', else FoodData Central's branded foods.
///
/// `None` is "nobody here knows that code", and is only said when both
/// services answered. One that could not be reached, with the other finding
/// nothing, is its error: a person told "not found" would add the food by
/// hand when it was only the network.
pub async fn barcode(
    svc: &Arc<Service>,
    vault: Arc<Vault>,
    code: &str,
) -> CommandResult<Option<FoodHit>> {
    let code = clean_barcode(code)?;
    // Asked for without its leading noughts, so that a food saved under the
    // UPC-A is found by the EAN-13 of the same product, and the other way.
    let significant = significant_digits(&code);
    let own = {
        let (vault, significant) = (vault.clone(), significant.clone());
        blocking(move || Ok(vault.list_foods(&significant)?)).await?
    };
    if let Some(food) = own.into_iter().find(|f| significant_digits(&f.barcode) == significant) {
        return Ok(Some(FoodHit { food, origin: "mine".into() }));
    }

    let now = svc.now();
    let off = match off_lookup(&code, now).await {
        Ok(Some(food)) => return Ok(Some(FoodHit { food, origin: "openFoodFacts".into() })),
        other => other,
    };
    let key = fdc_key(&vault).await?;
    let usda = usda_by_barcode(&code, &key, now).await;
    match (off, usda) {
        (_, Ok(Some(food))) => Ok(Some(FoodHit { food, origin: "usda".into() })),
        (Ok(_), Ok(None)) => Ok(None),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

// ---- the keys ----------------------------------------------------------------

/// The key requests to FoodData Central are made with: the person's own, or
/// the shared demo key when they have set none.
async fn fdc_key(vault: &Arc<Vault>) -> CommandResult<String> {
    let vault = vault.clone();
    let keys = blocking(move || Ok(vault.health_keys_access()?)).await?;
    let key = keys.fdc_key.trim();
    Ok(if key.is_empty() { DEMO_KEY.to_string() } else { key.to_string() })
}

// ---- the requests ------------------------------------------------------------

/// Search FoodData Central for `query` among `data_types`.
async fn usda_search(
    query: &str,
    data_types: &str,
    page_size: usize,
    key: &str,
    now: Timestamp,
) -> CommandResult<Vec<Food>> {
    let size = page_size.to_string();
    let url = Url::parse_with_params(
        USDA_SEARCH,
        [("query", query), ("pageSize", size.as_str()), ("dataType", data_types), ("api_key", key)],
    )
    .map_err(|e| CommandError::new(codes::INTERNAL, format!("could not build a search: {e}")))?;
    let body = get_json(url, USDA, key == DEMO_KEY).await?;
    Ok(body.map(|b| usda_foods(&b, now)).unwrap_or_default())
}

/// The branded food FoodData Central has for `code`, if any.
async fn usda_by_barcode(code: &str, key: &str, now: Timestamp) -> CommandResult<Option<Food>> {
    // FoodData Central files a UPC-A under its twelve digits; an EAN-13 of
    // the same product has a nought in front.
    let digits = significant_digits(code);
    let query = if digits.len() <= 12 { format!("{digits:0>12}") } else { code.to_string() };
    let found = usda_search(&query, "Branded", 5, key, now).await?;
    Ok(found.into_iter().find(|f| significant_digits(&f.barcode) == digits))
}

/// Open Food Facts' product for `code`, if it has one with nutrition facts.
async fn off_lookup(code: &str, now: Timestamp) -> CommandResult<Option<Food>> {
    let fields = "product_name,brands,nutriments,serving_size,serving_quantity";
    let address = format!("{OPEN_FOOD_FACTS}/{code}.json");
    let url = Url::parse_with_params(&address, [("fields", fields)]).map_err(|e| {
        CommandError::new(codes::INTERNAL, format!("could not build a lookup: {e}"))
    })?;
    let body = get_json(url, OFF, false).await?;
    Ok(body.and_then(|b| off_food(&b, code, now)))
}

/// GET `url` and answer its JSON; `None` for a 404, which both services use
/// for "no such thing" (Open Food Facts with a JSON body that says so).
async fn get_json(url: Url, provider: &str, demo_key: bool) -> CommandResult<Option<Value>> {
    let response = http::client()?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| describe(&e, provider))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(CommandError::new(codes::NETWORK, explain_status(provider, status, demo_key)));
    }
    let body = http::read_capped(response, MAX_ANSWER_BYTES, || {
        format!("{provider} sent more than this app will read")
    })
    .await?;
    serde_json::from_slice(&body).map(Some).map_err(|_| {
        CommandError::new(
            codes::UNREADABLE,
            format!("{provider} answered with something this app could not read."),
        )
    })
}

fn explain_status(provider: &str, status: reqwest::StatusCode, demo_key: bool) -> String {
    match status.as_u16() {
        400 => format!("{provider} could not answer that search."),
        401 | 403 if provider == USDA && !demo_key => format!(
            "{provider} refused the key. Check it in Settings \u{2192} Health, or remove it to \
             use the shared demo key."
        ),
        401 | 403 => format!("{provider} refused the request."),
        429 if provider == USDA && demo_key => format!(
            "{provider} limits the shared demo key to a few searches an hour. Add your own free \
             api.data.gov key in Settings \u{2192} Health, or try again later."
        ),
        429 => format!("{provider} asked us to slow down. Try again in a minute."),
        500..=599 => format!("{provider} is having trouble ({status})."),
        _ => format!("{provider} answered {status}."),
    }
}

/// A transport failure in words, without the address: it carries the search,
/// and for FoodData Central the key.
fn describe(e: &reqwest::Error, provider: &str) -> CommandError {
    let message = if e.is_timeout() {
        format!("{provider} did not answer in time.")
    } else if e.is_connect() {
        format!("could not reach {provider}. Check the network.")
    } else {
        format!("looking up food failed: {}", http::strip_url(&e.to_string()))
    };
    CommandError::new(http::transport_code(e), message)
}

// ---- what comes back ---------------------------------------------------------

/// This vault's foods, then FoodData Central's -- leaving out any of those
/// the person has already saved (their copy is the one in `own`).
fn merge(own: Vec<Food>, usda: Vec<Food>) -> Vec<FoodHit> {
    let saved: Vec<u64> = own
        .iter()
        .filter_map(|f| match f.source {
            FoodSource::Usda { fdc_id } => Some(fdc_id),
            _ => None,
        })
        .collect();
    let mut hits: Vec<FoodHit> =
        own.into_iter().map(|food| FoodHit { food, origin: "mine".into() }).collect();
    hits.extend(
        usda.into_iter()
            .filter(|f| !matches!(f.source, FoodSource::Usda { fdc_id } if saved.contains(&fdc_id)))
            .map(|food| FoodHit { food, origin: "usda".into() }),
    );
    hits
}

/// The foods in a FoodData Central search answer.
fn usda_foods(body: &Value, now: Timestamp) -> Vec<Food> {
    body["foods"]
        .as_array()
        .map(|items| items.iter().filter_map(|item| usda_food(item, now)).collect())
        .unwrap_or_default()
}

/// One FoodData Central food: per 100 g, with the serving it is sold in when
/// it names one. `None` for one with no id, no name, or no nutrition facts
/// at all -- it is of no use in a log.
fn usda_food(item: &Value, now: Timestamp) -> Option<Food> {
    let fdc_id = item["fdcId"].as_u64()?;
    let name = tame_caps(&clean_text(item["description"].as_str()?, MAX_NAME_CHARS));
    if name.is_empty() {
        return None;
    }
    let brand = ["brandName", "brandOwner"]
        .iter()
        .find_map(|k| {
            let text = clean_text(item[*k].as_str()?, MAX_BRAND_CHARS);
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_default();
    let listed = item["foodNutrients"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let per_100g = nutrients_from(&usda_nutrients(listed))?;
    Some(Food {
        id: FoodId::new(),
        name,
        brand: tame_caps(&brand),
        barcode: item["gtinUpc"].as_str().map(clean_barcode_text).unwrap_or_default(),
        source: FoodSource::Usda { fdc_id },
        per_100g,
        servings: usda_serving(item).into_iter().collect(),
        favorite: false,
        last_used: None,
        use_count: 0,
        created_at: now,
        updated_at: now,
    })
}

/// A food's nutrients by FoodData Central id. Entries come as
/// `{nutrientId, nutrientNumber, value}` in a search and
/// `{nutrient: {id, number}, amount}` in a food's own record; the legacy
/// number is accepted where the id is not given. The first of an id wins.
fn usda_nutrients(entries: &[Value]) -> BTreeMap<u64, f64> {
    let mut found = BTreeMap::new();
    for entry in entries {
        let id = entry["nutrientId"].as_u64().or_else(|| entry["nutrient"]["id"].as_u64()).or_else(
            || {
                let number = entry["nutrientNumber"]
                    .as_str()
                    .or_else(|| entry["nutrient"]["number"].as_str())?;
                legacy_to_id(number)
            },
        );
        let value = entry["value"].as_f64().or_else(|| entry["amount"].as_f64());
        if let (Some(id), Some(value)) = (id, value) {
            found.entry(id).or_insert(value);
        }
    }
    found
}

/// The ids behind the numbers FoodData Central used before it had ids.
fn legacy_to_id(number: &str) -> Option<u64> {
    Some(match number.trim() {
        "208" => ENERGY,
        "203" => PROTEIN,
        "204" => FAT,
        "205" => CARBS,
        "291" => FIBER,
        "269" => SUGARS,
        "606" => SAT_FAT,
        "307" => SODIUM,
        _ => return None,
    })
}

/// [`Nutrients`] per 100 g out of what a database listed, by FoodData
/// Central id. `None` when it listed none of energy, protein, fat and
/// carbohydrate. Energy listed nowhere is worked out from the three that are
/// with the general Atwater factors (4, 4 and 9 kcal a gram) -- the same
/// figure FoodData Central itself gives a food with no measured one (ids
/// 2047 and 2048, which are what is used first).
fn nutrients_from(found: &BTreeMap<u64, f64>) -> Option<Nutrients> {
    let get = |id: u64, max: f64| found.get(&id).copied().and_then(|v| plausible(v, max));
    let grams = |id: u64| get(id, MAX_GRAMS);
    let protein = grams(PROTEIN);
    let fat = grams(FAT);
    let carbs = grams(CARBS);
    let energy = [ENERGY, ENERGY_ATWATER_GENERAL, ENERGY_ATWATER_SPECIFIC]
        .into_iter()
        .find_map(|id| get(id, MAX_KCAL));
    if energy.is_none() && protein.is_none() && fat.is_none() && carbs.is_none() {
        return None;
    }
    let kcal = energy.unwrap_or_else(|| {
        4.0 * protein.unwrap_or(0.0) + 4.0 * carbs.unwrap_or(0.0) + 9.0 * fat.unwrap_or(0.0)
    });
    Some(Nutrients {
        kcal: round(kcal),
        protein_g: round(protein.unwrap_or(0.0)),
        carbs_g: round(carbs.unwrap_or(0.0)),
        fat_g: round(fat.unwrap_or(0.0)),
        fiber_g: grams(FIBER).map(round),
        sugar_g: grams(SUGARS).or_else(|| grams(SUGARS_NLEA)).map(round),
        sat_fat_g: grams(SAT_FAT).map(round),
        sodium_mg: get(SODIUM, MAX_SODIUM_MG).map(round),
        extra: BTreeMap::new(),
    })
}

/// A portion of 100 g holds at most 100 g of anything, and fat has the most
/// energy of anything there is. A value past these is a typo in somebody's
/// label, and is left out rather than logged.
const MAX_GRAMS: f64 = 100.0;
const MAX_KCAL: f64 = 900.0;
/// Pure salt is 39 g of sodium in 100 g.
const MAX_SODIUM_MG: f64 = 40_000.0;

/// `value` if it could be an amount of something: finite, not negative,
/// not past `max`.
fn plausible(value: f64, max: f64) -> Option<f64> {
    (value.is_finite() && (0.0..=max).contains(&value)).then_some(value)
}

/// To two decimal places: databases carry float noise, and nobody weighs
/// a hundredth of a gram.
fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The serving a branded food is sold in, in grams. Millilitres count as
/// grams here, as a food per 100 g counts for a drink.
fn usda_serving(item: &Value) -> Option<Serving> {
    let size = plausible(item["servingSize"].as_f64()?, 5000.0).filter(|s| *s > 0.0)?;
    let unit = match item["servingSizeUnit"].as_str()?.trim().to_ascii_lowercase().as_str() {
        "g" | "grm" => "g",
        "ml" | "mlt" => "ml",
        _ => return None,
    };
    let household = item["householdServingFullText"].as_str().unwrap_or_default();
    let household = tame_caps(&clean_text(household, MAX_SERVING_CHARS));
    let name = if household.is_empty() { format!("{size} {unit}") } else { household };
    Some(Serving { name, grams: round(size) })
}

/// The food in an Open Food Facts product answer. `None` for a product it
/// does not know, and for one it knows by name only: with no nutrition facts
/// there is nothing to log.
fn off_food(body: &Value, code: &str, now: Timestamp) -> Option<Food> {
    if body["status"].as_i64() == Some(0) {
        return None;
    }
    let product = body["product"].as_object().map(|_| &body["product"])?;
    let nutriments = &product["nutriments"];
    // Numbers come as numbers or as strings, whichever the contributor's
    // tool wrote.
    let number = |key: &str| {
        nutriments[key]
            .as_f64()
            .or_else(|| nutriments[key].as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    };
    let kcal = number("energy-kcal_100g").or_else(|| number("energy_100g").map(|kj| kj / 4.184));
    let sodium_mg = number("sodium_100g")
        .or_else(|| number("salt_100g").map(|salt| salt / 2.5))
        .map(|g| g * 1000.0);
    let mut found = BTreeMap::new();
    for (id, value) in [
        (ENERGY, kcal),
        (PROTEIN, number("proteins_100g")),
        (CARBS, number("carbohydrates_100g")),
        (FAT, number("fat_100g")),
        (FIBER, number("fiber_100g")),
        (SUGARS, number("sugars_100g")),
        (SAT_FAT, number("saturated-fat_100g")),
        (SODIUM, sodium_mg),
    ] {
        if let Some(value) = value {
            found.insert(id, value);
        }
    }
    let per_100g = nutrients_from(&found)?;

    let name = clean_text(product["product_name"].as_str().unwrap_or(""), MAX_NAME_CHARS);
    let brand = product["brands"]
        .as_str()
        .and_then(|b| b.split(',').next())
        .map(|b| clean_text(b, MAX_BRAND_CHARS))
        .unwrap_or_default();
    let grams = product["serving_quantity"]
        .as_f64()
        .or_else(|| product["serving_quantity"].as_str().and_then(|s| s.trim().parse().ok()))
        .and_then(|g| plausible(g, 5000.0))
        .filter(|g| *g > 0.0);
    let serving_name =
        clean_text(product["serving_size"].as_str().unwrap_or_default(), MAX_SERVING_CHARS);
    let servings = grams
        .map(|grams| Serving {
            name: if serving_name.is_empty() { format!("{grams} g") } else { serving_name },
            grams: round(grams),
        })
        .into_iter()
        .collect();
    Some(Food {
        id: FoodId::new(),
        name: if name.is_empty() { format!("Product {code}") } else { name },
        brand,
        barcode: clean_barcode_text(code),
        source: FoodSource::OpenFoodFacts { code: clean_barcode_text(code) },
        per_100g,
        servings,
        favorite: false,
        last_used: None,
        use_count: 0,
        created_at: now,
        updated_at: now,
    })
}

// ---- a stranger's text -------------------------------------------------------

/// `text` as one tidy line of at most `max` characters: control characters
/// and the invisible ones that disguise a name (see
/// [`karmayoga_core::money::text::is_invisible`]) taken out, runs of
/// whitespace made one space, the ends trimmed.
pub(super) fn clean_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() {
            gap = !out.is_empty();
        } else if !c.is_control() && !is_invisible(c) {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(c);
        }
    }
    let kept: String = out.chars().take(max).collect();
    kept.trim_end().to_string()
}

/// A label printed in capitals, as branded foods in FoodData Central are,
/// as words with capitals only at their starts. Text with any lower-case
/// letter in it is as its writer meant, and so is a short one: "KFC" and
/// "IBM" are brands, not shouting.
fn tame_caps(text: &str) -> String {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 4 || text.chars().any(char::is_lowercase) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut word_start = true;
    for c in text.chars() {
        if c.is_alphabetic() {
            if word_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            word_start = false;
        } else {
            word_start = !(c == '\'' || c == '\u{2019}');
            out.push(c);
        }
    }
    out
}

/// What somebody typed as a search: one line, at most [`MAX_QUERY_CHARS`].
fn clean_query(query: &str) -> String {
    clean_text(query, MAX_QUERY_CHARS)
}

/// A scanned code: digits only (a scanner or a person may leave spaces or
/// dashes in), of a length a barcode has. A refusal says what was wrong.
fn clean_barcode(code: &str) -> CommandResult<String> {
    let digits: String = code.chars().filter(char::is_ascii_digit).collect();
    if (8..=14).contains(&digits.len()) && digits.bytes().any(|d| d != b'0') {
        Ok(digits)
    } else {
        Err(CommandError::new(
            codes::INVALID,
            "That is not a barcode: they are 8 to 14 digits (EAN-8, UPC-A, EAN-13).",
        ))
    }
}

/// The digits in `text`, at most fourteen.
fn clean_barcode_text(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).take(14).collect()
}

/// A barcode without its leading noughts, which is what a UPC-A and the
/// EAN-13 of the same product have in common.
fn significant_digits(code: &str) -> String {
    clean_barcode_text(code).trim_start_matches('0').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> Timestamp {
        "2026-11-15T12:00:00Z".parse().unwrap()
    }

    /// What FoodData Central answers for a branded food: per 100 g, in a
    /// search's `{nutrientId, value}` shape.
    fn branded() -> Value {
        json!({
            "fdcId": 1105904,
            "description": "WHOLE MILK YOGURT",
            "dataType": "Branded",
            "brandOwner": "ACME DAIRY CO",
            "gtinUpc": "041220576800",
            "servingSize": 170.0,
            "servingSizeUnit": "g",
            "householdServingFullText": "3/4 cup",
            "foodNutrients": [
                { "nutrientId": 1008, "nutrientNumber": "208", "unitName": "KCAL", "value": 61.0 },
                { "nutrientId": 1003, "nutrientNumber": "203", "value": 3.47 },
                { "nutrientId": 1004, "nutrientNumber": "204", "value": 3.25 },
                { "nutrientId": 1005, "nutrientNumber": "205", "value": 4.66 },
                { "nutrientId": 1079, "nutrientNumber": "291", "value": 0.0 },
                { "nutrientId": 2000, "nutrientNumber": "269", "value": 4.66 },
                { "nutrientId": 1258, "nutrientNumber": "606", "value": 2.1 },
                { "nutrientId": 1093, "nutrientNumber": "307", "unitName": "MG", "value": 46.0 },
                { "nutrientId": 1062, "nutrientNumber": "268", "unitName": "kJ", "value": 255.0 },
            ],
        })
    }

    #[test]
    fn a_branded_food_is_per_100_g_with_its_serving() {
        let food = usda_food(&branded(), now()).unwrap();
        assert_eq!(food.name, "Whole Milk Yogurt", "a label in capitals is made words");
        assert_eq!(food.brand, "Acme Dairy Co");
        assert_eq!(food.barcode, "041220576800");
        assert_eq!(food.source, FoodSource::Usda { fdc_id: 1105904 });
        let n = &food.per_100g;
        assert_eq!((n.kcal, n.protein_g, n.fat_g, n.carbs_g), (61.0, 3.47, 3.25, 4.66));
        assert_eq!(n.fiber_g, Some(0.0));
        assert_eq!(n.sugar_g, Some(4.66));
        assert_eq!(n.sat_fat_g, Some(2.1));
        assert_eq!(n.sodium_mg, Some(46.0));
        assert_eq!(food.servings, vec![Serving { name: "3/4 cup".into(), grams: 170.0 }]);
        assert!(!food.favorite && food.use_count == 0 && food.last_used.is_none());
    }

    #[test]
    fn a_serving_without_a_household_name_is_named_by_its_size() {
        let mut item = branded();
        item["householdServingFullText"] = json!("");
        item["servingSize"] = json!(240.0);
        item["servingSizeUnit"] = json!("MLT");
        let food = usda_food(&item, now()).unwrap();
        assert_eq!(food.servings, vec![Serving { name: "240 ml".into(), grams: 240.0 }]);
        // A unit that is not a weight or a volume is no serving at all.
        item["servingSizeUnit"] = json!("IU");
        assert!(usda_food(&item, now()).unwrap().servings.is_empty());
    }

    #[test]
    fn a_foundation_food_has_no_serving_and_takes_energy_from_atwater() {
        let item = json!({
            "fdcId": 748967,
            "description": "Egg, whole, raw, fresh",
            "foodNutrients": [
                { "nutrientId": 2047, "nutrientNumber": "957", "value": 143.0 },
                { "nutrientId": 1003, "value": 12.4 },
                { "nutrientId": 1004, "value": 9.96 },
                { "nutrientId": 1005, "value": 0.96 },
            ],
        });
        let food = usda_food(&item, now()).unwrap();
        assert_eq!(food.name, "Egg, whole, raw, fresh", "mixed case is left as written");
        assert_eq!(food.per_100g.kcal, 143.0);
        assert!(food.servings.is_empty() && food.brand.is_empty() && food.barcode.is_empty());
    }

    #[test]
    fn nutrients_are_found_by_legacy_number_and_in_a_food_records_own_shape() {
        let item = json!({
            "fdcId": 1,
            "description": "Oats",
            "foodNutrients": [
                { "nutrientNumber": "208", "value": 389.0 },
                { "nutrient": { "id": 1003, "number": "203" }, "amount": 16.9 },
                { "nutrientNumber": "204", "value": 6.9 },
                { "nutrientNumber": "205", "value": 66.3 },
            ],
        });
        let n = usda_food(&item, now()).unwrap().per_100g;
        assert_eq!((n.kcal, n.protein_g, n.fat_g, n.carbs_g), (389.0, 16.9, 6.9, 66.3));
    }

    #[test]
    fn energy_nobody_listed_is_worked_out_and_impossible_values_are_dropped() {
        let item = json!({
            "fdcId": 2,
            "description": "Odd",
            "foodNutrients": [
                { "nutrientId": 1003, "value": 10.0 },
                { "nutrientId": 1004, "value": 10.0 },
                { "nutrientId": 1005, "value": 20.0 },
                // More than a portion weighs, negative, and not a number.
                { "nutrientId": 1079, "value": 140.0 },
                { "nutrientId": 2000, "value": -3.0 },
                { "nutrientId": 1258, "value": null },
            ],
        });
        let n = usda_food(&item, now()).unwrap().per_100g;
        assert_eq!(n.kcal, 4.0 * 10.0 + 4.0 * 20.0 + 9.0 * 10.0);
        assert_eq!((n.fiber_g, n.sugar_g, n.sat_fat_g, n.sodium_mg), (None, None, None, None));
    }

    #[test]
    fn a_food_with_no_nutrition_facts_or_no_name_is_not_an_answer() {
        let nameless = json!({
            "fdcId": 3,
            "description": " \u{200b}\n ",
            "foodNutrients": [{ "nutrientId": 1008, "value": 1.0 }],
        });
        let bare = json!({ "fdcId": 4, "description": "Water", "foodNutrients": [] });
        let idless = json!({
            "description": "Salt",
            "foodNutrients": [{ "nutrientId": 1008, "value": 1.0 }],
        });
        for item in [nameless, bare, idless] {
            assert!(usda_food(&item, now()).is_none(), "{item}");
        }
        let body = json!({ "foods": [branded(), { "fdcId": 4, "description": "Water" }] });
        assert_eq!(usda_foods(&body, now()).len(), 1);
        assert!(usda_foods(&json!({}), now()).is_empty());
    }

    #[test]
    fn a_strangers_words_lose_what_disguises_them_and_are_capped() {
        assert_eq!(clean_text("  Ice\u{202e}cream \n\t bar\u{0}  ", 100), "Icecream bar");
        assert_eq!(clean_text("\u{feff}\u{200b}", 10), "");
        assert_eq!(clean_text("abcdefghij", 4), "abcd");
        assert_eq!(clean_text("ab cd", 3), "ab", "a cut never leaves a space on the end");
        let long = "x".repeat(500);
        assert_eq!(clean_text(&long, MAX_NAME_CHARS).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn capitals_are_made_words_only_when_the_whole_label_shouts() {
        assert_eq!(tame_caps("WHOLE MILK"), "Whole Milk");
        assert_eq!(tame_caps("MCDONALD'S"), "Mcdonald's");
        assert_eq!(tame_caps("Greek yogurt"), "Greek yogurt");
        assert_eq!(tame_caps("KFC"), "KFC");
        assert_eq!(tame_caps("A"), "A");
    }

    #[test]
    fn a_barcode_is_digits_of_a_length_barcodes_have() {
        assert_eq!(clean_barcode(" 0 41220-57680 0").unwrap(), "041220576800");
        for bad in ["", "1234567", "123456789012345", "abcdefgh", "00000000"] {
            assert_eq!(clean_barcode(bad).unwrap_err().code, codes::INVALID, "{bad:?}");
        }
        assert_eq!(significant_digits("0041220576800"), significant_digits("041220576800"));
    }

    fn off_answer() -> Value {
        json!({
            "code": "3017620422003",
            "status": 1,
            "product": {
                "product_name": "Hazelnut spread",
                "brands": "Ferrero, Nutella",
                "serving_size": "15 g",
                "serving_quantity": "15",
                "nutriments": {
                    "energy-kcal_100g": 539,
                    "proteins_100g": 6.3,
                    "carbohydrates_100g": "57.5",
                    "fat_100g": 30.9,
                    "fiber_100g": 0,
                    "sugars_100g": 56.3,
                    "saturated-fat_100g": 10.6,
                    "sodium_100g": 0.0428,
                },
            },
        })
    }

    #[test]
    fn an_open_food_facts_product_is_per_100_g_with_sodium_in_milligrams() {
        let food = off_food(&off_answer(), "3017620422003", now()).unwrap();
        assert_eq!(food.name, "Hazelnut spread");
        assert_eq!(food.brand, "Ferrero", "the first of its brands");
        assert_eq!(food.barcode, "3017620422003");
        assert_eq!(food.source, FoodSource::OpenFoodFacts { code: "3017620422003".into() });
        let n = &food.per_100g;
        assert_eq!((n.kcal, n.protein_g, n.carbs_g, n.fat_g), (539.0, 6.3, 57.5, 30.9));
        assert_eq!((n.fiber_g, n.sugar_g, n.sat_fat_g), (Some(0.0), Some(56.3), Some(10.6)));
        assert_eq!(n.sodium_mg, Some(42.8));
        assert_eq!(food.servings, vec![Serving { name: "15 g".into(), grams: 15.0 }]);
    }

    #[test]
    fn open_food_facts_energy_in_kilojoules_and_salt_are_converted() {
        let body = json!({
            "status": 1,
            "product": {
                "product_name": "Crackers",
                "nutriments": {
                    "energy_100g": 1884, "proteins_100g": 9, "carbohydrates_100g": 70,
                    "fat_100g": 10, "salt_100g": 2.5,
                },
            },
        });
        let food = off_food(&body, "12345678", now()).unwrap();
        assert_eq!(food.per_100g.kcal, 450.29);
        assert_eq!(food.per_100g.sodium_mg, Some(1000.0));
        assert!(food.servings.is_empty() && food.brand.is_empty());
    }

    #[test]
    fn an_open_food_facts_product_with_nothing_to_log_is_not_an_answer() {
        // Not found, with the status Open Food Facts gives it.
        let missing = json!({ "status": 0, "status_verbose": "product not found" });
        assert!(off_food(&missing, "1", now()).is_none());
        // Known, but with no nutrition facts.
        let bare =
            json!({ "status": 1, "product": { "product_name": "Mystery", "nutriments": {} } });
        assert!(off_food(&bare, "12345678", now()).is_none());
        // No name: the code stands in.
        let unnamed =
            json!({ "status": 1, "product": { "nutriments": { "energy-kcal_100g": 10 } } });
        assert_eq!(off_food(&unnamed, "12345678", now()).unwrap().name, "Product 12345678");
    }

    fn food(name: &str, source: FoodSource) -> Food {
        Food {
            id: FoodId::new(),
            name: name.into(),
            brand: String::new(),
            barcode: String::new(),
            source,
            per_100g: Nutrients::default(),
            servings: Vec::new(),
            favorite: false,
            last_used: None,
            use_count: 0,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn the_vaults_own_foods_come_first_and_are_not_repeated_by_the_database() {
        let own = vec![
            food("My oats", FoodSource::Own),
            food("Whole milk", FoodSource::Usda { fdc_id: 7 }),
        ];
        let usda = vec![
            food("Whole milk", FoodSource::Usda { fdc_id: 7 }),
            food("Skim milk", FoodSource::Usda { fdc_id: 8 }),
        ];
        let hits = merge(own, usda);
        let listed: Vec<(&str, &str)> =
            hits.iter().map(|h| (h.food.name.as_str(), h.origin.as_str())).collect();
        assert_eq!(listed, [("My oats", "mine"), ("Whole milk", "mine"), ("Skim milk", "usda")]);
    }

    #[test]
    fn a_search_is_one_short_line() {
        assert_eq!(clean_query("  greek \n yogurt  "), "greek yogurt");
        assert_eq!(clean_query(""), "");
        assert_eq!(clean_query(&"a".repeat(1000)).chars().count(), MAX_QUERY_CHARS);
    }

    #[test]
    fn a_refusal_says_what_a_person_can_do_about_it() {
        let busy = explain_status(USDA, reqwest::StatusCode::TOO_MANY_REQUESTS, true);
        assert!(busy.contains("demo key") && busy.contains("Settings"), "{busy}");
        let own_key = explain_status(USDA, reqwest::StatusCode::TOO_MANY_REQUESTS, false);
        assert!(own_key.contains("slow down"), "{own_key}");
        let refused = explain_status(USDA, reqwest::StatusCode::FORBIDDEN, false);
        assert!(refused.contains("key"), "{refused}");
        assert!(explain_status(OFF, reqwest::StatusCode::BAD_GATEWAY, false).contains("trouble"));
    }

    #[test]
    fn the_search_address_carries_what_was_typed_and_nothing_else() {
        let size = "20";
        let url = Url::parse_with_params(
            USDA_SEARCH,
            [
                ("query", "mac & cheese"),
                ("pageSize", size),
                ("dataType", "Foundation,SR Legacy,Branded"),
                ("api_key", "DEMO_KEY"),
            ],
        )
        .unwrap();
        assert_eq!(url.host_str(), Some("api.nal.usda.gov"));
        let pairs: Vec<(String, String)> =
            url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(pairs[0], ("query".to_string(), "mac & cheese".to_string()));
        assert_eq!(pairs[2].1, "Foundation,SR Legacy,Branded");
    }
}
