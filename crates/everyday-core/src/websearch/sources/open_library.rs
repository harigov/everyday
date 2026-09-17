use crate::error::Result;
use crate::library::normalize_rating;
use crate::websearch::{SearchResult, Source, first_string, string_at, to_year, trim_summary};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn parse_open_library(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(docs) = root.get("docs").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for doc in docs {
        let title = string_at(doc, "title");
        if title.is_empty() {
            continue;
        }
        let mut facts = BTreeMap::new();
        let author = first_string(doc, "author_name");
        if !author.is_empty() {
            facts.insert("author".to_string(), author.clone());
        }
        if let Some(pages) = doc.get("number_of_pages_median").and_then(Value::as_i64) {
            facts.insert("pages".to_string(), pages.to_string());
        }
        let publisher = first_string(doc, "publisher");
        if !publisher.is_empty() {
            facts.insert("publisher".to_string(), publisher);
        }
        let isbn = first_string(doc, "isbn");
        if !isbn.is_empty() {
            facts.insert("isbn".to_string(), isbn);
        }
        // Open Library rates out of five.
        let rating = doc
            .get("ratings_average")
            .and_then(Value::as_f64)
            .and_then(|score| normalize_rating(score, 5.0));
        let key = string_at(doc, "key");
        out.push(SearchResult {
            title,
            subtitle: string_at(doc, "subtitle"),
            creator: author,
            summary: trim_summary(&first_string(doc, "first_sentence")),
            year: doc.get("first_publish_year").and_then(Value::as_i64).and_then(to_year),
            url: if key.is_empty() {
                String::new()
            } else {
                format!("https://openlibrary.org{key}")
            },
            image_url: doc
                .get("cover_i")
                .and_then(Value::as_i64)
                .map(|id| format!("https://covers.openlibrary.org/b/id/{id}-L.jpg"))
                .unwrap_or_default(),
            rating,
            rating_count: doc
                .get("ratings_count")
                .and_then(Value::as_i64)
                .and_then(|n| u32::try_from(n).ok()),
            facts,
            source: Source::OpenLibrary.slug().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_library_gives_a_book_its_cover_and_its_numbers() {
        let body = r#"{"docs":[{
            "key":"/works/OL893415W","title":"Dune","subtitle":"Book one",
            "author_name":["Frank Herbert"],"first_publish_year":1965,
            "cover_i":8188463,"number_of_pages_median":412,
            "publisher":["Chilton Books"],"isbn":["9780441013593"],
            "ratings_average":4.2,"ratings_count":1204,
            "first_sentence":["A beginning is the time for taking the most delicate care."]
        }]}"#;
        let hits = parse_open_library(body).unwrap();
        let hit = &hits[0];
        assert_eq!(hit.title, "Dune");
        assert_eq!(hit.creator, "Frank Herbert");
        assert_eq!(hit.year, Some(1965));
        assert_eq!(hit.url, "https://openlibrary.org/works/OL893415W");
        assert_eq!(hit.image_url, "https://covers.openlibrary.org/b/id/8188463-L.jpg");
        // 4.2 out of five is 84 out of a hundred.
        assert_eq!(hit.rating, Some(84));
        assert_eq!(hit.rating_count, Some(1204));
        assert_eq!(hit.facts.get("pages").map(String::as_str), Some("412"));
        assert_eq!(hit.facts.get("isbn").map(String::as_str), Some("9780441013593"));
        assert!(hit.summary.starts_with("A beginning"));
    }
}
