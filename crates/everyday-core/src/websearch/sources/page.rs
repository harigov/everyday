use crate::library::normalize_rating;
use crate::websearch::{
    SearchResult, decode_entities, http_url, strip_tags, trim_summary, year_from_iso,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Most lines kept in a list fact. A recipe with more ingredients than this
/// is a page that has put its whole index in the structured data.
const MAX_LINES: usize = 120;
/// Longest single line kept, in characters.
const MAX_LINE_CHARS: usize = 1_000;
/// How deep the structured data is walked looking for a recipe. Real pages
/// nest it two or three levels down, in a `@graph` or a page's
/// `mainEntity`; a page nesting it forty deep is not one to go looking in.
const MAX_DEPTH: u8 = 8;

/// Read what a page says about itself into the result it was picked as.
///
/// A web page is two things to this app: the search engine's excerpt of it,
/// which is what a [`Source::Web`](crate::websearch::Source::Web) result
/// carries, and the page's own account of what it is, which is in its head
/// for exactly this purpose. Two accounts, in order of preference:
///
/// - **A recipe**, as schema.org structured data. Nearly every recipe on the
///   web carries one, because search engines draw recipe cards from it, and
///   it is the whole recipe: the ingredients, the method one step at a time,
///   how many it serves, how long it takes, a picture and the site's own
///   rating. A cocktail is a recipe too, which is how the Drinks shelf gets
///   one.
/// - **OpenGraph**, which almost every page has whatever it is: a name, a
///   sentence, a picture and the name of the site. It is what turns an
///   article into a card with a picture on it.
///
/// The page's own name and blurb *replace* the search's excerpt of them --
/// "Classic lasagne" over "Classic lasagne recipe | BBC Good Food" -- since
/// the excerpt was only ever somebody else's rendering of the same page.
/// Everything else fills gaps, as [`apply`](crate::websearch::apply) does.
///
/// Never fails. A page with nothing in its head leaves the result as it
/// was, which was already enough to put on a shelf.
pub(crate) fn merge_page(result: &mut SearchResult, html: &str) {
    let meta = meta_tags(html);
    let recipe = ld_json(html).iter().find_map(|block| find_typed(block, "Recipe", 0).cloned());
    let site = site_name(recipe.as_ref(), &meta, &result.url);

    let own_title = recipe
        .as_ref()
        .map(|r| clean(&str_at(r, "name")))
        .into_iter()
        .chain(["og:title", "twitter:title"].iter().map(|k| clean(meta_at(&meta, k))))
        .find(|t| !t.is_empty());
    match own_title {
        Some(title) => result.title = title,
        // A pasted address, or a result with no title at all: the page's
        // `<title>` is better than either, and only then.
        None if result.title.trim().is_empty() || is_address(&result.title) => {
            let title = clean(&title_tag(html));
            if !title.is_empty() {
                result.title = title;
            }
        }
        None => {}
    }

    let own_summary = recipe
        .as_ref()
        .map(|r| clean(&str_at(r, "description")))
        .into_iter()
        .chain(
            ["og:description", "twitter:description", "description"]
                .iter()
                .map(|k| clean(meta_at(&meta, k))),
        )
        .find(|s| !s.is_empty());
    if let Some(summary) = own_summary {
        result.summary = trim_summary(&summary);
    }

    let author = recipe
        .as_ref()
        .map(|r| names(r.get("author")))
        .into_iter()
        .chain(["author", "article:author"].iter().map(|k| clean(meta_at(&meta, k))))
        // `article:author` is often the address of the author's page.
        .find(|a| !a.is_empty() && !is_address(a));
    if let Some(author) = author {
        if result.creator.trim().is_empty() {
            result.creator = author.clone();
        }
        result.facts.entry("author".into()).or_insert(author);
    }

    if result.image_url.trim().is_empty() {
        let picture = recipe
            .as_ref()
            .and_then(|r| image(r.get("image")))
            .or_else(|| {
                ["og:image", "og:image:url", "og:image:secure_url", "twitter:image"]
                    .iter()
                    .map(|k| meta_at(&meta, k).trim().to_string())
                    .find(|u| !u.is_empty())
            })
            .and_then(|u| absolute(&result.url, &u));
        if let Some(picture) = picture {
            result.image_url = picture;
        }
    }

    // When it was published is a year worth having for an article. A
    // recipe's is the day somebody posted it, which says nothing about the
    // dish, so it is left out.
    if result.year.is_none() && recipe.is_none() {
        result.year = year_from_iso(meta_at(&meta, "article:published_time").trim());
    }

    if let Some(recipe) = &recipe {
        merge_recipe(result, recipe, &site);
    }

    let address = http_url(&result.url).unwrap_or_default();
    let mut fill = |key: &str, value: String| {
        if !value.trim().is_empty() {
            result.facts.entry(key.to_string()).or_insert(value);
        }
    };
    // Where it is from, under both of the names the seeded shelves use for
    // it: a recipe's "From", an article's "Publication".
    fill("source", site.clone());
    fill("publication", site);
    fill("url", address);
}

/// The recipe's own fields, keyed the way the Recipes and Drinks shelves
/// key theirs. [`apply`](crate::websearch::apply) drops whichever ones the
/// shelf at hand has no field for.
fn merge_recipe(result: &mut SearchResult, recipe: &Value, site: &str) {
    let mut fill = |key: &str, value: String| {
        if !value.trim().is_empty() {
            result.facts.entry(key.to_string()).or_insert(value);
        }
    };

    let mut ingredients = Vec::new();
    for key in ["recipeIngredient", "ingredients"] {
        if let Some(value) = recipe.get(key) {
            collect_lines(value, &mut ingredients);
        }
    }
    fill("ingredients", joined(ingredients));

    let mut method = Vec::new();
    if let Some(value) = recipe.get("recipeInstructions") {
        steps(value, &mut method, 0);
    }
    fill("method", joined(method));

    fill("serves", recipe.get("recipeYield").and_then(first_count).unwrap_or_default());
    let minutes = ["totalTime"]
        .iter()
        .find_map(|k| recipe.get(*k).and_then(Value::as_str).and_then(iso_minutes))
        .or_else(|| {
            let parts: Vec<u32> = ["prepTime", "cookTime", "performTime"]
                .iter()
                .filter_map(|k| recipe.get(*k).and_then(Value::as_str).and_then(iso_minutes))
                .collect();
            (!parts.is_empty()).then(|| parts.iter().sum())
        });
    fill("time", minutes.filter(|m| *m > 0).map(|m| m.to_string()).unwrap_or_default());
    fill("cuisine", listed(recipe.get("recipeCuisine")));
    fill("course", listed(recipe.get("recipeCategory")));
    fill("type", drink_type(recipe));

    if result.rating.is_none()
        && let Some(rating) = recipe.get("aggregateRating")
    {
        let best = number(rating.get("bestRating")).filter(|b| *b > 0.0).unwrap_or(5.0);
        if let Some(score) =
            number(rating.get("ratingValue")).and_then(|v| normalize_rating(v, best))
        {
            result.rating = Some(score);
            result.rating_count = ["ratingCount", "reviewCount"]
                .iter()
                .find_map(|k| number(rating.get(*k)))
                .filter(|n| *n >= 1.0)
                .map(|n| n.min(u32::MAX as f64) as u32);
            // The site's readers gave it, not "the web".
            result.rating_source = site.to_string();
        }
    }
}

/// "Cocktail" or "Mocktail", when the recipe says it is one -- the first
/// field on the Drinks shelf, which nothing else would fill.
fn drink_type(recipe: &Value) -> String {
    let said = [
        listed(recipe.get("recipeCategory")),
        listed(recipe.get("keywords")),
        str_at(recipe, "name"),
    ]
    .join(" ")
    .to_lowercase();
    let without_alcohol = ["mocktail", "non-alcoholic", "alcohol-free", "alcohol free", "virgin "];
    if without_alcohol.iter().any(|w| said.contains(w)) {
        "Mocktail".into()
    } else if said.contains("cocktail") {
        "Cocktail".into()
    } else {
        String::new()
    }
}

// ── Structured data ──────────────────────────────────────────────────────

/// Every `<script type="application/ld+json">` block on the page that
/// parses, in order.
fn ld_json(html: &str) -> Vec<Value> {
    // `to_ascii_lowercase` changes no byte's length, so an offset found in
    // the lowered copy is the same offset in the original.
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<script") {
        let open = from + at;
        let Some(gt) = lower[open..].find('>') else { break };
        let body = open + gt + 1;
        let Some(len) = lower[body..].find("</script") else { break };
        from = body + len;
        if !lower[open..body].contains("ld+json") {
            continue;
        }
        let raw = html[body..body + len]
            .trim()
            .trim_start_matches("<!--")
            .trim_end_matches("-->")
            .trim()
            .trim_start_matches("//<![CDATA[")
            .trim_end_matches("//]]>")
            .trim();
        match serde_json::from_str::<Value>(raw) {
            Ok(value) => out.push(value),
            // The commonest way a page's JSON is broken: a raw line break
            // inside a string, which a browser's own parser forgives and
            // serde does not. Outside a string it was only whitespace.
            Err(_) => {
                let flattened: String =
                    raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
                if let Ok(value) = serde_json::from_str::<Value>(&flattened) {
                    out.push(value);
                }
            }
        }
    }
    out
}

