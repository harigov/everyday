//! OPML in: somebody else's feed reader's list of subscriptions. Writing is
//! `everyday_core::news::opml::write`; reading needs an XML parser, which the
//! core does not have.

use everyday_core::news::opml::Outline;
use roxmltree::{Document, Node, ParsingOptions};

/// Every subscription in an OPML document, nested outlines flattened, each
/// carrying the title of the outline it sat under as its folder. Outlines
/// with no `xmlUrl` are folders, not subscriptions, and are not returned.
pub fn read(xml: &str) -> Result<Vec<Outline>, String> {
    // Some exporters put a byte-order mark in front of the declaration,
    // which XML does not allow a parser to take for text.
    let xml = xml.trim_start_matches('\u{feff}');
    // A DTD is allowed because old exporters write one; `roxmltree` fetches
    // no external entity and guards against entities that expand into
    // themselves. The node limit is what keeps a hostile file's memory in
    // check, for a list of subscriptions is a few thousand nodes at most.
    let options =
        ParsingOptions { allow_dtd: true, nodes_limit: 1_000_000, ..ParsingOptions::default() };
    let doc = Document::parse_with_options(xml, options)
        .map_err(|e| format!("The OPML file is malformed: {e}."))?;

    let root = doc.root_element();
    if !is_named(&root, "opml") {
        return Err("This is not an OPML file.".into());
    }
    let Some(body) = root.children().find(|node| is_named(node, "body")) else {
        return Err("This OPML file has no list of subscriptions.".into());
    };

    // Walked with a stack of the outlines still to visit, each with the
    // folder it sits in, rather than by recursion: the depth is a stranger's
    // to choose. Children are pushed in reverse so that they come off in
    // document order.
    let mut found = Vec::new();
    let mut pending: Vec<(Node, String)> =
        body.children().rev().map(|node| (node, String::new())).collect();
    while let Some((node, folder)) = pending.pop() {
        if !is_named(&node, "outline") {
            continue;
        }
        let name = attribute(&node, "title")
            .filter(|title| !title.trim().is_empty())
            .or_else(|| attribute(&node, "text"))
            .map_or("", str::trim);

        let inside = match attribute(&node, "xmlUrl").map(str::trim).filter(|url| !url.is_empty()) {
            Some(xml_url) => {
                let title = if name.is_empty() { xml_url } else { name };
                found.push(Outline {
                    title: title.to_string(),
                    xml_url: xml_url.to_string(),
                    html_url: attribute(&node, "htmlUrl").unwrap_or_default().trim().to_string(),
                    folder: folder.clone(),
                });
                // Anything nested inside a subscription is in the folder the
                // subscription is in, not in the subscription.
                folder
            }
            // An outline with no address is a folder, if it has a name; a
            // nameless one is only a level of nesting.
            None if name.is_empty() => folder,
            None => name.to_string(),
        };
        pending.extend(node.children().rev().map(|child| (child, inside.clone())));
    }
    Ok(found)
}

