//! FiveFilters' site rules: for nearly two thousand sites, where the article
//! is on the page and what around it to throw away -- the knowledge a
//! read-it-later service builds up site by site, which Readability's
//! heuristics on their own do not have.
//!
//! The rules are FiveFilters' own (https://github.com/fivefilters/ftr-site-config,
//! released to the public domain), bundled in `site_rules.txt` by
//! `scripts/site-rules.mjs`, which names the commit they came from. Only
//! these directives are kept, and read:
//!
//! * `find_string` / `replace_string` (and `replace_string(find): replace`):
//!   text replaced in the page before it is parsed.
//! * `strip`, `strip_id_or_class`, `strip_image_src`: what to take out.
//! * `body`: where the article is, as alternatives tried in order.
//! * `author`: who wrote it.
//!
//! The rules for a site are its own file's and `global.txt`'s together. A
//! site's own rules come first: its find-and-replace, then the global one.
//! Paths are XPath, translated by [`super::xpath`]; one that cannot be
//! translated is left out, as is a selector the HTML reader refuses.
//!
//! What is not read: titles and dates (a feed has its own), multi-page
//! articles and logins (they would mean fetching more than the one page),
//! and the switches that only mean something to FiveFilters' own extractor.
//!
//! The strip rules are applied to the whole page whether or not the site has
//! a body rule, since they help Readability as much as they help a rule.

use super::xpath::{self, Read};
use dom_query::{Document, Matcher, NodeId};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// The rules, as `scripts/site-rules.mjs` bundled them.
const BUNDLE: &str = include_str!("site_rules.txt");

/// The section of rules for every site.
const GLOBAL: &str = "global";

/// One site's rules, or the global ones.
#[derive(Debug, Default)]
struct SiteRules {
    replace: Vec<(String, String)>,
    strip: Vec<String>,
    body: Vec<Vec<String>>,
    author: Vec<xpath::Css>,
}

/// What a site's rules made of a page: the page with what they strip taken
/// out, for Readability; the article, when a body rule found one; and who
/// wrote it, when an author rule said.
#[derive(Debug)]
pub struct Prepared {
    pub page: String,
    pub article: Option<Article>,
    pub byline: Option<String>,
}

/// The article a body rule found.
#[derive(Debug)]
pub struct Article {
    pub html: String,
    /// How many characters of text it has, for weighing it against
    /// Readability's: see [`holds_up`].
    pub text_chars: usize,
}

/// Every site's rules by the name of its file: `example.com`, or
/// `.example.com` for the site and every name under it. Read from the bundle
/// the first time an article is.
fn sites() -> &'static HashMap<String, SiteRules> {
    static SITES: OnceLock<HashMap<String, SiteRules>> = OnceLock::new();
    SITES.get_or_init(|| parse_bundle(BUNDLE))
}

fn parse_bundle(bundle: &str) -> HashMap<String, SiteRules> {
    let mut sites = HashMap::new();
    let mut current: Option<(String, SiteRules, Vec<String>)> = None;
    let finish = |sites: &mut HashMap<String, SiteRules>,
                  site: Option<(String, SiteRules, Vec<String>)>| {
        if let Some((name, rules, _)) = site {
            sites.insert(name, rules);
        }
    };
    for line in bundle.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("== ") {
            finish(&mut sites, current.take());
            current = Some((name.trim().to_ascii_lowercase(), SiteRules::default(), Vec::new()));
            continue;
        }
        let Some((_, rules, finds)) = current.as_mut() else { continue };
        read_directive(rules, finds, line);
    }
    finish(&mut sites, current);
    sites
}

