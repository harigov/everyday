use crate::error::Result;
use crate::library::normalize_rating;
use crate::websearch::{
    SearchResult, Source, decode_entities, first_string, http_url, string_at, strip_tags,
    trim_summary, year_from_text,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Pull games out of Steam's store search.
///
/// Thin, as store replies go: a name, an id, a picture the size of a
/// postage stamp and what it costs today. The id is the useful part — the
/// store serves each game's poster at a fixed address built from it, which
/// is how a card gets artwork worth looking at out of a reply that contains
/// a 231-pixel thumbnail.
///
/// # Why the score is dropped
///
/// The reply carries a `metascore`, and it is Metacritic's, not Steam's.
/// [`apply`] labels an [`ExternalRating`] with the source that answered, so
/// keeping it would put "94 on Steam" beside a game — a number attributed to
/// the shop that repeated it rather than the people who gave it. A rating
/// with the wrong name on it is worse than no rating.
pub(crate) fn parse_steam(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(items) = root.get("items").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        let title = string_at(item, "name");
        let Some(id) = item.get("id").and_then(Value::as_i64) else { continue };
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        // What it runs on, which is the only sense in which a store that
        // sells nothing else can answer "platform".
        let platforms: Vec<&str> = [("windows", "Windows"), ("mac", "macOS"), ("linux", "Linux")]
            .iter()
            .filter(|(key, _)| {
                item.pointer(&format!("/platforms/{key}")).and_then(Value::as_bool).unwrap_or(false)
            })
            .map(|(_, name)| *name)
            .collect();
        if !platforms.is_empty() {
            facts.insert("platform".to_string(), platforms.join(", "));
        }
        out.push(SearchResult {
            title,
            url: format!("https://store.steampowered.com/app/{id}/"),
            // The tall library artwork rather than the wide banner: a shelf
            // draws covers, and `header.jpg` in a poster's frame is a letter
            // box with a logo in it.
            image_url: format!(
                "https://cdn.cloudflare.steamstatic.com/steam/apps/{id}/library_600x900.jpg"
            ),
            facts,
            source: Source::Steam.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// Fill a Steam result in from the store's page for that one game.
///
/// The reply is keyed by the app id — `{"1145360":{"success":true,"data":…}}`
/// — and `success` is `false` for a game that is not sold in the store's
/// region, which is a blank rather than an error: the search result is still
/// perfectly good, it just stays as thin as it arrived.
pub(crate) fn merge_steam_detail(result: &mut SearchResult, body: &str) -> Result<()> {
    let root: Value = serde_json::from_str(body)?;
    let data = root
        .as_object()
        .and_then(|by_id| by_id.values().next())
        .filter(|entry| entry.get("success").and_then(Value::as_bool).unwrap_or(false))
        .and_then(|entry| entry.get("data"));
    let Some(data) = data else { return Ok(()) };

    if result.summary.trim().is_empty() {
        let blurb = string_at(data, "short_description");
        result.summary = trim_summary(&decode_entities(&strip_tags(&blurb)));
    }
    if result.year.is_none() {
        // "Sep 17, 2020", "2020", "Q3 2026" — a store's own prose, in a
        // format that has changed before. The year is the only part of it
        // this app has a field for.
        result.year = year_from_text(&string_at(data.get("release_date").unwrap_or(data), "date"));
    }
    for (key, field) in [("developer", "developers"), ("publisher", "publishers")] {
        let who = first_string(data, field);
        if !who.is_empty() {
            result.facts.entry(key.to_string()).or_insert(who);
        }
    }
    // Steam files a game under several genres and calls each one a
    // description; the shelf has one line for them.
    let genres: Vec<String> = data
        .get("genres")
        .and_then(Value::as_array)
        .map(|all| all.iter().map(|g| string_at(g, "description")).collect())
        .unwrap_or_default();
    let genres: Vec<String> = genres.into_iter().filter(|g| !g.is_empty()).collect();
    if !genres.is_empty() {
        result.facts.entry("genre".to_string()).or_insert_with(|| genres.join(", "));
    }
    if result.creator.trim().is_empty() {
        result.creator = first_string(data, "developers");
    }
    // Metacritic's, out of a hundred, and credited to them — see
    // `SearchResult::rating_source`.
    if let Some(score) = data.pointer("/metacritic/score").and_then(Value::as_f64) {
        if let Some(rating) = normalize_rating(score, 100.0) {
            result.rating = Some(rating);
            result.rating_source = "Metacritic".to_string();
            result.rating_url = data
                .pointer("/metacritic/url")
                .and_then(Value::as_str)
                .and_then(|u| http_url(u).ok())
                .unwrap_or_default();
        }
    }
    Ok(())
}

/// The app id inside a store address this file built, and nothing else.
///
/// Deliberately strict about the prefix: the id is about to be interpolated
/// into a request, and the only addresses that may produce one are the ones
/// [`parse_steam`] wrote.
pub(crate) fn steam_app_id(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://store.steampowered.com/app/")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Item;
    use crate::websearch::apply;
    use crate::websearch::tests::kind;

    #[test]
    fn steam_builds_the_poster_from_the_app_id() {
        let body = r#"{"total":2,"items":[
            {"type":"app","name":"Hades","id":1145360,
             "price":{"currency":"USD","final":2499},"metascore":"93",
             "tiny_image":"https://shared.akamai.steamstatic.com/capsule_231x87.jpg",
             "platforms":{"windows":true,"mac":true,"linux":false}},
            {"type":"app","name":"No id here"}
        ]}"#;
        let hits = parse_steam(body).unwrap();
        // The second has no id, so there is no address to build and no
        // result to show; it is dropped rather than half-drawn.
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.title, "Hades");
        assert_eq!(hit.url, "https://store.steampowered.com/app/1145360/");
        // Not the 231-pixel thumbnail the reply carries.
        assert_eq!(
            hit.image_url,
            "https://cdn.cloudflare.steamstatic.com/steam/apps/1145360/library_600x900.jpg"
        );
        assert_eq!(hit.facts.get("platform").map(String::as_str), Some("Windows, macOS"));
        // Metacritic's number, arriving through a shop. Keeping it would put
        // Steam's name on somebody else's score.
        assert_eq!(hit.rating, None);
    }

    #[test]
    fn a_chosen_steam_result_is_filled_in_from_the_games_own_page() {
        let mut hit = parse_steam(
            r#"{"items":[{"type":"app","name":"Hades","id":1145360,
                "platforms":{"windows":true,"mac":false,"linux":false}}]}"#,
        )
        .unwrap()
        .remove(0);
        // Nothing worth putting on a card yet, which is the reason the
        // second request exists.
        assert_eq!(hit.year, None);
        assert!(hit.summary.is_empty());

        let request = Source::Steam.detail(&hit).expect("a store page to ask for");
        assert_eq!(
            request.url,
            "https://store.steampowered.com/api/appdetails?appids=1145360&l=en"
        );

        let body = r#"{"1145360":{"success":true,"data":{
            "name":"Hades","type":"game","developers":["Supergiant Games"],
            "publishers":["Supergiant Games"],
            "release_date":{"coming_soon":false,"date":"Sep 17, 2020"},
            "metacritic":{"score":93,"url":"https://www.metacritic.com/game/pc/hades"},
            "short_description":"Defy the god of the dead &amp; hack out of the Underworld.",
            "genres":[{"id":"1","description":"Action"},{"id":"23","description":"Indie"}]
        }}}"#;
        Source::Steam.merge_detail(&mut hit, body).unwrap();
        assert_eq!(hit.year, Some(2020));
        assert_eq!(hit.creator, "Supergiant Games");
        assert_eq!(hit.facts.get("developer").map(String::as_str), Some("Supergiant Games"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Action, Indie"));
        assert_eq!(hit.summary, "Defy the god of the dead & hack out of the Underworld.");
        // The platform the search found is not overwritten by the page.
        assert_eq!(hit.facts.get("platform").map(String::as_str), Some("Windows"));

        // The score is Metacritic's, and says so.
        assert_eq!(hit.rating, Some(93));
        assert_eq!(hit.rating_source, "Metacritic");
        let mut item = Item::new(kind("game").id, "Hades");
        apply(&hit, &kind("game"), &mut item, false);
        let theirs = &item.external[0];
        assert_eq!(theirs.source, "Metacritic", "a score keeps the name of whoever gave it");
        assert_eq!(theirs.url, "https://www.metacritic.com/game/pc/hades");
        // The link to the thing itself is still the shop's, because that is
        // where the game is.
        assert_eq!(item.links[0].label, "Steam");
    }

    #[test]
    fn a_detail_reply_about_nothing_leaves_the_result_alone() {
        let mut hit =
            parse_steam(r#"{"items":[{"name":"Hades","id":1145360}]}"#).unwrap().remove(0);
        let before = hit.clone();
        // Not sold here: `success` is false and there is no `data` at all.
        Source::Steam.merge_detail(&mut hit, r#"{"1145360":{"success":false}}"#).unwrap();
        assert_eq!(hit, before);
        // And a reply that is not JSON is an error, never a panic and never
        // a half-filled card.
        assert!(Source::Steam.merge_detail(&mut hit, "<html>nope").is_err());
        assert_eq!(hit, before);
    }
}