/// Is this an element called `name`? Case does not matter, and neither does
/// the namespace: exporters differ on both.
fn is_named(node: &Node, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

/// An attribute's value, by name, ignoring case: `xmlUrl` is also seen as
/// `xmlurl`.
fn attribute<'a>(node: &Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name().eq_ignore_ascii_case(name)).map(|a| a.value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::news::opml::write;

    const NESTED: &str = include_str!("../../tests/fixtures/news/nested.opml");
    const FLAT: &str = include_str!("../../tests/fixtures/news/flat.opml");

    fn outline(title: &str, xml_url: &str, html_url: &str, folder: &str) -> Outline {
        Outline {
            title: title.into(),
            xml_url: xml_url.into(),
            html_url: html_url.into(),
            folder: folder.into(),
        }
    }

    #[test]
    fn nested_outlines_are_flattened_with_the_nearest_folder() {
        let outlines = read(NESTED).unwrap();
        assert_eq!(
            outlines,
            [
                // At the top level: no folder.
                outline(
                    "Daring Fireball",
                    "https://daringfireball.example/feeds/main",
                    "https://daringfireball.example/",
                    ""
                ),
                // An entity in an attribute is decoded, a title is taken
                // over text, and the folder is the one it sits in.
                outline(
                    "Ars & Co",
                    "https://ars.example/rss?a=1&b=2",
                    "https://ars.example/",
                    "Tech"
                ),
                // Two levels down: the nearest folder only, not the path.
                outline("This Week in Rust", "https://twir.example/rss.xml", "", "Rust"),
                // `title` alone is a title.
                outline("Only a title", "https://only-title.example/feed", "", "Rust"),
                // With neither, the address is what it is called.
                outline("https://bare.example/feed", "https://bare.example/feed", "", "Tech"),
                // `xmlurl` and `htmlurl` in lower case, as some exporters write them.
                outline("xkcd", "https://xkcd.example/atom.xml", "https://xkcd.example/", "Comics"),
            ]
        );
    }

    #[test]
    fn a_folder_with_nothing_in_it_and_an_outline_that_is_a_link_are_not_subscriptions() {
        let outlines = read(NESTED).unwrap();
        assert!(outlines.iter().all(|o| o.title != "Empty folder"));

        let outlines = read(FLAT).unwrap();
        assert_eq!(
            outlines,
            [
                outline("Alpha", "https://alpha.example/feed", "https://alpha.example", ""),
                outline("Beta", "https://beta.example/rss", "", ""),
            ]
        );
    }

    #[test]
    fn a_byte_order_mark_and_a_doctype_are_tolerated() {
        let xml = "\u{feff}<?xml version=\"1.0\"?>\n\
            <!DOCTYPE opml [ <!ENTITY pub \"The Press\"> ]>\n\
            <opml version=\"2.0\"><body>\
            <outline text=\"&pub;\" xmlUrl=\"https://press.example/feed\"/>\
            </body></opml>";
        let outlines = read(xml).unwrap();
        assert_eq!(outlines, [outline("The Press", "https://press.example/feed", "", "")]);
    }

    #[test]
    fn a_nameless_outline_does_not_hide_the_folder_above_it() {
        let xml = r#"<opml version="2.0"><body>
            <outline text="News">
              <outline>
                <outline text="Wire" xmlUrl="https://wire.example/feed"/>
              </outline>
            </outline>
            </body></opml>"#;
        assert_eq!(read(xml).unwrap(), [outline("Wire", "https://wire.example/feed", "", "News")]);
    }

    #[test]
    fn blank_addresses_and_whitespace_are_not_subscriptions_or_titles() {
        let xml = r#"<opml version="2.0"><body>
            <outline text="Blank" xmlUrl="   "/>
            <outline text="  Padded  " xmlUrl="  https://padded.example/feed  "
                     htmlUrl=" https://padded.example/ "/>
            <outline title="" text="Falls back" xmlUrl="https://fb.example/feed"/>
            </body></opml>"#;
        assert_eq!(
            read(xml).unwrap(),
            [
                outline("Padded", "https://padded.example/feed", "https://padded.example/", ""),
                outline("Falls back", "https://fb.example/feed", "", ""),
            ]
        );
    }

    #[test]
    fn an_empty_list_is_an_empty_list() {
        assert!(read(r#"<opml version="2.0"><head/><body/></opml>"#).unwrap().is_empty());
    }

    #[test]
    fn what_is_not_opml_says_so_in_a_sentence() {
        assert_eq!(read("<html><body></body></html>").unwrap_err(), "This is not an OPML file.");
        assert_eq!(
            read(r#"<opml version="2.0"><head/></opml>"#).unwrap_err(),
            "This OPML file has no list of subscriptions."
        );
        for broken in ["", "not xml at all", r#"<opml><body><outline text="a" xmlUrl="x""#] {
            let error = read(broken).unwrap_err();
            assert!(error.starts_with("The OPML file is malformed: "), "{broken:?}: {error}");
            assert!(error.ends_with('.'), "{error}");
        }
    }

    #[test]
    fn what_the_core_writes_this_reads_back() {
        let outlines = vec![
            outline("Plain", "https://plain.example/feed", "https://plain.example/", ""),
            outline(
                "Q&A \"live\" <it's>",
                "https://q.example/f?a=1&b=2",
                "https://q.example/?x='1'",
                "R&D <lab>",
            ),
            outline("Second", "https://second.example/feed", "", "R&D <lab>"),
            outline("Other", "https://other.example/feed", "", "Reading"),
        ];
        let xml = write("My subscriptions", &outlines);
        assert_eq!(read(&xml).unwrap(), outlines);
    }
}