/// One `directive: value` line into `rules`. `finds` holds the
/// `find_string`s still waiting for their `replace_string`, which pair up in
/// order.
fn read_directive(rules: &mut SiteRules, finds: &mut Vec<String>, line: &str) {
    if let Some(rest) = line.strip_prefix("replace_string(") {
        // `replace_string(find): replace`, the pair in one line.
        if let Some((find, replace)) = rest.split_once("):")
            && !find.is_empty()
        {
            rules.replace.push((find.to_string(), replace.trim_start().to_string()));
        }
        return;
    }
    let Some((directive, value)) = line.split_once(':') else { return };
    let value = value.trim();
    match directive.trim() {
        "find_string" => finds.push(value.to_string()),
        "replace_string" => {
            if !finds.is_empty() {
                let find = finds.remove(0);
                if !find.is_empty() {
                    rules.replace.push((find, value.to_string()));
                }
            }
        }
        "strip" => {
            if let Some(parts) = elements(value) {
                rules.strip.extend(parts);
            }
        }
        // Quotes around these values are not part of them: FiveFilters takes
        // them out before it searches, and a hundred rules are written so.
        "strip_id_or_class" => {
            let value = value.replace(['\'', '"'], "");
            if !value.is_empty() {
                let needle = css_string(&value);
                rules.strip.push(format!("[class*={needle}], [id*={needle}]"));
            }
        }
        "strip_image_src" => {
            let value = value.replace(['\'', '"'], "");
            if !value.is_empty() {
                rules.strip.push(format!("img[src*={}]", css_string(&value)));
            }
        }
        "body" => {
            if let Some(parts) = elements(value) {
                rules.body.push(parts);
            }
        }
        "author" => {
            if let Some(parts) = xpath::to_css(value) {
                rules.author.extend(parts);
            }
        }
        _ => {}
    }
}

/// The selectors for a path that names elements, or `None` when it names a
/// value or cannot be translated.
fn elements(xpath: &str) -> Option<Vec<String>> {
    let parts = xpath::to_css(xpath)?;
    parts.into_iter().map(|css| (css.read == Read::Element).then_some(css.selector)).collect()
}

/// A CSS string holding `value`.
fn css_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " "))
}

/// The names a site's rules may be filed under, most particular first: the
/// host as it is, without `www.`, with it, then as a wildcard for it and for
/// each name above it.
fn candidates(host: &str) -> Vec<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let bare = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let mut names = vec![host.clone(), bare.clone(), format!("www.{bare}")];
    let mut rest = bare.as_str();
    while rest.contains('.') {
        names.push(format!(".{rest}"));
        rest = rest.split_once('.').map_or("", |(_, after)| after);
    }
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

/// The rules filed for `host`, if any.
fn site_for(host: &str) -> Option<&'static SiteRules> {
    candidates(host).iter().find_map(|name| sites().get(name))
}

/// What the rules for `page_url`'s site, and the global ones, make of `html`.
pub fn prepare(html: &str, page_url: &str) -> Prepared {
    let host = reqwest::Url::parse(page_url).ok().and_then(|u| u.host_str().map(str::to_string));
    let site = host.as_deref().and_then(site_for);
    prepare_with(html, site, sites().get(GLOBAL))
}

fn prepare_with(html: &str, site: Option<&SiteRules>, global: Option<&SiteRules>) -> Prepared {
    let all = || site.into_iter().chain(global);

    let mut page = html.to_string();
    for (find, replace) in all().flat_map(|rules| &rules.replace) {
        if page.contains(find.as_str()) {
            page = page.replace(find.as_str(), replace);
        }
    }

    let doc = Document::from(page.as_str());
    // Who wrote it is read before anything is stripped: a byline is often in
    // the furniture a strip rule takes out.
    let byline = site.and_then(|rules| rules.author.iter().find_map(|css| read_value(&doc, css)));

    for selector in all().flat_map(|rules| &rules.strip) {
        if let Some(matcher) = matcher(selector) {
            doc.select_matcher(&matcher).remove();
        }
    }

    let article = site.and_then(|rules| rules.body.iter().find_map(|parts| body(&doc, parts)));
    Prepared { page: doc.html().to_string(), article, byline }
}

fn matcher(selector: &str) -> Option<Matcher> {
    Matcher::new(selector).ok()
}

