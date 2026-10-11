//! OPML 2.0 out: the list of subscriptions every feed reader can read.
//!
//! Writing only. Reading somebody else's OPML file means parsing XML, which
//! this crate does not do; `everyday-service::news::opml` reads it. Writing
//! is a few lines of escaped text, and lives here so the export
//! (`everyday-transfer`, which has no XML parser either) can call it.

use std::collections::HashMap;

/// One subscription, as OPML can say it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outline {
    pub title: String,
    /// The feed's address (`xmlUrl`).
    pub xml_url: String,
    /// The site's address (`htmlUrl`). Empty when unknown.
    pub html_url: String,
    /// The folder it sits in. Empty for none; outlines sharing a folder are
    /// grouped under one parent `<outline>`.
    pub folder: String,
}

/// An OPML 2.0 document listing `outlines`, grouped by folder, in the order
/// given (folders in order of first appearance).
///
/// A folder sits where its first member did, so an unfiled subscription that
/// came before it stays above it. Every subscription is written as
/// `type="rss"`, the one value every feed reader understands, whatever
/// format the feed itself is in.
pub fn write(title: &str, outlines: &[Outline]) -> String {
    let mut top: Vec<Node> = Vec::new();
    let mut folders: HashMap<&str, usize> = HashMap::new();
    for outline in outlines {
        let folder = outline.folder.trim();
        if folder.is_empty() {
            top.push(Node::Feed(outline));
        } else if let Some(&at) = folders.get(folder) {
            if let Node::Folder(_, members) = &mut top[at] {
                members.push(outline);
            }
        } else {
            folders.insert(folder, top.len());
            top.push(Node::Folder(folder, vec![outline]));
        }
    }

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<opml version=\"2.0\">\n");
    out.push_str(&format!("  <head>\n    <title>{}</title>\n  </head>\n", escape(title)));
    out.push_str("  <body>\n");
    for node in top {
        match node {
            Node::Feed(outline) => push_feed(&mut out, "    ", outline),
            Node::Folder(name, members) => {
                let name = escape(name);
                out.push_str(&format!("    <outline text=\"{name}\" title=\"{name}\">\n"));
                for outline in members {
                    push_feed(&mut out, "      ", outline);
                }
                out.push_str("    </outline>\n");
            }
        }
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

/// What sits at the top level of the body.
enum Node<'a> {
    Feed(&'a Outline),
    Folder(&'a str, Vec<&'a Outline>),
}

fn push_feed(out: &mut String, indent: &str, outline: &Outline) {
    // `text` is the one attribute the specification requires, so a
    // subscription with no title still has something to be called.
    let name = escape(match outline.title.trim() {
        "" => outline.xml_url.trim(),
        title => title,
    });
    out.push_str(&format!(
        "{indent}<outline type=\"rss\" text=\"{name}\" title=\"{name}\" xmlUrl=\"{}\"",
        escape(outline.xml_url.trim())
    ));
    let html_url = outline.html_url.trim();
    if !html_url.is_empty() {
        out.push_str(&format!(" htmlUrl=\"{}\"", escape(html_url)));
    }
    out.push_str("/>\n");
}

/// Text safe in an XML attribute or element: the five characters XML gives a
/// meaning to, the whitespace an attribute would otherwise lose, and nothing
/// XML 1.0 forbids outright -- a feed's title is a stranger's word, and one
/// stray control character would make the whole file unreadable.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if c < ' ' || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(title: &str, xml_url: &str, html_url: &str, folder: &str) -> Outline {
        Outline {
            title: title.into(),
            xml_url: xml_url.into(),
            html_url: html_url.into(),
            folder: folder.into(),
        }
    }

    #[test]
    fn an_empty_list_is_still_a_document() {
        let xml = write("Subscriptions", &[]);
        assert_eq!(
            xml,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <opml version=\"2.0\">\n  \
             <head>\n    <title>Subscriptions</title>\n  </head>\n  \
             <body>\n  </body>\n\
             </opml>\n"
        );
    }

    #[test]
    fn a_subscription_is_one_line_and_leaves_out_an_unknown_site() {
        let xml = write(
            "Mine",
            &[
                outline("Quiet", "https://q.example/feed", "https://q.example/", ""),
                outline("Lab", "https://lab.example/rss", "", ""),
            ],
        );
        assert!(xml.contains(
            "    <outline type=\"rss\" text=\"Quiet\" title=\"Quiet\" \
             xmlUrl=\"https://q.example/feed\" htmlUrl=\"https://q.example/\"/>\n"
        ));
        assert!(xml.contains(
            "    <outline type=\"rss\" text=\"Lab\" title=\"Lab\" \
             xmlUrl=\"https://lab.example/rss\"/>\n"
        ));
    }

    #[test]
    fn a_subscription_without_a_title_is_called_by_its_address() {
        let xml = write("Mine", &[outline("  ", "https://a.example/feed", "", "")]);
        assert!(xml.contains("text=\"https://a.example/feed\" title=\"https://a.example/feed\""));
    }

    #[test]
    fn folders_are_grouped_in_order_of_first_appearance_and_unfiled_stay_on_top() {
        let xml = write(
            "Mine",
            &[
                outline("News 1", "https://n1.example/f", "", "News"),
                outline("Loose 1", "https://l1.example/f", "", ""),
                outline("Code 1", "https://c1.example/f", "", "Code"),
                outline("News 2", "https://n2.example/f", "", " News "),
                outline("Loose 2", "https://l2.example/f", "", ""),
                outline("Code 2", "https://c2.example/f", "", "Code"),
            ],
        );
        let order: Vec<&str> = xml
            .lines()
            .filter_map(|l| l.split("text=\"").nth(1))
            .filter_map(|l| l.split('"').next())
            .collect();
        assert_eq!(
            order,
            ["News", "News 1", "News 2", "Loose 1", "Code", "Code 1", "Code 2", "Loose 2"]
        );
        // A folder opens before its members and closes after them, and a
        // member is indented one level further than the folder.
        assert!(xml.contains("title=\"News\">\n      <outline type=\"rss\" text=\"News 1\""));
        assert_eq!(xml.matches("    </outline>\n").count(), 2);
        assert!(xml.contains("    <outline type=\"rss\" text=\"Loose 1\""));
    }

    #[test]
    fn what_xml_gives_a_meaning_to_is_escaped_in_attributes_and_the_title() {
        let xml = write(
            "Tom & Jerry's <feeds>",
            &[outline(
                "Q&A \"live\" <b>it's</b>",
                "https://x.example/f?a=1&b=2",
                "https://x.example/?q='1'",
                "R&D <lab>",
            )],
        );
        assert!(xml.contains("<title>Tom &amp; Jerry&apos;s &lt;feeds&gt;</title>"));
        assert!(xml.contains("<outline text=\"R&amp;D &lt;lab&gt;\""));
        assert!(xml.contains("text=\"Q&amp;A &quot;live&quot; &lt;b&gt;it&apos;s&lt;/b&gt;\""));
        assert!(xml.contains("xmlUrl=\"https://x.example/f?a=1&amp;b=2\""));
        assert!(xml.contains("htmlUrl=\"https://x.example/?q=&apos;1&apos;\""));
        assert!(!xml.contains("Q&A"), "no raw ampersand survives");
    }

    #[test]
    fn whitespace_an_attribute_would_lose_is_kept_and_what_xml_forbids_is_dropped() {
        let o = outline("one\ntwo\tthree\u{7}four", "https://x.example/f", "", "");
        let xml = write("Mine", &[o]);
        assert!(xml.contains("text=\"one&#10;two&#9;threefour\""));
    }
}
