use crate::websearch::{
    SearchResult, Source, decode_entities, decode_percent, http_url, strip_tags,
};

/// Pull results out of DuckDuckGo's HTML endpoint.
///
/// Hand-rolled and deliberately forgiving. This is the one parser here that
/// reads a page meant for a browser rather than a documented reply, so it
/// assumes nothing about the surrounding markup: it looks for the two
/// classes that carry a result and skips anything it cannot make sense of.
/// A redesign at the other end therefore produces an empty list — which the
/// interface says plainly — rather than a shelf full of navigation links.
pub fn parse_duckduckgo(html: &str) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    // Each result opens with an anchor carrying `result__a`. Walking the
    // occurrences of that class rather than a DOM keeps this to a page of
    // code; the cost -- that a class rename breaks it -- is unavoidable
    // either way, and breaking means returning nothing.
    while let Some(offset) = html[cursor..].find(RESULT_LINK) {
        let at = cursor + offset;
        let (before, after) = (&html[cursor..at], &html[at..]);
        cursor = at + RESULT_LINK.len();

        let Some(title) = anchor_text(after) else { continue };
        if title.trim().is_empty() {
            continue;
        }
        // The href may sit either side of the class within the same opening
        // tag, and both orders occur. Look forward to the `>` first, then
        // backwards no further than the `<a` that opened this tag -- ranging
        // over all of `before` would pick up the *previous* result's link.
        let url = after
            .split_once('>')
            .and_then(|(attrs, _)| last_href(attrs))
            .or_else(|| before.rfind("<a").and_then(|from| last_href(&before[from..])))
            .map(|href| unwrap_redirect(&href))
            .and_then(|url| http_url(&url).ok())
            .unwrap_or_default();

        let summary = after
            .split_once("class=\"result__snippet\"")
            .and_then(|(_, rest)| anchor_text(rest))
            .unwrap_or_default();

        out.push(SearchResult {
            title: decode_entities(&title),
            summary: decode_entities(&summary),
            url,
            source: Source::Web.slug().to_string(),
            ..Default::default()
        });
    }
    out
}

/// The class every result's title link carries.
const RESULT_LINK: &str = "class=\"result__a\"";
/// The words inside the element whose opening tag `after` is part-way
/// through, with any markup between them removed.
///
/// Reads to the closing `</a>` rather than to the next `<`, because a
/// result's title arrives with the matched words already wrapped in `<b>` --
/// stopping at the first tag would return the empty run before "Dune" and
/// throw the title away.
fn anchor_text(after: &str) -> Option<String> {
    let start = after.find('>')? + 1;
    let end = after[start..].find("</a>")? + start;
    Some(strip_tags(&after[start..end]))
}

/// The last `href="…"` in a fragment.
fn last_href(fragment: &str) -> Option<String> {
    let at = fragment.rfind("href=\"")? + 6;
    let end = fragment[at..].find('"')? + at;
    Some(fragment[at..end].to_string())
}

/// DuckDuckGo wraps every outbound link in `/l/?uddg=<encoded>`. Unwrap it,
/// so what is stored is the address of the page rather than a redirector's.
fn unwrap_redirect(href: &str) -> String {
    let Some(at) = href.find("uddg=") else {
        // Protocol-relative links are what the page actually emits.
        return href
            .strip_prefix("//")
            .map_or_else(|| href.to_string(), |r| format!("https://{r}"));
    };
    let tail = &href[at + 5..];
    let encoded = tail.split(['&', '"']).next().unwrap_or(tail);
    decode_percent(encoded)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const DDG: &str = r#"
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fdune&amp;rut=x">
          <b>Dune</b> &amp; its sequels
        </a>
        <a class="result__snippet">A desert planet, and the empire that wants it.</a>
      </div>
      <div class="result">
        <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Ftwo">Two</a>
        <a class="result__snippet">Second.</a>
      </div>"#;

    #[test]
    fn duckduckgo_results_are_unwrapped_from_the_redirect() {
        let hits = parse_duckduckgo(DDG);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Dune & its sequels", "entities should be decoded");
        assert_eq!(hits[0].url, "https://example.org/dune", "the redirect was not unwrapped");
        assert!(hits[0].summary.starts_with("A desert planet"));
        assert_eq!(hits[1].url, "https://example.org/two");
    }

    #[test]
    fn a_redesigned_results_page_yields_nothing_rather_than_rubbish() {
        // The failure mode that matters for a scraper: it must not return a
        // shelf full of navigation links when the markup changes.
        assert!(
            parse_duckduckgo("<html><body><a href=\"/about\">About</a></body></html>").is_empty()
        );
        assert!(parse_duckduckgo("").is_empty());
        assert!(parse_duckduckgo("class=\"result__a\"").is_empty());
    }
}
