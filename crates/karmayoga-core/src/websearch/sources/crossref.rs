//! Crossref: the registry behind nearly every journal article's DOI.
//!
//! A DOI is the one identifier that names an article and nothing else, so a
//! look-up by DOI is exact in a way no title search is -- and Crossref, which
//! holds the record for the great majority of them, answers without a key and
//! without an account. A title is searched too, by its bibliographic
//! similarity, for the article somebody remembers the name of and not the
//! number.
//!
//! The same record answers both, in two envelopes: a work on its own, and a
//! list of them. [`parse_crossref`] reads either.

use crate::error::Result;
use crate::text::one_line;
use crate::websearch::{
    SearchResult, Source, decode_entities, http_url, plain_text, string_at, to_year, trim_summary,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// How many authors a byline names before it says "et al.". A paper from a
/// collaboration lists a thousand, and none of them is a line a card can hold.
const MAX_AUTHORS: usize = 5;

/// The fields a search asks for, and no others. A work carries its whole
/// reference list, which for a review is hundreds of entries and, across a
/// page of results, most of the response.
pub(crate) const SELECT: &str = "DOI,title,subtitle,author,container-title,volume,issue,page,\
                                 article-number,issued,published,published-print,\
                                 published-online,abstract,URL,type";

/// Names as the one line a card prints under a title: the first
/// [`MAX_AUTHORS`], joined by commas, and "et al." for the rest. Shared by
/// everything that makes an item out of somebody's author list -- a Crossref
/// record's, a file's own.
pub(crate) fn join_authors(names: &[String]) -> String {
    let mut line = names.iter().take(MAX_AUTHORS).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > MAX_AUTHORS {
        line.push_str(" et al.");
    }
    line
}

/// The DOI in `text`, when that is all `text` is.
///
/// A DOI as people actually write one: bare (`10.1038/nature14539`), with its
/// scheme (`doi:10.1038/nature14539`), or as the address it resolves at
/// (`https://doi.org/10.1038/nature14539`, `dx.doi.org` too, with or without
/// the `https://`). Nothing else is one: a title that happens to contain a
/// DOI is a title, and an address on any other site is a page.
///
/// A DOI's own characters are almost anything, so the test is its shape and
/// not its alphabet: `10.`, four to nine digits for whoever registered it, a
/// slash, and something after the slash with no white space in it. Trailing
/// full stops, commas and semicolons are the sentence it was copied out of,
/// not part of it -- a closing bracket is left, since old DOIs end in one.
pub fn doi_of(text: &str) -> Option<String> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    // Offsets in `lower` are offsets in `text`: lowercasing ASCII never
    // changes a byte's length.
    let (rest, from_address) = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi.org/",
        "dx.doi.org/",
    ]
    .iter()
    .find(|prefix| lower.starts_with(**prefix))
    .map(|prefix| (&text[prefix.len()..], true))
    .or_else(|| lower.starts_with("doi:").then(|| (text[4..].trim_start(), false)))
    .unwrap_or((text, false));

    // An address may carry a query or a fragment of its own, and writes the
    // DOI's awkward characters as escapes. A bare DOI may legitimately hold
    // either character, so only an address is cut and unescaped.
    let rest = if from_address {
        let end = rest.find(['?', '#']).unwrap_or(rest.len());
        unescape(&rest[..end])
    } else {
        rest.to_string()
    };
    let doi = rest.trim().trim_end_matches(['.', ',', ';']);

    let (registrant, suffix) = doi.strip_prefix("10.")?.split_once('/')?;
    let well_formed = (4..=9).contains(&registrant.len())
        && registrant.bytes().all(|b| b.is_ascii_digit())
        && !suffix.is_empty()
        && !doi.chars().any(|c| c.is_whitespace() || c.is_control());
    well_formed.then(|| doi.to_string())
}

