use crate::error::Result;
use crate::websearch::{SearchResult, Source, string_at, year_from_iso};
use serde_json::Value;
use std::collections::BTreeMap;

/// Pull records out of MusicBrainz's release-group search.
///
/// # Where the picture comes from
///
/// Not from this reply: MusicBrainz holds the facts and the Cover Art
/// Archive holds the sleeves, and a search says nothing about whether one
/// exists. The address is *derived* from the record's id, which the archive
/// serves or answers 404 to, and a 404 here is a card with no artwork rather
/// than a failed lookup — see `apply_and_cover` in the service, which treats
/// the picture as best-effort for exactly this reason. The alternative was a
/// second request per result, eight of them per search, to a rate-limited
/// host, to find out something the card can live without.
pub(crate) fn parse_musicbrainz(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(groups) = root.get("release-groups").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<(i64, SearchResult)> = Vec::new();
    for group in groups {
        let title = string_at(group, "title");
        let id = string_at(group, "id");
        if title.is_empty() || id.is_empty() {
            continue;
        }
        // A credit is a list — "Simon & Garfunkel" is one artist, but a
        // duet is two joined by a phrase the data carries. Joining on it is
        // what turns three objects back into the line printed on the sleeve.
        let artist = group
            .get("artist-credit")
            .and_then(Value::as_array)
            .map(|credits| {
                credits.iter().fold(String::new(), |mut line, credit| {
                    line.push_str(&string_at(credit, "name"));
                    line.push_str(credit.get("joinphrase").and_then(Value::as_str).unwrap_or(""));
                    line
                })
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut facts = BTreeMap::new();
        if !artist.is_empty() {
            facts.insert("artist".to_string(), artist.clone());
        }
        // "Album", "EP", "Single", and the secondary types that say a record
        // is a compilation or a soundtrack rather than a record proper.
        let mut descriptors: Vec<String> = Vec::new();
        let primary = string_at(group, "primary-type");
        if !primary.is_empty() {
            descriptors.push(primary);
        }
        if let Some(secondary) = group.get("secondary-types").and_then(Value::as_array) {
            descriptors.extend(secondary.iter().filter_map(Value::as_str).map(str::to_string));
        }
        out.push((
            // Lucene's relevance, which the endpoint sorts by and which a
            // parser has no business reordering. Kept explicitly so that it
            // survives anything this function does to the list.
            group.get("score").and_then(Value::as_i64).unwrap_or(0),
            SearchResult {
                title,
                subtitle: descriptors.join(", "),
                creator: artist,
                summary: string_at(group, "disambiguation"),
                year: year_from_iso(&string_at(group, "first-release-date")),
                url: format!("https://musicbrainz.org/release-group/{id}"),
                image_url: format!("https://coverartarchive.org/release-group/{id}/front-500"),
                facts,
                source: Source::MusicBrainz.slug().to_string(),
                ..Default::default()
            },
        ));
    }
    out.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_musicbrainz_cover_is_derived_from_the_records_id() {
        // The search reply says nothing about artwork; the address is built
        // from the id and the archive answers it or does not.
        let body = r#"{"release-groups":[
            {"id":"6f25f9fb","score":87,"title":"KID A MNESIA",
             "primary-type":"Album","secondary-types":["Compilation"],
             "first-release-date":"2021-11-05",
             "artist-credit":[{"name":"Radiohead"}]},
            {"id":"e75c0549","score":100,"title":"Kid A","primary-type":"Album",
             "first-release-date":"2000-08-03",
             "artist-credit":[{"name":"Radiohead"}]}
        ]}"#;
        let hits = parse_musicbrainz(body).unwrap();
        // Relevance decides the order, not the order the reply happened to
        // arrive in.
        assert_eq!(hits[0].title, "Kid A");
        assert_eq!(hits[0].creator, "Radiohead");
        // The year the record came out, not the year of a remaster somebody
        // is selling -- which is the whole reason this source is here.
        assert_eq!(hits[0].year, Some(2000));
        assert_eq!(hits[0].url, "https://musicbrainz.org/release-group/e75c0549");
        assert_eq!(
            hits[0].image_url,
            "https://coverartarchive.org/release-group/e75c0549/front-500"
        );
        assert_eq!(hits[0].facts.get("artist").map(String::as_str), Some("Radiohead"));
        assert_eq!(hits[1].subtitle, "Album, Compilation");
    }

    #[test]
    fn a_split_artist_credit_is_joined_back_into_one_line() {
        let body = r#"{"release-groups":[{"id":"x","title":"Watch the Throne",
            "artist-credit":[{"name":"Jay-Z","joinphrase":" & "},{"name":"Kanye West"}]}]}"#;
        let hits = parse_musicbrainz(body).unwrap();
        assert_eq!(hits[0].creator, "Jay-Z & Kanye West");
    }
}