/// The first object of schema.org type `wanted`, wherever it is: at the top,
/// in a `@graph`, or inside the page that is about it.
fn find_typed<'a>(value: &'a Value, wanted: &str, depth: u8) -> Option<&'a Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Array(items) => items.iter().find_map(|v| find_typed(v, wanted, depth + 1)),
        Value::Object(map) => {
            if is_type(value.get("@type"), wanted) {
                return Some(value);
            }
            map.values().find_map(|v| find_typed(v, wanted, depth + 1))
        }
        _ => None,
    }
}

/// Whether a `@type` names `wanted`: as a bare word, in a list of types, or
/// spelled out in full as `http://schema.org/Recipe`.
fn is_type(value: Option<&Value>, wanted: &str) -> bool {
    let matches = |name: &str| {
        let name = name.rsplit(['/', ':', '#']).next().unwrap_or(name);
        name.eq_ignore_ascii_case(wanted)
    };
    match value {
        Some(Value::String(name)) => matches(name),
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).any(matches),
        _ => false,
    }
}

/// A recipe's method, one step to a line, however the page wrote it: a
/// paragraph, a list of strings, a list of steps, or steps in named
/// sections. A section's name becomes a line of its own -- "For the
/// sauce:" -- so a method in two parts still reads as two parts.
fn steps(value: &Value, out: &mut Vec<String>, depth: u8) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::String(text) => out.extend(lines(text)),
        Value::Array(items) => items.iter().for_each(|v| steps(v, out, depth + 1)),
        Value::Object(_) => {
            let name = clean(&str_at(value, "name"));
            if is_type(value.get("@type"), "HowToSection") {
                if !name.is_empty() {
                    out.push(if name.ends_with(':') { name } else { format!("{name}:") });
                }
                if let Some(list) = value.get("itemListElement") {
                    steps(list, out, depth + 1);
                }
            } else if let Some(text) = value.get("text").and_then(Value::as_str) {
                out.extend(lines(text));
            } else if let Some(list) = value.get("itemListElement") {
                steps(list, out, depth + 1);
            } else if !name.is_empty() {
                out.push(name);
            }
        }
        _ => {}
    }
}