/// The article one `body` rule finds: every element its paths select, in
/// the page's order and each once (an element inside another selected one
/// is already in it), tidied the way Readability tidies what it keeps
/// ([`tidy`]), or `None` when that leaves no text.
fn body(doc: &Document, parts: &[String]) -> Option<Article> {
    let matcher = matcher(&parts.join(", "))?;
    let found = doc.select_matcher(&matcher);
    let selected: HashSet<NodeId> = found.nodes().iter().map(|node| node.id).collect();
    let outer: Vec<String> = found
        .nodes()
        .iter()
        .filter(|node| !node.ancestors_it(None).any(|ancestor| selected.contains(&ancestor.id)))
        .map(|node| node.html().to_string())
        .collect();
    if outer.is_empty() {
        return None;
    }
    let article = Document::fragment(outer.join("\n"));
    tidy(&article);
    let text_chars = article.text().split_whitespace().map(str::len).sum();
    if text_chars == 0 {
        return None;
    }
    Some(Article { html: article.html_root().inner_html().to_string(), text_chars })
}

/// Where a page keeps a lazily loaded picture's address until a script moves
/// it into `src`.
const LAZY_SOURCES: [&str; 4] = ["data-src", "data-lazy-src", "data-original", "data-url"];

/// What Readability does to the article it keeps and a rule's would not get
/// otherwise: a lazily loaded picture given its real address, a `<noscript>`
/// picture put where a script would have put it (or dropped when the picture
/// is there already), and what only a script or a stylesheet reads --
/// `<script>`, `<style>`, `<template>` and the like -- taken out. A style
/// left in would set the article's type over the reader's.
fn tidy(doc: &Document) {
    for image in doc.select("img, source").nodes().to_vec() {
        let src = image.attr("src").map(|s| s.trim().to_string()).unwrap_or_default();
        if image.node_name().as_deref() == Some("img")
            && (src.is_empty() || src.starts_with("data:"))
        {
            let real = LAZY_SOURCES
                .iter()
                .find_map(|name| image.attr(name).filter(|v| !v.trim().is_empty()));
            if let Some(real) = real {
                image.set_attr("src", real.trim());
            }
        }
        if !image.has_attr("srcset")
            && let Some(set) = image.attr("data-srcset").filter(|v| !v.trim().is_empty())
        {
            image.set_attr("srcset", set.trim());
        }
    }
    for noscript in doc.select("noscript").nodes().to_vec() {
        let inner = noscript.inner_html().to_string();
        if !inner.to_ascii_lowercase().contains("<img") {
            noscript.remove_from_parent();
            continue;
        }
        let before =
            noscript.prev_element_sibling().filter(|n| n.node_name().as_deref() == Some("img"));
        match before {
            Some(image) if is_placeholder(&image) => {
                image.replace_with_html(inner);
                noscript.remove_from_parent();
            }
            Some(_) => noscript.remove_from_parent(),
            None => noscript.replace_with_html(inner),
        }
    }
    doc.select("script, style, template, link, meta, object, embed").remove();
}

/// A picture with no address of its own yet: none, or a `data:` stand-in.
fn is_placeholder(image: &dom_query::NodeRef) -> bool {
    let src = image.attr("src").map(|s| s.trim().to_string()).unwrap_or_default();
    src.is_empty() || src.starts_with("data:")
}