/// `%2F` and its kind read back as the characters they stand for. A `+` is
/// itself here -- a path, not a form -- and anything that is not valid UTF-8
/// once unescaped is left as it was written.
fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = if bytes[i] == b'%' && i + 2 < bytes.len() {
            std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        } else {
            None
        };
        match byte {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// A DOI as it goes in a request's path: the slash that is part of every one
/// stays, and every other character the path would have to be careful about
/// -- the brackets, semicolons and angle brackets older DOIs are full of --
/// is escaped.
pub(crate) fn path_of(doi: &str) -> String {
    let mut out = String::with_capacity(doi.len() + 8);
    for byte in doi.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Where a DOI resolves. Built here and not taken from the record's own
/// `URL`, which older deposits still write as `http://dx.doi.org/...`: the
/// address kept on a shelf is the one everybody now gives, with only the
/// characters an address cannot hold escaped -- the brackets and semicolons
/// of an old DOI read better as they are.
pub(crate) fn doi_link(doi: &str) -> String {
    let mut out = String::from("https://doi.org/");
    for byte in doi.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b'('
            | b')'
            | b':'
            | b';'
            | b'+'
            | b','
            | b'='
            | b'@'
            | b'$'
            | b'*'
            | b'!'
            | b'\'' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) fn parse_crossref(body: &str) -> Result<Vec<SearchResult>> {
    let root: Value = serde_json::from_str(body)?;
    let Some(message) = root.get("message") else { return Ok(Vec::new()) };
    let works: Vec<&Value> = match message.get("items").and_then(Value::as_array) {
        Some(items) => items.iter().collect(),
        // A work asked for by its DOI is the message itself.
        None if message.get("DOI").is_some() => vec![message],
        None => Vec::new(),
    };
    Ok(works.into_iter().filter_map(work).collect())
}

/// One work as a result. Without a title there is nothing to show -- some
/// deposits are datasets with only a number -- so it is not one.
fn work(value: &Value) -> Option<SearchResult> {
    let title = plain_text(&first(value, "title"));
    if title.is_empty() {
        return None;
    }
    let doi = string_at(value, "DOI");
    let creator = byline(value);
    let journal = plain_text(&first(value, "container-title"));
    let pages = match string_at(value, "page") {
        page if !page.is_empty() => page,
        // A journal that numbers its articles rather than paging them.
        _ => string_at(value, "article-number"),
    };
    let url = if doi.is_empty() {
        // Not every deposit has a DOI worth the name; one that names its own
        // address is still somewhere to read it.
        http_url(&string_at(value, "URL")).unwrap_or_default()
    } else {
        doi_link(&doi)
    };

    let (volume, issue) = (string_at(value, "volume"), string_at(value, "issue"));
    let mut facts = BTreeMap::new();
    for (key, text) in [
        ("author", &creator),
        ("publication", &journal),
        ("doi", &doi),
        ("volume", &volume),
        ("issue", &issue),
        ("pages", &pages),
        ("url", &url),
    ] {
        if !text.trim().is_empty() {
            facts.insert(key.to_string(), text.trim().to_string());
        }
    }

    Some(SearchResult {
        title,
        subtitle: plain_text(&first(value, "subtitle")),
        creator,
        summary: abstract_text(&string_at(value, "abstract")),
        year: ["issued", "published", "published-print", "published-online"]
            .iter()
            .find_map(|key| year_of(value.get(*key)?)),
        url,
        facts,
        source: Source::Crossref.slug().to_string(),
        ..Default::default()
    })
}

/// The first string of `key`, which Crossref makes a list of one even for a
/// single title.
fn first(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::Array(items)) => {
            items.iter().find_map(Value::as_str).unwrap_or_default().to_string()
        }
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// The authors as "Given Family", in the order the paper lists them. A body
/// that is the author -- a consortium, a committee -- has a `name` and nothing
/// else.
fn byline(value: &Value) -> String {
    let Some(authors) = value.get("author").and_then(Value::as_array) else {
        return String::new();
    };
    let names: Vec<String> = authors
        .iter()
        .map(|a| {
            let (given, family) = (string_at(a, "given"), string_at(a, "family"));
            let name = match (given.is_empty(), family.is_empty()) {
                (false, false) => format!("{given} {family}"),
                (false, true) => given,
                (true, false) => family,
                (true, true) => string_at(a, "name"),
            };
            plain_text(&name)
        })
        .filter(|name| !name.is_empty())
        .collect();
    join_authors(&names)
}

/// The year in one of Crossref's dates: `{"date-parts": [[2015, 5, 27]]}`,
/// where a part a record does not know is `null`.
fn year_of(date: &Value) -> Option<i16> {
    date.pointer("/date-parts/0/0").and_then(Value::as_i64).and_then(to_year)
}

/// An abstract as plain words.
///
/// Crossref hands over what the publisher deposited, which is JATS: the
/// paragraphs are `<jats:p>`, a structured abstract's sections each have a
/// `<jats:title>`, and the whole is XML-escaped -- so an `&nbsp;` in the
/// original arrives as `&amp;nbsp;`, which is why entities are read twice.
/// Titles become labels ("Methods: ...") so a structured abstract still says
/// which part is which; a bare "Abstract:" heading is dropped, being what the
/// whole thing already is.
fn abstract_text(jats: &str) -> String {
    let marked = jats
        .replace("</jats:title>", ": ")
        .replace("</jats:p>", " ")
        .replace("</jats:sec>", " ")
        .replace("</title>", ": ")
        .replace("</p>", " ");
    // The second reading is of entities only: tags have been dealt with, and
    // a `<` it turns up is somebody's "p < 0.05", not markup.
    let mut text = one_line(&decode_entities(&plain_text(&marked)));
    for label in ["abstract:", "summary:"] {
        if text.get(..label.len()).is_some_and(|head| head.eq_ignore_ascii_case(label)) {
            text = text[label.len()..].trim_start().to_string();
            break;
        }
    }
    trim_summary(&text)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Nature's "Deep learning", as Crossref answers for its DOI -- trimmed
    /// of its hundred references and licences, which nothing here reads.
    pub(crate) const DEEP_LEARNING: &str = r#"{"status":"ok","message-type":"work","message":{
        "publisher":"Springer Science and Business Media LLC","issue":"7553",
        "short-container-title":["Nature"],"published-print":{"date-parts":[[2015,5,28]]},
        "DOI":"10.1038/nature14539","type":"journal-article","page":"436-444",
        "title":["Deep learning"],"volume":"521",
        "author":[{"given":"Yann","family":"LeCun","sequence":"first"},
                  {"given":"Yoshua","family":"Bengio","sequence":"additional"},
                  {"given":"Geoffrey","family":"Hinton","sequence":"additional"}],
        "published-online":{"date-parts":[[2015,5,27]]},"container-title":["Nature"],
        "issued":{"date-parts":[[2015,5,27]]},
        "URL":"https://doi.org/10.1038/nature14539","subtitle":[]}}"#;

    /// A search for a title: a chapter, a preprint with a (double-escaped)
    /// abstract, and a piece with no journal to its name.
    pub(crate) const SEARCH: &str = r#"{"status":"ok","message-type":"work-list","message":{
        "facets":{},"total-results":1258886,"items":[
        {"DOI":"10.1007/978-3-031-84300-6_13","type":"book-chapter","page":"297-314",
         "title":["Is Attention All You Need?"],
         "author":[{"ORCID":"https://orcid.org/0000-0001-5519-842X","given":"Patrick","family":"Mineault","sequence":"first","affiliation":[]}],
         "container-title":["From Human Attention to Computational Attention"],
         "score":32.92009,"issued":{"date-parts":[[2025]]},
         "URL":"https://doi.org/10.1007/978-3-031-84300-6_13","published":{"date-parts":[[2025]]}},
        {"abstract":"<jats:p>This study discusses the parameters that define the value of artificial intelligence (AI) tokens based on user interaction. The results show that monthly active users (MAU) and website visits significantly predict valuation, while pricing shows a marginal effect. This research provides insights for stakeholders in understanding economic factors affecting AI token values, emphasizing user engagement and pricing strategies.&amp;nbsp;</jats:p>",
         "DOI":"10.2139/ssrn.4993784","type":"posted-content",
         "title":["Attention Is All You Need: An Analysis Of The Valuation Of Artificial Intelligence Tokens"],
         "author":[{"given":"Mfon","family":"Akpan","sequence":"first","affiliation":[]}],
         "score":31.053305,"issued":{"date-parts":[[2024]]},
         "URL":"https://doi.org/10.2139/ssrn.4993784","published":{"date-parts":[[2024]]}},
        {"DOI":"10.5040/9781350101272.00000005","type":"other","title":["All You Need Is LSD"],
         "author":[{"given":"Leo","family":"Butler","sequence":"first","affiliation":[]}],
         "container-title":["All You Need Is LSD"],"score":30.769325,
         "issued":{"date-parts":[[2018]]},
         "URL":"https://doi.org/10.5040/9781350101272.00000005"}],
        "items-per-page":3,"query":{"start-index":0,"search-terms":null}}}"#;

    /// An article whose abstract is in sections, as a Frontiers deposit is,
    /// with the white space the publisher's XML was indented with.
    const STRUCTURED: &str = r#"{"message":{"DOI":"10.3389/fmicb.2025.1691537",
        "title":["Reestablishment of an algae-bacteria pest model using continuous passage in the laboratory"],
        "abstract":"<jats:sec>\n                    <jats:title>Introduction</jats:title>\n                    <jats:p>\n                      The paucity of laboratory models hinders our understanding of bacteria that predate algae. A novel predatory bacterium was found in ponds of\n                      <jats:italic>Nannochloropsis salina</jats:italic>\n                      located in New Mexico.\n                    </jats:p>\n                  </jats:sec>\n                  <jats:sec>\n                    <jats:title>Methods</jats:title>\n                    <jats:p>We passaged the pest into healthy cultures ten times.</jats:p>\n                  </jats:sec>"}}"#;

    fn nth(body: &str, n: usize) -> SearchResult {
        parse_crossref(body).unwrap().swap_remove(n)
    }

    #[test]
    fn a_work_asked_for_by_its_doi_is_one_result_with_everything_a_shelf_keeps() {
        let hits = parse_crossref(DEEP_LEARNING).unwrap();
        assert_eq!(hits.len(), 1);
        let deep = &hits[0];
        assert_eq!(deep.title, "Deep learning");
        assert_eq!(deep.creator, "Yann LeCun, Yoshua Bengio, Geoffrey Hinton");
        assert_eq!(deep.year, Some(2015));
        assert_eq!(deep.url, "https://doi.org/10.1038/nature14539");
        assert_eq!(deep.source, "crossref");
        assert_eq!(deep.subtitle, "", "an empty list is no subtitle");
        let fact = |key: &str| deep.facts.get(key).map(String::as_str);
        assert_eq!(fact("publication"), Some("Nature"), "the journal is the publication");
        assert_eq!(fact("doi"), Some("10.1038/nature14539"));
        assert_eq!(fact("volume"), Some("521"));
        assert_eq!(fact("issue"), Some("7553"));
        assert_eq!(fact("pages"), Some("436-444"));
        assert_eq!(fact("author"), Some("Yann LeCun, Yoshua Bengio, Geoffrey Hinton"));
        assert_eq!(fact("url"), Some("https://doi.org/10.1038/nature14539"));
    }

    #[test]
    fn a_search_keeps_crossrefs_ranking_and_says_what_each_answer_is() {
        let hits = parse_crossref(SEARCH).unwrap();
        let titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Is Attention All You Need?",
                "Attention Is All You Need: An Analysis Of The Valuation Of Artificial \
                 Intelligence Tokens",
                "All You Need Is LSD",
            ]
        );
        // A chapter's container is its book, and is still where it was
        // published; the year falls back from `issued` to `published`.
        assert_eq!(
            hits[0].facts.get("publication").map(String::as_str),
            Some("From Human Attention to Computational Attention")
        );
        assert_eq!(hits[0].year, Some(2025));
        // A preprint has no journal, and a fact with nothing to say is left
        // out rather than written empty.
        assert!(!hits[1].facts.contains_key("publication"));
        assert!(!hits[1].facts.contains_key("volume"));
        assert_eq!(hits[1].facts.get("doi").map(String::as_str), Some("10.2139/ssrn.4993784"));
    }

    #[test]
    fn an_abstract_arrives_as_words_without_the_markup_it_was_deposited_in() {
        let hit = nth(SEARCH, 1);
        assert!(hit.summary.starts_with("This study discusses the parameters"), "{}", hit.summary);
        assert!(!hit.summary.contains('<') && !hit.summary.contains("&nbsp;"), "{}", hit.summary);
        assert!(hit.summary.ends_with("pricing strategies."), "{}", hit.summary);
    }

    #[test]
    fn a_structured_abstract_keeps_its_section_labels_and_loses_its_indentation() {
        let hit = nth(STRUCTURED, 0);
        assert_eq!(
            hit.summary,
            "Introduction: The paucity of laboratory models hinders our understanding of \
             bacteria that predate algae. A novel predatory bacterium was found in ponds of \
             Nannochloropsis salina located in New Mexico. \
             Methods: We passaged the pest into healthy cultures ten times."
        );
    }

    #[test]
    fn a_heading_that_only_says_abstract_is_not_part_of_it() {
        let body = r#"{"message":{"DOI":"10.1/x","title":["T"],
            "abstract":"<jats:title>Abstract</jats:title><jats:p>Vision loss affects &gt;3 million people.</jats:p>"}}"#;
        assert_eq!(nth(body, 0).summary, "Vision loss affects >3 million people.");
    }

    #[test]
    fn an_abstract_is_cut_to_what_a_card_holds() {
        let long = "word ".repeat(400);
        let body = format!(
            r#"{{"message":{{"DOI":"10.1/x","title":["T"],"abstract":"<jats:p>{long}</jats:p>"}}}}"#
        );
        let summary = nth(&body, 0).summary;
        assert!(summary.chars().count() <= 481, "{}", summary.chars().count());
        assert!(summary.ends_with('\u{2026}'));
    }

    #[test]
    fn markup_and_entities_in_a_title_are_read_not_shown() {
        let body = r#"{"message":{"DOI":"10.1/x",
            "title":["Fitting <i>Escherichia coli</i> &amp; its kin:\n  a note"]}}"#;
        assert_eq!(nth(body, 0).title, "Fitting Escherichia coli & its kin: a note");
    }

    #[test]
    fn a_long_author_list_is_cut_and_a_consortium_is_still_named() {
        let authors: String = (1..=8)
            .map(|n| format!(r#"{{"given":"A{n}","family":"Writer"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            r#"{{"message":{{"DOI":"10.1/x","title":["Big paper"],"author":[{authors}]}}}}"#
        );
        assert_eq!(
            nth(&body, 0).creator,
            "A1 Writer, A2 Writer, A3 Writer, A4 Writer, A5 Writer et al."
        );

        let body = r#"{"message":{"DOI":"10.1/x","title":["A report"],
            "author":[{"name":"ATLAS Collaboration"},{"family":"Solo"},{"given":"Cher"}]}}"#;
        assert_eq!(nth(body, 0).creator, "ATLAS Collaboration, Solo, Cher");
    }

    #[test]
    fn an_article_number_stands_in_for_pages_and_the_link_is_built_from_the_doi() {
        // The deposit's own `URL` is the older `http://dx.doi.org/` form.
        let body = r#"{"message":{"DOI":"10.1371/journal.pmed.0020124",
            "title":["Why Most Published Research Findings Are False"],
            "URL":"http://dx.doi.org/10.1371/journal.pmed.0020124",
            "article-number":"e124","volume":"2","issued":{"date-parts":[[2005,8,30]]}}}"#;
        let hit = nth(body, 0);
        assert_eq!(hit.facts.get("pages").map(String::as_str), Some("e124"));
        assert_eq!(hit.url, "https://doi.org/10.1371/journal.pmed.0020124");
        assert_eq!(hit.year, Some(2005));
    }

    #[test]
    fn a_link_keeps_an_old_dois_brackets_and_escapes_what_an_address_cannot_hold() {
        assert_eq!(
            doi_link("10.1016/S0140-6736(20)30183-5"),
            "https://doi.org/10.1016/S0140-6736(20)30183-5"
        );
        assert_eq!(doi_link("10.1002/a<b> c#d"), "https://doi.org/10.1002/a%3Cb%3E%20c%23d");
    }

    #[test]
    fn a_date_with_a_part_unknown_is_not_a_year_and_the_next_date_is_tried() {
        let body = r#"{"message":{"DOI":"10.1/x","title":["T"],
            "issued":{"date-parts":[[null]]},"published-online":{"date-parts":[[2019,3]]}}}"#;
        assert_eq!(nth(body, 0).year, Some(2019));
    }

    #[test]
    fn what_is_not_a_work_with_a_title_is_not_a_result() {
        assert!(parse_crossref(r#"{"message":{"items":[{"DOI":"10.1/x"}]}}"#).unwrap().is_empty());
        assert!(parse_crossref(r#"{"status":"failed"}"#).unwrap().is_empty());
        assert!(parse_crossref(r#"{"message":{"items":[]}}"#).unwrap().is_empty());
        assert!(parse_crossref("<html>Resource not found.</html>").is_err());
    }

    #[test]
    fn a_doi_is_found_however_it_is_written_and_nowhere_else() {
        for written in [
            "10.1038/nature14539",
            "  10.1038/nature14539  ",
            "doi:10.1038/nature14539",
            "DOI: 10.1038/nature14539",
            "https://doi.org/10.1038/nature14539",
            "http://dx.doi.org/10.1038/nature14539",
            "doi.org/10.1038/nature14539",
            "https://doi.org/10.1038/nature14539?utm_source=newsletter#abstract",
            "10.1038/nature14539.",
        ] {
            assert_eq!(doi_of(written).as_deref(), Some("10.1038/nature14539"), "{written:?}");
        }
        // The characters an address escapes come back as themselves, and a
        // closing bracket is the DOI's own.
        assert_eq!(
            doi_of("https://doi.org/10.1016/S0140-6736%2820%2930183-5").as_deref(),
            Some("10.1016/S0140-6736(20)30183-5")
        );
        assert!(doi_of("10.1002/(SICI)1097-4571(199806)49:8<693::AID-ASI4>3.0.CO;2-O").is_some());
        // `+` is a plus in a path.
        assert_eq!(doi_of("https://doi.org/10.1000/a+b").as_deref(), Some("10.1000/a+b"));

        for not in [
            "Deep learning",
            "10.5 reasons to cook",
            "10.1038",
            "10.1038/",
            "10.12/short-registrant",
            "https://example.com/10.1038/nature14539",
            "see 10.1038/nature14539 for details",
            "https://doi.org/",
            "",
        ] {
            assert_eq!(doi_of(not), None, "{not:?}");
        }
    }

    #[test]
    fn a_doi_goes_in_a_path_with_its_slash_and_without_its_awkward_characters() {
        assert_eq!(path_of("10.1038/nature14539"), "10.1038/nature14539");
        assert_eq!(path_of("10.1016/S0140-6736(20)30183-5"), "10.1016/S0140-6736%2820%2930183-5");
        assert_eq!(path_of("10.1002/a<b>;c"), "10.1002/a%3Cb%3E%3Bc");
    }

    #[test]
    fn authors_are_cut_at_five_and_say_so() {
        let names: Vec<String> = ["A", "B", "C"].iter().map(|n| n.to_string()).collect();
        assert_eq!(join_authors(&names), "A, B, C");
        assert_eq!(join_authors(&[]), "");
        let many: Vec<String> = (1..=6).map(|n| format!("N{n}")).collect();
        assert_eq!(join_authors(&many), "N1, N2, N3, N4, N5 et al.");
    }
}
