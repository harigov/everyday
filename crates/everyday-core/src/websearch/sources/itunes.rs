use crate::error::Result;
use crate::websearch::{SearchResult, Source, http_url, string_at, trim_summary, year_from_iso};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn parse_itunes(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(results) = root.get("results").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for r in results {
        // A film is a `trackName`, an album a `collectionName`; asking for
        // both in that order is what makes one parser serve four media.
        let title = match string_at(r, "trackName") {
            t if !t.is_empty() => t,
            _ => string_at(r, "collectionName"),
        };
        if title.is_empty() {
            continue;
        }
        let creator = string_at(r, "artistName");
        let mut facts = BTreeMap::new();
        let genre = string_at(r, "primaryGenreName");
        if !genre.is_empty() {
            facts.insert("genre".to_string(), genre);
        }
        // The same number under three names, because a kind's field key is
        // the kind's business and a runtime is a runtime.
        if let Some(ms) = r.get("trackTimeMillis").and_then(Value::as_i64) {
            let minutes = (ms / 60_000).max(0).to_string();
            facts.insert("runtime".to_string(), minutes);
        }
        if let Some(n) = r.get("trackCount").and_then(Value::as_i64) {
            facts.insert("tracks".to_string(), n.to_string());
        }
        if !creator.is_empty() {
            // Whichever of these the kind has a field for wins; the rest are
            // dropped by `apply`, which only keeps facts a kind can show.
            for key in ["director", "artist", "show"] {
                facts.insert(key.to_string(), creator.clone());
            }
        }
        let description = match string_at(r, "longDescription") {
            d if !d.is_empty() => d,
            _ => string_at(r, "description"),
        };
        out.push(SearchResult {
            title,
            subtitle: string_at(r, "collectionName"),
            creator,
            summary: trim_summary(&description),
            year: year_from_iso(&string_at(r, "releaseDate")),
            url: http_url(&string_at(r, "trackViewUrl"))
                .or_else(|_| http_url(&string_at(r, "collectionViewUrl")))
                .unwrap_or_default(),
            // The artwork is served at 100px unless the size in the path is
            // rewritten. Apple serves whatever is asked for from the same
            // address, and 100px is unusable on any display made this decade.
            image_url: http_url(&string_at(r, "artworkUrl100").replace("100x100", "600x600"))
                .unwrap_or_default(),
            facts,
            source: Source::ITunes.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itunes_artwork_is_asked_for_at_a_usable_size() {
        let body = r#"{"results":[{
            "trackName":"Dune","artistName":"Denis Villeneuve",
            "artworkUrl100":"https://is1.example.com/img/100x100bb.jpg",
            "releaseDate":"2021-10-22T07:00:00Z","primaryGenreName":"Sci-Fi",
            "trackTimeMillis":9360000,"trackViewUrl":"https://itunes.example/dune",
            "longDescription":"A desert planet."
        }]}"#;
        let hit = &parse_itunes(body).unwrap()[0];
        assert_eq!(hit.image_url, "https://is1.example.com/img/600x600bb.jpg");
        assert_eq!(hit.year, Some(2021));
        assert_eq!(hit.facts.get("runtime").map(String::as_str), Some("156"));
        assert_eq!(hit.facts.get("genre").map(String::as_str), Some("Sci-Fi"));
        assert_eq!(hit.creator, "Denis Villeneuve");
    }

    #[test]
    fn itunes_falls_back_to_the_collection_name_for_an_album() {
        let body = r#"{"results":[{
            "collectionName":"Kind of Blue","artistName":"Miles Davis",
            "trackCount":5,"releaseDate":"1959-08-17T07:00:00Z",
            "collectionViewUrl":"https://itunes.example/kob"
        }]}"#;
        let hit = &parse_itunes(body).unwrap()[0];
        assert_eq!(hit.title, "Kind of Blue");
        assert_eq!(hit.facts.get("tracks").map(String::as_str), Some("5"));
        assert_eq!(hit.url, "https://itunes.example/kob");
    }
}
