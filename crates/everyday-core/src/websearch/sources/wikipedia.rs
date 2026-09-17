use crate::error::Result;
use crate::websearch::{SearchResult, Source, http_url, string_at, trim_summary};
use serde_json::Value;

pub(crate) fn parse_wikipedia(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let pages = root.pointer("/query/pages").and_then(Value::as_array);
    let Some(pages) = pages else { return Ok(Vec::new()) };
    let mut out: Vec<(i64, SearchResult)> = Vec::new();
    for page in pages {
        let title = string_at(page, "title");
        if title.is_empty() {
            continue;
        }
        // `index` is the search rank. JSON object order is not, so without
        // this the best match arrives wherever the map happened to put it.
        let rank = page.get("index").and_then(Value::as_i64).unwrap_or(i64::MAX);
        out.push((
            rank,
            SearchResult {
                title,
                summary: trim_summary(&string_at(page, "extract")),
                url: http_url(&string_at(page, "fullurl")).unwrap_or_default(),
                image_url: page
                    .pointer("/thumbnail/source")
                    .and_then(Value::as_str)
                    .and_then(|u| http_url(u).ok())
                    .unwrap_or_default(),
                source: Source::Wikipedia.slug().to_string(),
                ..Default::default()
            },
        ));
    }
    out.sort_by_key(|(rank, _)| *rank);
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikipedia_results_keep_the_search_ranking() {
        // The bug this guards: JSON object order is not search order, so the
        // best match arrives wherever the map put it.
        let body = r#"{"query":{"pages":[
            {"index":2,"title":"Dune (novel)","extract":"A 1965 novel.",
             "fullurl":"https://en.wikipedia.org/wiki/Dune_(novel)",
             "thumbnail":{"source":"https://upload.example/dune.jpg"}},
            {"index":1,"title":"Dune","extract":"A sand dune.",
             "fullurl":"https://en.wikipedia.org/wiki/Dune"}
        ]}}"#;
        let hits = parse_wikipedia(body).unwrap();
        assert_eq!(hits[0].title, "Dune", "search rank was not honoured");
        assert_eq!(hits[1].image_url, "https://upload.example/dune.jpg");
    }
}