/// Every line of text in a string or a list of strings.
fn collect_lines(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.extend(lines(text)),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    out.extend(lines(text));
                }
            }
        }
        _ => {}
    }
}

/// A run of HTML or text as the lines a person would read: a paragraph, a
/// list item and a line break each end one.
fn lines(text: &str) -> Vec<String> {
    let mut broken = String::with_capacity(text.len());
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find('<') {
        let at = from + at;
        let tag = &lower[at..];
        let ends_line =
            ["<br", "</p", "</li", "</div", "<p>", "<p ", "<li"].iter().any(|t| tag.starts_with(t));
        broken.push_str(&text[from..at]);
        if ends_line {
            broken.push('\n');
        }
        broken.push('<');
        from = at + 1;
    }
    broken.push_str(&text[from..]);
    strip_tags(&broken)
        .lines()
        .map(clean)
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(MAX_LINE_CHARS).collect())
        .collect()
}

fn joined(lines: Vec<String>) -> String {
    lines.into_iter().take(MAX_LINES).collect::<Vec<_>>().join("\n")
}

/// Entities decoded, markup removed and whitespace collapsed to single
/// spaces: one line of somebody else's text, tidied.
fn clean(text: &str) -> String {
    decode_entities(&strip_tags(&decode_entities(text)))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn str_at(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A string or a list of them, as one line: "Italian, Vegetarian".
fn listed(value: Option<&Value>) -> String {
    let words: Vec<String> = match value {
        Some(Value::String(s)) => vec![clean(s)],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(clean).collect(),
        _ => Vec::new(),
    };
    let mut seen = std::collections::BTreeSet::new();
    words
        .into_iter()
        .filter(|w| !w.is_empty() && seen.insert(w.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Who, from a schema.org person or organisation, a list of them, or a
/// plain name. The first three; a recipe credited to a whole kitchen is
/// credited to its first few.
fn names(value: Option<&Value>) -> String {
    let one = |v: &Value| match v {
        Value::String(s) => clean(s),
        Value::Object(_) => clean(&str_at(v, "name")),
        _ => String::new(),
    };
    let all: Vec<String> = match value {
        Some(Value::Array(items)) => items.iter().map(one).collect(),
        Some(v) => vec![one(v)],
        None => Vec::new(),
    };
    all.into_iter().filter(|n| !n.is_empty()).take(3).collect::<Vec<_>>().join(", ")
}

/// The picture worth having, from an address, an `ImageObject` or a list
/// of either.
///
/// A recipe usually lists the same photograph several times over, in the
/// shapes search engines ask for, and often smallest first -- a 225-pixel
/// square, then 260 by 195, then the original. The first is a thumbnail, so
/// the choice is by width: the smallest at least [`GOOD_WIDTH`] across,
/// which is sharp on a card without being a poster; failing that one whose
/// size is not stated, which is usually the original; failing that the
/// widest there is.
fn image(value: Option<&Value>) -> Option<String> {
    let mut found = Vec::new();
    images(value?, &mut found, 0);
    let rank = |(_, width): &(String, Option<u32>)| match width {
        Some(w) if *w >= GOOD_WIDTH => (2, u32::MAX - w),
        None => (1, 0),
        Some(w) => (0, *w),
    };
    // `max_by_key` keeps the last of equals, and the page's own first
    // choice should win a tie.
    found.into_iter().rev().max_by_key(rank).map(|(url, _)| url)
}

/// Wide enough for a cover on a high-density screen.
const GOOD_WIDTH: u32 = 600;

fn images(value: &Value, out: &mut Vec<(String, Option<u32>)>, depth: u8) {
    if depth > 2 {
        return;
    }
    match value {
        Value::String(url) if !url.trim().is_empty() => {
            out.push((url.trim().to_string(), width_in_name(url)))
        }
        Value::Array(items) => items.iter().for_each(|v| images(v, out, depth + 1)),
        Value::Object(_) => {
            let url = ["url", "contentUrl"]
                .iter()
                .map(|k| str_at(value, k).trim().to_string())
                .find(|u| !u.is_empty());
            if let Some(url) = url {
                let width = number(value.get("width")).map(|w| w as u32);
                let width = width.or_else(|| width_in_name(&url));
                out.push((url, width));
            }
        }
        _ => {}
    }
}

/// The width a resized copy carries in its name -- `lentil-soup-225x225.jpg`
/// -- the way most publishing software names its thumbnails.
fn width_in_name(url: &str) -> Option<u32> {
    let path = url.split(['?', '#']).next()?;
    let file = path.rsplit('/').next()?;
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let (width, height) = stem.rsplit('-').next()?.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 5 && s.chars().all(|c| c.is_ascii_digit());
    (digits(width) && digits(height)).then(|| width.parse().ok()).flatten()
}

/// A number, whether the page wrote it as one or as a string -- and with a
/// decimal comma, as half of Europe does.
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// How many it serves: the first whole number in "4 servings", "Serves
/// 4-6" or "12 biscuits", or the number itself.
fn first_count(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => n.as_u64().filter(|n| *n > 0).map(|n| n.to_string()),
        Value::String(s) => {
            let digits: String = s
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u32>().ok().filter(|n| *n > 0).map(|n| n.to_string())
        }
        Value::Array(items) => items.iter().find_map(first_count),
        _ => None,
    }
}

/// An ISO 8601 duration -- `PT1H30M`, `P0DT45M` -- in whole minutes.
fn iso_minutes(text: &str) -> Option<u32> {
    let rest = text.trim().strip_prefix(['P', 'p'])?;
    let (mut minutes, mut in_time, mut number, mut any) = (0f64, false, String::new(), false);
    for c in rest.chars() {
        match c.to_ascii_uppercase() {
            'T' => in_time = true,
            c if c.is_ascii_digit() || c == '.' || c == ',' => {
                number.push(if c == ',' { '.' } else { c })
            }
            unit => {
                let n: f64 = std::mem::take(&mut number).parse().ok()?;
                minutes += match (unit, in_time) {
                    ('D', false) => n * 24.0 * 60.0,
                    ('W', false) => n * 7.0 * 24.0 * 60.0,
                    ('H', true) => n * 60.0,
                    ('M', true) => n,
                    ('S', true) => n / 60.0,
                    // Months and years: a dish that takes those is not one
                    // with a cooking time worth recording.
                    _ => return None,
                };
                any = true;
            }
        }
    }
    (any && number.is_empty()).then_some(minutes.round().min(f64::from(u32::MAX)) as u32)
}

// ── The head ─────────────────────────────────────────────────────────────

/// `<meta>` tags, by `property` or `name`, lower-cased; the first of each
/// wins, which is the one a page put in its head rather than its body.
fn meta_tags(html: &str) -> BTreeMap<String, String> {
    let lower = html.to_ascii_lowercase();
    let mut out = BTreeMap::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let open = from + at + "<meta".len();
        let Some(len) = lower[open..].find('>') else { break };
        from = open + len;
        let attrs = attributes(&html[open..open + len]);
        let key = attrs
            .iter()
            .find(|(k, _)| k == "property" || k == "name")
            .map(|(_, v)| v.trim().to_ascii_lowercase());
        let content = attrs.iter().find(|(k, _)| k == "content").map(|(_, v)| v.clone());
        if let (Some(key), Some(content)) = (key, content)
            && !key.is_empty()
        {
            out.entry(key).or_insert(content);
        }
    }
    out
}

fn meta_at<'a>(meta: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    meta.get(key).map(String::as_str).unwrap_or_default()
}

/// The attributes inside one opening tag, names lower-cased, values as
/// written: double-quoted, single-quoted or bare.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            break;
        }
        let name_len =
            rest.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(rest.len());
        let name = rest[..name_len].to_ascii_lowercase();
        rest = rest[name_len..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let inner = &after[1..];
                    let end = inner.find(quote).unwrap_or(inner.len());
                    value = inner[..end].to_string();
                    rest = inner.get(end + 1..).unwrap_or_default();
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    value = after[..end].to_string();
                    rest = &after[end..];
                }
            }
        }
        if name.is_empty() {
            // Not an attribute at all; step past the character that stopped
            // the name, or this would never end.
            rest = rest.get(1..).unwrap_or_default();
            continue;
        }
        out.push((name, decode_entities(&value)));
    }
    out
}

