use crate::error::Result;
use crate::websearch::{SearchResult, Source, string_at};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn parse_nominatim(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(places) = root.as_array() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for place in places {
        let display = string_at(place, "display_name");
        if display.is_empty() {
            continue;
        }
        // `display_name` is a full comma-separated address whose first part
        // is the name of the thing. Splitting it is what turns "Som Saa, 43,
        // Commercial Street, Spitalfields, …" into a title and an address.
        let (name, rest) = display.split_once(',').unwrap_or((display.as_str(), ""));
        let mut facts = BTreeMap::new();
        facts.insert("address".to_string(), rest.trim().to_string());
        for (tag, key) in [("cuisine", "cuisine"), ("phone", "phone"), ("website", "url")] {
            let value = place.pointer(&format!("/extratags/{tag}")).and_then(Value::as_str);
            if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                facts.insert(key.to_string(), value.to_string());
            }
        }
        if let Some(country) = place.pointer("/address/country").and_then(Value::as_str) {
            facts.insert("country".to_string(), country.to_string());
        }
        if let Some(kind) = osm_place_type(place) {
            facts.insert("type".to_string(), kind);
        }
        let osm_url = match (place.get("osm_type").and_then(Value::as_str), place.get("osm_id")) {
            (Some(kind), Some(id)) => format!("https://www.openstreetmap.org/{kind}/{id}"),
            _ => String::new(),
        };
        out.push(SearchResult {
            title: name.trim().to_string(),
            subtitle: rest.trim().to_string(),
            summary: display,
            url: osm_url,
            facts,
            source: Source::Nominatim.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// OpenStreetMap's own word for what a place is -- `museum`, `playground`,
/// `theme_park` -- tidied into what a Places shelf's type field holds.
///
/// Only for the categories that describe somewhere a person goes. The rest
/// describe the map instead: a city is `boundary/administrative`, a street
/// address `building/house`, a road `highway/primary`. Filling the field
/// with "Administrative" would be wrong on that card, and the detail panel
/// would then offer it on every other place on the shelf.
fn osm_place_type(place: &Value) -> Option<String> {
    const VISITABLE: &[&str] = &["tourism", "leisure", "historic", "natural", "amenity"];
    // `category` in the `jsonv2` format this asks for, `class` in `json`.
    let category = place.get("category").or_else(|| place.get("class")).and_then(Value::as_str)?;
    let osm_type = place.get("type").and_then(Value::as_str)?.trim();
    // "yes" means "one of these, of no stated sort", which is no type.
    if !VISITABLE.contains(&category) || osm_type.is_empty() || osm_type == "yes" {
        return None;
    }
    let words = osm_type.replace('_', " ");
    let mut chars = words.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominatim_splits_a_display_name_into_a_name_and_an_address() {
        let body = r#"[{
            "display_name":"Som Saa, 43A, Commercial Street, Spitalfields, London, E1 6BD",
            "osm_type":"way","osm_id":123,
            "extratags":{"cuisine":"thai","phone":"020 7324 7790","website":"https://somsaa.example"},
            "address":{"country":"United Kingdom"}
        }]"#;
        let hit = &parse_nominatim(body).unwrap()[0];
        assert_eq!(hit.title, "Som Saa");
        assert_eq!(hit.facts.get("cuisine").map(String::as_str), Some("thai"));
        assert!(hit.facts["address"].starts_with("43A, Commercial Street"));
        assert_eq!(hit.facts.get("country").map(String::as_str), Some("United Kingdom"));
        assert_eq!(hit.url, "https://www.openstreetmap.org/way/123");
    }

    #[test]
    fn nominatim_says_what_sort_of_place_it_is() {
        let body = r#"[
            {"display_name":"Adventure Playground, Mill Road","category":"leisure","type":"playground"},
            {"display_name":"Some Hall, High Street","category":"historic","type":"yes"},
            {"display_name":"Alton Towers, Staffordshire","class":"tourism","type":"theme_park"},
            {"display_name":"Paris, Île-de-France","category":"boundary","type":"administrative"},
            {"display_name":"12, Mill Road, Cambridge","category":"building","type":"house"},
            {"display_name":"Mill Road, Cambridge","category":"highway","type":"primary"},
            {"display_name":"No category, anywhere","type":"museum"}
        ]"#;
        let hits = parse_nominatim(body).unwrap();
        let types: Vec<Option<&str>> =
            hits.iter().map(|hit| hit.facts.get("type").map(String::as_str)).collect();
        // "yes" is "one of these, of no stated sort", which is no type; and a
        // city, a house or a road is a place on the map, not a sort of outing.
        assert_eq!(types, [Some("Playground"), None, Some("Theme park"), None, None, None, None],);
    }
}
