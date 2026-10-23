//! A page as the model reads it: the browser's accessibility tree, written
//! out as short lines -- headings, text, and every link, button and field
//! with a reference to act on it by.
//!
//! The accessibility tree rather than the HTML, for the reason screen
//! readers use it: it is what the page *shows*, already resolved -- names
//! computed from labels, `display: none` and `aria-hidden` gone, a `<div>`
//! with a click handler and `role="button"` called a button. Text a page
//! hides by other means (white on white, one pixel high, a long way off the
//! left edge) is marked hidden before the tree is read -- see
//! `HIDE_INVISIBLE` in [`super::page`] -- because hidden text is how a page
//! talks to a model behind the person's back.
//!
//! References are stable: an element keeps its `e12` for as long as it is
//! on the page, however many times the page is looked at, so a model that
//! clicked one thing and then types into another it saw earlier types into
//! the one it meant.

use std::collections::HashMap;

use serde_json::Value;

use super::action::flat;

/// The roles that get a reference: the things that can be acted on.
const ACTIONABLE: [&str; 18] = [
    "link",
    "button",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "option",
    "MenuListOption",
    "treeitem",
    "listbox",
];

/// Roles whose lines say nothing a model needs.
const SKIPPED: [&str; 4] = ["InlineTextBox", "LineBreak", "ListMarker", "none"];

/// The longest any one line's text may be.
const MAX_LINE_CHARS: usize = 400;

/// Which element has which reference, kept for as long as a session lives.
#[derive(Debug, Default)]
pub struct Refs {
    by_node: HashMap<i64, u32>,
    by_ref: HashMap<u32, i64>,
    next: u32,
}

impl Refs {
    /// The reference for `node`, giving it one if it has none.
    fn of(&mut self, node: i64) -> String {
        let n = *self.by_node.entry(node).or_insert_with(|| {
            self.next += 1;
            self.next
        });
        self.by_ref.insert(n, node);
        format!("e{n}")
    }

    /// The node a reference names, if it named one.
    pub fn node(&self, reference: &str) -> Option<i64> {
        let n: u32 = reference.trim().trim_start_matches(['e', 'E']).parse().ok()?;
        self.by_ref.get(&n).copied()
    }

    /// Forget every reference: the page is a different page now.
    pub fn clear(&mut self) {
        *self = Refs::default();
    }
}

/// The page written out, and every address it links to.
#[derive(Debug, Default, PartialEq)]
pub struct Outline {
    pub text: String,
    pub links: Vec<String>,
}

/// Write out the nodes `Accessibility.getFullAXTree` returned.
pub fn render(nodes: &[Value], refs: &mut Refs) -> Outline {
    let by_id: HashMap<&str, &Value> =
        nodes.iter().filter_map(|n| Some((n.get("nodeId")?.as_str()?, n))).collect();
    // The root is the node nobody names as a child.
    let children: std::collections::HashSet<&str> = nodes
        .iter()
        .flat_map(|n| n.get("childIds").and_then(Value::as_array).into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    let mut out = Outline::default();
    for root in
        nodes.iter().filter_map(|n| n.get("nodeId")?.as_str()).filter(|id| !children.contains(id))
    {
        walk(root, &by_id, refs, &mut out, false, 0);
    }
    out
}

fn walk(
    id: &str,
    by_id: &HashMap<&str, &Value>,
    refs: &mut Refs,
    out: &mut Outline,
    in_named: bool,
    depth: usize,
) {
    // A tree deeper than this is a page built to be one.
    if depth > 200 {
        return;
    }
    let Some(node) = by_id.get(id) else { return };
    let ignored = node.get("ignored").and_then(Value::as_bool).unwrap_or(false);
    let role = value_str(node.get("role")).unwrap_or_default();
    let name = flat(&value_str(node.get("name")).unwrap_or_default(), MAX_LINE_CHARS);
    let mut names_its_text = in_named;

    if !ignored && !SKIPPED.contains(&role.as_str()) {
        if ACTIONABLE.contains(&role.as_str()) {
            if let Some(node_id) = node.get("backendDOMNodeId").and_then(Value::as_i64) {
                let reference = refs.of(node_id);
                let line = actionable_line(&reference, &role, &name, node, out);
                out.text.push_str(&line);
                out.text.push('\n');
            }
            names_its_text = true;
        } else if role == "heading" {
            let level = property(node, "level").and_then(Value::as_u64).unwrap_or(2).clamp(1, 6);
            if !name.is_empty() {
                out.text.push_str(&"#".repeat(level as usize));
                out.text.push(' ');
                out.text.push_str(&name);
                out.text.push('\n');
            }
            names_its_text = true;
        } else if role == "StaticText" {
            if !in_named && !name.is_empty() {
                out.text.push_str(&name);
                out.text.push('\n');
            }
        } else if (role == "image" || role == "img") && !name.is_empty() && !in_named {
            out.text.push_str(&format!("image \u{201c}{name}\u{201d}\n"));
        }
    }

    for child in node.get("childIds").and_then(Value::as_array).into_iter().flatten() {
        if let Some(child) = child.as_str() {
            walk(child, by_id, refs, out, names_its_text, depth + 1);
        }
    }
}

/// `[e3] link "Next page" -> https://...`, `[e4] textbox "Email" = "a@b"
/// (required)`, and so on.
fn actionable_line(
    reference: &str,
    role: &str,
    name: &str,
    node: &Value,
    out: &mut Outline,
) -> String {
    let role = if role == "MenuListOption" { "option" } else { role };
    let mut line = format!("[{reference}] {role}");
    if !name.is_empty() {
        line.push_str(&format!(" \u{201c}{name}\u{201d}"));
    }
    if let Some(value) = value_str(node.get("value")).filter(|v| !v.trim().is_empty()) {
        line.push_str(&format!(" = \u{201c}{}\u{201d}", flat(&value, 120)));
    }
    if role == "link"
        && let Some(url) = property(node, "url").and_then(Value::as_str)
        && (url.starts_with("http://") || url.starts_with("https://"))
    {
        out.links.push(url.to_string());
        line.push_str(" -> ");
        line.push_str(&flat(url, 200));
    }
    let mut states = Vec::new();
    for (property_name, shown) in [
        ("disabled", "disabled"),
        ("required", "required"),
        ("invalid", "invalid"),
        ("focused", "focused"),
        ("selected", "selected"),
        ("expanded", "expanded"),
    ] {
        if property(node, property_name)
            .is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"))
        {
            states.push(shown);
        }
    }
    match property(node, "checked")
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_bool().map(|b| b.to_string())))
    {
        Some(c) if c == "true" => states.push("checked"),
        Some(c) if c == "mixed" => states.push("partly checked"),
        Some(c) if c == "false" => states.push("not checked"),
        _ => {}
    }
    if !states.is_empty() {
        line.push_str(&format!(" ({})", states.join(", ")));
    }
    line
}