/// The page's `<title>`, as written.
fn title_tag(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<title") {
        let open = from + at;
        from = open + "<title".len();
        // `<title>` or `<title lang=…>`, not `<titles>`.
        if !matches!(lower.as_bytes().get(from), Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')) {
            continue;
        }
        let Some(gt) = lower[open..].find('>') else { break };
        let start = open + gt + 1;
        let Some(len) = lower[start..].find("</title") else { break };
        return html[start..start + len].to_string();
    }
    String::new()
}

/// Who published it: the recipe's publisher, the page's site name, or
/// failing both the site's own address.
fn site_name(recipe: Option<&Value>, meta: &BTreeMap<String, String>, url: &str) -> String {
    recipe
        .map(|r| names(r.get("publisher")))
        .filter(|n| !n.is_empty())
        .or_else(|| Some(clean(meta_at(meta, "og:site_name"))).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| host(url).trim_start_matches("www.").to_string())
}

/// The host of an address, or nothing.
fn host(url: &str) -> &str {
    let Some((_, rest)) = url.split_once("://") else { return "" };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    }
}

/// `href` as an absolute web address, read relative to the page at `base`.
/// Pages are meant to give absolute addresses here and most do; the ones
/// that do not give `//cdn…` or `/images/…`.
fn absolute(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if let Ok(url) = http_url(href) {
        return Some(url);
    }
    let scheme = base.split_once("://").map(|(s, _)| s.to_ascii_lowercase())?;
    if let Some(rest) = href.strip_prefix("//") {
        return http_url(&format!("{scheme}://{rest}")).ok();
    }
    if href.starts_with('/') {
        let host = host(base);
        if host.is_empty() {
            return None;
        }
        return http_url(&format!("{scheme}://{host}{href}")).ok();
    }
    None
}

