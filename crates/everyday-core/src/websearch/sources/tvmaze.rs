use crate::error::Result;
use crate::library::normalize_rating;
use crate::websearch::{
    SearchResult, Source, decode_entities, http_url, string_at, strip_tags, trim_summary,
    year_from_iso,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Pull shows out of TVmaze's search reply.
///
/// The endpoint answers with a list of `{score, show}` already ranked, so
/// the order is kept as given. Everything a television shelf asks for is in
/// the one reply — the network, the genres, the year it began, a portrait
/// and the site's own rating — which is the whole reason this source is
/// here rather than a shop's television section.
pub(crate) fn parse_tvmaze(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(hits) = root.as_array() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for hit in hits {
        // A search answers with the show wrapped in a score; the endpoints
        // that return a show on its own do not. Accept both, so that a
        // single-show reply is not silently no results.
        let show = hit.get("show").unwrap_or(hit);
        let title = string_at(show, "name");
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        // Broadcast or streamed: one field either way, because "where it is
        // on" is one question however the industry files it.
        let network = match show.pointer("/network/name").and_then(Value::as_str) {
            Some(name) => name.to_string(),
            None => show.pointer("/webChannel/name").and_then(Value::as_str).unwrap_or("").into(),
        };
        if !network.is_empty() {
            facts.insert("network".to_string(), network.clone());
        }
        let genres: Vec<&str> = show
            .get("genres")
            .and_then(Value::as_array)
            .map(|g| g.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !genres.is_empty() {
            facts.insert("genre".to_string(), genres.join(", "));
        }
        if let Some(minutes) = show
            .get("averageRuntime")
            .and_then(Value::as_i64)
            .or_else(|| show.get("runtime").and_then(Value::as_i64))
        {
            facts.insert("runtime".to_string(), minutes.to_string());
        }
        out.push(SearchResult {
            title,
            subtitle: network,
            // TVmaze names no creator in a search reply, and guessing one
            // from the network would be worse than leaving it for the person
            // who knows.
            creator: String::new(),
            // The summary is a fragment of HTML, always. Tags out, entities
            // decoded, then cut — in that order, or a `&amp;` lands mid-cut.
            summary: trim_summary(&decode_entities(&strip_tags(&string_at(show, "summary")))),
            year: year_from_iso(&string_at(show, "premiered")),
            url: http_url(&string_at(show, "url")).unwrap_or_default(),
            image_url: show
                .pointer("/image/original")
                .or_else(|| show.pointer("/image/medium"))
                .and_then(Value::as_str)
                .and_then(|u| http_url(u).ok())
                .unwrap_or_default(),
            // TVmaze rates out of ten, and says nothing about how many
            // people voted.
            rating: show
                .pointer("/rating/average")
                .and_then(Value::as_f64)
                .and_then(|score| normalize_rating(score, 10.0)),
            rating_count: None,
            facts,
            source: Source::TvMaze.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tvmaze_answers_with_the_show_rather_than_a_season_for_sale() {
        // Shaped as the endpoint answers: the show inside a relevance score,
        // a streamer rather than a network, and a blurb that is HTML.
        let body = r#"[{"score":0.89,"show":{
            "id":44933,"url":"https://www.tvmaze.com/shows/44933/severance",
            "name":"Severance","genres":["Drama","Science-Fiction"],
            "averageRuntime":49,"premiered":"2022-02-18","rating":{"average":7.6},
            "network":null,"webChannel":{"id":310,"name":"Apple TV"},
            "image":{"medium":"https://static.tvmaze.com/m.jpg",
                     "original":"https://static.tvmaze.com/o.jpg"},
            "summary":"<p>Mark Scout leads a team at Lumon &amp; co.</p>"
        }}]"#;
        let hits = parse_tvmaze(body).unwrap();
        let hit = &hits[0];
        assert_eq!(hit.title, "Severance");
        assert_eq!(hit.year, Some(2022));
        // The streamer stands in for the network, because "where it is on"
        // is one question however the industry files it.
        assert_eq!(hit.facts.get("network").map(String::as_str), Some("Apple TV"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Drama, Science-Fiction"));
        assert_eq!(hit.facts.get("runtime").map(String::as_str), Some("49"));
        // 7.6 out of ten, on the same hundred as everybody else's score.
        assert_eq!(hit.rating, Some(76));
        assert_eq!(hit.image_url, "https://static.tvmaze.com/o.jpg");
        // Markup out and entities decoded, in that order.
        assert_eq!(hit.summary, "Mark Scout leads a team at Lumon & co.");
    }
}