/// What an author rule reads from the first element it selects: an
/// attribute or the text, on one line.
fn read_value(doc: &Document, css: &xpath::Css) -> Option<String> {
    let matcher = matcher(&css.selector)?;
    let found = doc.select_matcher(&matcher);
    let node = found.nodes().first()?;
    let raw = match &css.read {
        Read::Attribute(name) => node.attr(name)?.to_string(),
        Read::Element | Read::Text => node.text().to_string(),
    };
    let value = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

/// Whether a body rule's article is the one to keep over Readability's,
/// which has `readability_chars` of text: when it has a third of that or
/// more. A rule often leaves out what Readability keeps -- comments, a
/// related list -- but one that finds a fraction of the page is a rule the
/// site has outgrown, matching a teaser or a caption.
pub fn holds_up(rule_chars: usize, readability_chars: usize) -> bool {
    rule_chars > 0 && rule_chars * 3 >= readability_chars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> SiteRules {
        let mut sites = parse_bundle(&format!("== test\n{text}"));
        sites.remove("test").unwrap()
    }

    #[test]
    fn a_site_is_found_under_its_own_name_or_a_wildcard_above_it() {
        assert_eq!(
            candidates("WWW.News.Example.com."),
            ["www.news.example.com", "news.example.com", ".news.example.com", ".example.com"]
        );
        // Never the bare top-level domain: no site's rules are every `.com`'s.
        assert_eq!(candidates("example.com"), ["example.com", "www.example.com", ".example.com"]);
    }

    #[test]
    fn find_and_replace_pair_up_in_order_and_in_one_line() {
        let r = rules(
            "find_string: <amp-img\nfind_string: </amp-img>\nreplace_string: <img\n\
             replace_string: <!-- -->\nreplace_string(noscript>): div>\nreplace_string: orphan",
        );
        assert_eq!(
            r.replace,
            [
                ("<amp-img".to_string(), "<img".to_string()),
                ("</amp-img>".to_string(), "<!-- -->".to_string()),
                ("noscript>".to_string(), "div>".to_string()),
            ]
        );
    }

    #[test]
    fn quotes_around_a_strip_value_are_not_part_of_it() {
        let r = rules(
            "strip_id_or_class: 'currents'\nstrip_image_src: \"pixel.gif\"\nstrip_id_or_class: ''",
        );
        assert_eq!(
            r.strip,
            ["[class*=\"currents\"], [id*=\"currents\"]", "img[src*=\"pixel.gif\"]"]
        );
    }

    #[test]
    fn a_rules_article_is_tidied_as_readability_tidies_its_own() {
        let page = r#"<html><body><div id="story">
            <style>p { font-size: 16px }</style>
            <p>The words of the story, long enough to be an article on their own.</p>
            <img src="data:image/gif;base64,R0lGOD" data-src="https://cdn.example/real.jpg">
            <noscript><img src="https://cdn.example/real.jpg"></noscript>
            <img src="data:image/gif;base64,R0lGOD">
            <noscript><img src="https://cdn.example/only-here.jpg"></noscript>
            <noscript><p>Turn on JavaScript</p></noscript>
            <script>window.__DATA__ = {"a": 1}</script>
          </div></body></html>"#;
        let site = rules("body: //div[@id='story']");
        let article = prepare_with(page, Some(&site), None).article.unwrap();
        assert!(!article.html.contains("<style"), "{}", article.html);
        assert!(!article.html.contains("<script"), "{}", article.html);
        assert!(!article.html.contains("noscript"), "{}", article.html);
        assert!(!article.html.contains("JavaScript"), "{}", article.html);
        // The lazy picture given its address, its noscript copy dropped, and
        // the picture only a noscript had put in its stand-in's place.
        assert_eq!(article.html.matches("<img").count(), 2, "{}", article.html);
        assert!(
            article.html.contains("<img src=\"https://cdn.example/real.jpg\""),
            "{}",
            article.html
        );
        assert!(
            article.html.contains("src=\"https://cdn.example/only-here.jpg\""),
            "{}",
            article.html
        );
        assert!(!article.html.contains("data:image"), "{}", article.html);
    }

    #[test]
    fn a_container_holding_only_a_script_is_no_article() {
        let page = r#"<html><body><div id="story"><script>window.__DATA__ = {"text": "a great deal of it"}</script></div>
            <article><p>Where the words went.</p></article></body></html>"#;
        let site = rules("body: //div[@id='story']");
        assert!(prepare_with(page, Some(&site), None).article.is_none());
    }

    #[test]
    fn a_rule_that_cannot_be_translated_is_left_out() {
        let r = rules(
            "body: //h2[.='Details']/following-sibling::div\nbody: //article\n\
             author: //meta[@name='author']/@content\nstrip: //p[last()]\nstrip: //aside",
        );
        assert_eq!(r.body, [vec!["article".to_string()]]);
        assert_eq!(r.strip, ["aside"]);
        assert_eq!(r.author.len(), 1);
    }

    const PAGE: &str = r#"<html><head><meta name="byl" content="By  Jane Doe"></head><body>
        <div class="masthead">The Daily</div>
        <div id="story"><p>The first paragraph of the story, long enough to count.</p>
          <div class="share-tools">Share this</div>
          <p>The second paragraph, with <amp-img src="/a.jpg"></amp-img> a picture.</p>
          <img src="https://ads.doubleclick.net/pixel.gif">
        </div>
        <div class="related">Read next: something else entirely</div>
      </body></html>"#;

    #[test]
    fn a_site_rule_finds_the_article_and_strips_what_is_around_it() {
        let site = rules(
            "body: //div[@id='story']\nstrip_id_or_class: share\n\
             author: //meta[@name=\"byl\"]/@content\nfind_string: <amp-img\nreplace_string: <img",
        );
        let global =
            rules("strip_image_src: doubleclick.net\nfind_string: </amp-img>\nreplace_string: ");
        let prepared = prepare_with(PAGE, Some(&site), Some(&global));

        let article = prepared.article.expect("the body rule matched");
        assert!(article.html.contains("first paragraph"), "{}", article.html);
        assert!(article.html.contains("<img src=\"/a.jpg\">"), "{}", article.html);
        assert!(!article.html.contains("Share this"), "{}", article.html);
        assert!(!article.html.contains("doubleclick"), "{}", article.html);
        assert!(!article.html.contains("masthead"), "{}", article.html);
        assert!(!article.html.contains("Read next"), "{}", article.html);
        assert_eq!(prepared.byline.as_deref(), Some("By Jane Doe"));
        // The page Readability is given has lost the same things.
        assert!(!prepared.page.contains("Share this"));
        assert!(prepared.page.contains("Read next"));
    }

    #[test]
    fn the_first_body_rule_with_text_wins_and_nested_matches_count_once() {
        let site = rules(
            "body: //section[@id='missing']\nbody: //div[@id='story'] | //div[@id='story']//p",
        );
        let article = prepare_with(PAGE, Some(&site), None).article.unwrap();
        assert_eq!(article.html.matches("first paragraph").count(), 1, "{}", article.html);
        assert!(article.text_chars > 50);
    }

    #[test]
    fn no_site_rules_strips_only_what_the_global_rules_say() {
        let global = rules("strip_image_src: doubleclick.net");
        let prepared = prepare_with(PAGE, None, Some(&global));
        assert!(prepared.article.is_none());
        assert!(prepared.byline.is_none());
        assert!(!prepared.page.contains("doubleclick"));
        assert!(prepared.page.contains("Share this"));
    }

    #[test]
    fn a_rule_that_finds_a_fraction_of_the_page_does_not_hold_up() {
        assert!(holds_up(900, 1000));
        assert!(holds_up(400, 1200));
        assert!(!holds_up(300, 1200));
        assert!(!holds_up(0, 0));
        assert!(holds_up(10, 0), "Readability found nothing");
    }

    #[test]
    fn the_bundle_is_read_and_most_of_its_rules_are_used() {
        let sites = sites();
        assert!(sites.len() > 1_500, "{} sites", sites.len());
        assert!(sites.contains_key(GLOBAL));

        // How much of the bundle translates, directive by directive. A refresh
        // that brings in XPath the translator cannot read shows up here.
        let share = |directive: &str| {
            let (mut total, mut read) = (0usize, 0usize);
            for path in BUNDLE.lines().filter_map(|line| line.strip_prefix(directive)) {
                total += 1;
                read += usize::from(elements(path.trim()).is_some());
            }
            (read, total)
        };
        // Where the article is, which is what matters most, nearly always (96%
        // at 6c60966); what to strip less often (85%), since that is where the
        // axes and the tests on text are.
        let (read, total) = share("body:");
        assert!(read * 100 >= total * 95, "{read} of {total} body rules translated");
        let (read, total) = share("strip:");
        assert!(read * 100 >= total * 80, "{read} of {total} strip rules translated");

        // Every selector made is one the HTML reader accepts.
        for rules in sites.values() {
            for selector in rules.strip.iter().chain(rules.body.iter().flatten()) {
                assert!(Matcher::new(selector).is_ok(), "{selector}");
            }
            for css in &rules.author {
                assert!(Matcher::new(&css.selector).is_ok(), "{}", css.selector);
            }
        }
    }
}