/// Is this nothing but a web address?
pub(crate) fn is_address(text: &str) -> bool {
    let text = text.trim();
    !text.contains(char::is_whitespace)
        && http_url(text).is_ok()
        && text
            .split_once("://")
            .is_some_and(|(_, rest)| !host(text).is_empty() && !rest.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A recipe page the way the big recipe sites write one: the recipe in a
    /// `@graph` beside the page and the site, the method in a named
    /// section, the yield as prose and the rating as strings.
    pub(crate) const RECIPE_PAGE: &str = r#"<!doctype html>
<html><head>
<title>Classic lasagne recipe | Good Food</title>
<meta property="og:title" content="Classic lasagne recipe">
<meta property="og:site_name" content="Good Food">
<meta name="description" content="A lasagne for a crowd.">
<script type="application/ld+json">
{"@context":"https://schema.org","@graph":[
  {"@type":"WebPage","name":"Classic lasagne recipe"},
  {"@type":["Recipe","NewsArticle"],
   "name":"Classic lasagne",
   "description":"Layers of rag&ugrave; and b&eacute;chamel &amp; a crisp top.",
   "author":[{"@type":"Person","name":"Cassie Best"}],
   "publisher":{"@type":"Organization","name":"BBC Good Food"},
   "image":[{"@type":"ImageObject","url":"https://images.example/lasagne.jpg"}],
   "recipeYield":"Serves 6 - 8",
   "prepTime":"PT20M","cookTime":"PT1H45M",
   "recipeCuisine":"Italian",
   "recipeCategory":["Dinner","Main course"],
   "recipeIngredient":["1 tbsp <b>olive oil</b>","2 onions, finely chopped","  500g   beef mince "],
   "recipeInstructions":[
     {"@type":"HowToSection","name":"For the ragù","itemListElement":[
        {"@type":"HowToStep","text":"<p>Heat the oil.</p><p>Fry the onions.</p>"},
        {"@type":"HowToStep","text":"Add the mince and brown it."}]},
     {"@type":"HowToStep","text":"Layer it up and bake for 45 mins."}],
   "aggregateRating":{"@type":"AggregateRating","ratingValue":"4,6","ratingCount":"1204"}}
]}
</script>
</head><body>…</body></html>"#;

    fn picked(title: &str, url: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            summary: "Make our easy lasagne recipe for dinner…".into(),
            source: "web".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_recipe_page_gives_the_whole_recipe() {
        let mut hit = picked(
            "Classic lasagne recipe | Good Food",
            "https://www.bbcgoodfood.com/recipes/classic-lasagne",
        );
        merge_page(&mut hit, RECIPE_PAGE);

        assert_eq!(hit.title, "Classic lasagne", "the page's own name, not the listing's");
        assert_eq!(hit.creator, "Cassie Best");
        assert!(hit.summary.starts_with("Layers of rag"), "{}", hit.summary);
        assert!(hit.summary.contains("& a crisp top"), "entities decoded: {}", hit.summary);
        assert_eq!(hit.image_url, "https://images.example/lasagne.jpg");
        assert_eq!(
            hit.facts["ingredients"],
            "1 tbsp olive oil\n2 onions, finely chopped\n500g beef mince"
        );
        assert_eq!(
            hit.facts["method"],
            "For the ragù:\nHeat the oil.\nFry the onions.\nAdd the mince and brown it.\n\
             Layer it up and bake for 45 mins."
        );
        assert_eq!(hit.facts["serves"], "6");
        assert_eq!(hit.facts["time"], "125", "prep and cook added when there is no total");
        assert_eq!(hit.facts["cuisine"], "Italian");
        assert_eq!(hit.facts["course"], "Dinner, Main course");
        assert_eq!(hit.facts["source"], "BBC Good Food", "the publisher over the site name");
        assert_eq!(hit.facts["url"], "https://www.bbcgoodfood.com/recipes/classic-lasagne");
        assert!(!hit.facts.contains_key("type"), "a lasagne is not a drink");
        // 4.6 of 5, with a decimal comma, credited to the site's readers.
        assert_eq!(hit.rating, Some(92));
        assert_eq!(hit.rating_count, Some(1204));
        assert_eq!(hit.rating_source, "BBC Good Food");
    }

    #[test]
    fn a_cocktail_is_a_recipe_and_says_which_kind_of_drink_it_is() {
        let page = r#"<script type="application/ld+json">
            {"@type":"Recipe","name":"Negroni","recipeCategory":"Cocktail",
             "recipeIngredient":["25ml gin","25ml Campari","25ml sweet vermouth"],
             "recipeInstructions":"Stir with ice.\nStrain over a large cube.",
             "totalTime":"PT3M","recipeYield":1}</script>"#;
        let mut hit = picked("Negroni", "https://drinks.example/negroni");
        merge_page(&mut hit, page);
        assert_eq!(hit.facts["type"], "Cocktail");
        assert_eq!(hit.facts["method"], "Stir with ice.\nStrain over a large cube.");
        assert_eq!(hit.facts["time"], "3");
        assert_eq!(hit.facts["serves"], "1");

        let mut virgin = picked("Nojito", "https://drinks.example/nojito");
        merge_page(
            &mut virgin,
            r#"<script type="application/ld+json">{"@type":"Recipe","name":"Nojito",
               "keywords":"mojito, cocktail, alcohol-free"}</script>"#,
        );
        assert_eq!(virgin.facts["type"], "Mocktail", "alcohol-free outranks cocktail");
    }

    #[test]
    fn a_page_without_a_recipe_still_has_a_name_a_picture_and_a_site() {
        let page = r#"<html><head>
            <meta property="og:title" content="Why we cook">
            <meta content="An essay." property="og:description" />
            <meta property='og:image' content='/img/hero.jpg'>
            <meta property="og:site_name" content="The Paper">
            <meta name="author" content="A. Writer">
            <meta property="article:published_time" content="2024-03-01T09:00:00Z">
            </head></html>"#;
        let mut hit = picked("Why we cook - The Paper", "https://paper.example/2024/cook");
        merge_page(&mut hit, page);
        assert_eq!(hit.title, "Why we cook");
        assert_eq!(hit.summary, "An essay.");
        assert_eq!(hit.image_url, "https://paper.example/img/hero.jpg", "made absolute");
        assert_eq!(hit.creator, "A. Writer");
        assert_eq!(hit.year, Some(2024));
        assert_eq!(hit.facts["publication"], "The Paper");
        assert_eq!(hit.facts["author"], "A. Writer");
        assert!(!hit.facts.contains_key("ingredients"));
    }

    #[test]
    fn a_pasted_address_takes_the_pages_title_when_that_is_all_there_is() {
        let mut hit = picked("https://plain.example/page", "https://plain.example/page");
        merge_page(
            &mut hit,
            "<html><head><title>  A   plain page &amp; more </title></head></html>",
        );
        assert_eq!(hit.title, "A plain page & more");
        assert_eq!(hit.facts["source"], "plain.example", "the host, when nothing names the site");

        // A real title from a search survives a page that has only a
        // `<title>`: that is what the search listed in the first place.
        let mut listed = picked("Listed title", "https://plain.example/page");
        merge_page(&mut listed, "<title>Other</title>");
        assert_eq!(listed.title, "Listed title");
    }

    #[test]
    fn a_page_with_broken_or_no_structured_data_is_left_as_it_was() {
        for page in [
            "",
            "<script type=\"application/ld+json\">{not json</script>",
            "<script type=\"application/ld+json\">",
            "<meta property=\"og:title\"",
            "<title>",
            "<meta =>",
            r#"<script type="application/ld+json">{"@type":"Recipe","recipeInstructions":[[[[[[[[[[["deep"]]]]]]]]]]]}</script>"#,
        ] {
            let mut hit = picked("Kept", "https://x.example/");
            merge_page(&mut hit, page);
            assert_eq!(hit.title, "Kept", "{page:?}");
        }
    }

    #[test]
    fn a_raw_line_break_inside_a_string_does_not_lose_the_recipe() {
        let page = "<script type=\"application/ld+json\">{\"@type\":\"Recipe\",\"name\":\"Soda\nbread\"}</script>";
        let mut hit = picked("x", "https://x.example/");
        merge_page(&mut hit, page);
        assert_eq!(hit.title, "Soda bread");
    }

    #[test]
    fn the_picture_is_a_sharp_copy_and_not_the_first_thumbnail() {
        let pick = |json: &str| image(Some(&serde_json::from_str::<Value>(json).unwrap()));
        // Smallest first, original last: the original.
        assert_eq!(
            pick(
                r#"["https://x.example/soup-225x225.jpg","https://x.example/soup-320x180.jpg","https://x.example/soup.jpg"]"#
            ),
            Some("https://x.example/soup.jpg".into())
        );
        // Stated widths: the smallest that is wide enough.
        assert_eq!(
            pick(
                r#"[{"url":"https://x.example/a.jpg","width":1920},{"url":"https://x.example/b.jpg","width":768},{"url":"https://x.example/c.jpg","width":300}]"#
            ),
            Some("https://x.example/b.jpg".into())
        );
        // Nothing wide enough and nothing unsized: the widest.
        assert_eq!(
            pick(r#"["https://x.example/s-100x100.jpg","https://x.example/s-320x180.jpg"]"#),
            Some("https://x.example/s-320x180.jpg".into())
        );
        // All alike: the page's own first choice.
        assert_eq!(
            pick(r#"["https://x.example/one.jpg","https://x.example/two.jpg"]"#),
            Some("https://x.example/one.jpg".into())
        );
        assert_eq!(
            width_in_name("https://x.example/2019/best-soup-4-1200x800.jpg?v=2"),
            Some(1200)
        );
        assert_eq!(width_in_name("https://x.example/soup-x.jpg"), None);
        assert_eq!(pick(r#"[]"#), None);
    }

    #[test]
    fn durations_read_in_minutes() {
        assert_eq!(iso_minutes("PT1H30M"), Some(90));
        assert_eq!(iso_minutes("P0DT45M"), Some(45));
        assert_eq!(iso_minutes("PT90S"), Some(2));
        assert_eq!(iso_minutes("P1D"), Some(1440));
        assert_eq!(iso_minutes("pt0.5h"), Some(30));
        for nonsense in ["", "P", "PT", "45 minutes", "PT1H30", "P1M", "PTxM"] {
            assert_eq!(iso_minutes(nonsense), None, "{nonsense:?}");
        }
    }

    #[test]
    fn only_a_whole_address_is_an_address() {
        assert!(is_address("https://example.com/recipe"));
        assert!(is_address("  http://example.com "));
        assert!(!is_address("https://"));
        assert!(!is_address("https:///path"));
        assert!(!is_address("lasagne https://example.com"));
        assert!(!is_address("example.com/recipe"));
        assert!(!is_address("ftp://example.com"));
    }
}