/// An `AXValue`'s own value as text.
fn value_str(value: Option<&Value>) -> Option<String> {
    let inner = value?.get("value")?;
    match inner {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A named entry of a node's `properties`, as its bare value.
fn property<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get("properties")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(name))?
        .get("value")?
        .get("value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ax(id: &str, role: &str, name: &str, children: &[&str]) -> Value {
        json!({
            "nodeId": id,
            "ignored": false,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "childIds": children,
            "backendDOMNodeId": id.parse::<i64>().unwrap() + 100,
        })
    }

    fn page() -> Vec<Value> {
        let mut link = ax("4", "link", "Next page", &["5"]);
        link["properties"] = json!([{ "name": "url", "value": { "type": "string", "value": "https://shop.example/p2" } }]);
        let mut field = ax("6", "textbox", "Email", &[]);
        field["value"] = json!({ "type": "string", "value": "a@b.example" });
        field["properties"] =
            json!([{ "name": "required", "value": { "type": "boolean", "value": true } }]);
        let mut heading = ax("2", "heading", "Results", &["3"]);
        heading["properties"] =
            json!([{ "name": "level", "value": { "type": "integer", "value": 1 } }]);
        let mut hidden = ax("8", "generic", "", &["9"]);
        hidden["ignored"] = json!(true);
        vec![
            ax("1", "RootWebArea", "Shop", &["2", "4", "6", "7", "8"]),
            heading,
            ax("3", "StaticText", "Results", &[]),
            link,
            ax("5", "StaticText", "Next page", &[]),
            field,
            ax("7", "StaticText", "Free   shipping over $50", &[]),
            hidden,
            ax("9", "StaticText", "still shown: ignored only hides its own line", &[]),
        ]
    }

    #[test]
    fn a_page_reads_as_headings_text_and_numbered_controls() {
        let mut refs = Refs::default();
        let outline = render(&page(), &mut refs);
        assert_eq!(
            outline.text,
            "# Results\n\
             [e1] link \u{201c}Next page\u{201d} -> https://shop.example/p2\n\
             [e2] textbox \u{201c}Email\u{201d} = \u{201c}a@b.example\u{201d} (required)\n\
             Free shipping over $50\n\
             still shown: ignored only hides its own line\n"
        );
        assert_eq!(outline.links, vec!["https://shop.example/p2".to_string()]);
        assert_eq!(refs.node("e1"), Some(104));
        assert_eq!(refs.node("e2"), Some(106));
        assert_eq!(refs.node("e3"), None);
    }

    #[test]
    fn an_element_keeps_its_reference_however_often_it_is_looked_at() {
        let mut refs = Refs::default();
        render(&page(), &mut refs);
        // The link gone, a button new: the field is still e2, the button e3.
        let mut nodes = page();
        nodes[0]["childIds"] = json!(["6", "10"]);
        nodes.push(ax("10", "button", "Buy", &[]));
        let outline = render(&nodes, &mut refs);
        assert!(outline.text.contains("[e2] textbox"), "{}", outline.text);
        assert!(outline.text.contains("[e3] button \u{201c}Buy\u{201d}"), "{}", outline.text);
        refs.clear();
        assert_eq!(refs.node("e2"), None);
    }
}
