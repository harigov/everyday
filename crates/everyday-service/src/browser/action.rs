//! What can be done to a page, as data, and the rules for when it may be.
//!
//! Every way of driving the browser comes down to a [`BrowserAction`]: the
//! assistant's own `browser_*` tools produce one per call, and a model built
//! for computer use -- one that looks at a screenshot and answers "click at
//! (412, 230)" -- would produce the same ones through an adapter, with a
//! [`Target::Point`] where the tools use a [`Target::Ref`]. Whichever it
//! was, [`judge`] decides on the action and on the [`Element`] it would
//! really touch, read out of the browser rather than taken from the model's
//! description of it. So the rules below are the rules for every driver, and
//! a new driver is a translation, not a new set of rules.
//!
//! # The rules
//!
//! - **Typing always asks**, showing the exact text, the field as the page
//!   names it, and the site as the browser knows it -- not as the page
//!   claims. A page can read what is typed into it the moment it is typed,
//!   so this is the moment something leaves the vault, exactly as sending
//!   mail is. There is no switch for it.
//! - **Some fields are never typed into**: passwords, one-time codes,
//!   payment cards, file uploads. The assistant has no business holding any
//!   of them, and a page asking for one is the shape phishing takes. The
//!   person is told to do it themselves.
//! - **Clicking, choosing and pressing keys go ahead**, unless mail (or
//!   anything else that is a stranger's writing *and* private) has been
//!   read this turn: then which button the model picks could be a choice the
//!   mail made, and each one asks, as a page open does. A click carries
//!   nothing the model wrote -- where a link goes, the page decided.
//! - **Opening an address** is not judged here: it is `read_web_page`'s
//!   rule, from [`crate::webpage::Provenance`], and the gate applies it.

use serde_json::Value;

/// What a click or a field is aimed at.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// An element by the reference the last look at the page gave it --
    /// `e12`. Stable for as long as the element is on the page.
    Ref(String),
    /// A point in the page's viewport, in CSS pixels from the top left --
    /// what a model reading a screenshot points with. Judged by the element
    /// that is really under it.
    Point { x: f64, y: f64 },
}

/// A key the browser may be asked to press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    Backspace,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
}

impl Key {
    /// Every key, by the name the tool takes.
    pub const ALL: [(&'static str, Key); 13] = [
        ("Enter", Key::Enter),
        ("Tab", Key::Tab),
        ("Escape", Key::Escape),
        ("Backspace", Key::Backspace),
        ("Space", Key::Space),
        ("ArrowUp", Key::ArrowUp),
        ("ArrowDown", Key::ArrowDown),
        ("ArrowLeft", Key::ArrowLeft),
        ("ArrowRight", Key::ArrowRight),
        ("PageUp", Key::PageUp),
        ("PageDown", Key::PageDown),
        ("Home", Key::Home),
        ("End", Key::End),
    ];

    pub fn parse(name: &str) -> Option<Key> {
        let name = name.trim();
        Self::ALL.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, k)| *k)
    }

    /// `(key, code, keyCode, text)` as `Input.dispatchKeyEvent` wants them.
    pub fn cdp(self) -> (&'static str, &'static str, u32, Option<&'static str>) {
        match self {
            Key::Enter => ("Enter", "Enter", 13, Some("\r")),
            Key::Tab => ("Tab", "Tab", 9, None),
            Key::Escape => ("Escape", "Escape", 27, None),
            Key::Backspace => ("Backspace", "Backspace", 8, None),
            Key::Space => (" ", "Space", 32, Some(" ")),
            Key::ArrowUp => ("ArrowUp", "ArrowUp", 38, None),
            Key::ArrowDown => ("ArrowDown", "ArrowDown", 40, None),
            Key::ArrowLeft => ("ArrowLeft", "ArrowLeft", 37, None),
            Key::ArrowRight => ("ArrowRight", "ArrowRight", 39, None),
            Key::PageUp => ("PageUp", "PageUp", 33, None),
            Key::PageDown => ("PageDown", "PageDown", 34, None),
            Key::Home => ("Home", "Home", 36, None),
            Key::End => ("End", "End", 35, None),
        }
    }
}

/// Which way to scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// One thing to do to the browser.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowserAction {
    /// Go to an address.
    Open { url: String },
    /// Click an element.
    Click { target: Target },
    /// Replace what is in a field with `text`, and press Enter after it if
    /// `submit`.
    Type { target: Target, text: String, submit: bool },
    /// Pick an option of a `<select>` list by what it says.
    Select { target: Target, option: String },
    /// Press a key, wherever the focus is.
    Press { key: Key },
    /// Scroll the page -- or the part of it at `target` -- a screenful.
    Scroll { direction: Direction, target: Option<Target> },
    /// Back to the page before.
    Back,
    /// Look at the page again without doing anything to it.
    Look,
    /// Close the browser.
    Close,
}

/// The longest text one `browser_type` may put into a field. A form field,
/// not a document; a model typing pages of text is not filling in a form.
pub const MAX_TYPED_CHARS: usize = 2_000;

/// The names of the tools, each one action.
pub const OPEN: &str = "browser_open";
pub const CLICK: &str = "browser_click";
pub const TYPE: &str = "browser_type";
pub const SELECT: &str = "browser_select";
pub const PRESS: &str = "browser_press";
pub const SCROLL: &str = "browser_scroll";
pub const BACK: &str = "browser_back";
pub const LOOK: &str = "browser_look";
pub const CLOSE: &str = "browser_close";

/// Every browser tool's name.
pub const TOOLS: [&str; 9] = [OPEN, CLICK, TYPE, SELECT, PRESS, SCROLL, BACK, LOOK, CLOSE];

/// Whether `name` is one of the browser's tools.
pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

impl BrowserAction {
    /// The action a call to `tool` with `arguments` asks for. The error is a
    /// sentence for the model.
    pub fn from_call(tool: &str, arguments: &Value) -> Result<Self, String> {
        let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::trim);
        let target = || -> Result<Target, String> {
            if let Some(element) = text("element").filter(|e| !e.is_empty()) {
                return Ok(Target::Ref(element.to_string()));
            }
            let x = arguments.get("x").and_then(Value::as_f64);
            let y = arguments.get("y").and_then(Value::as_f64);
            match (x, y) {
                (Some(x), Some(y)) if x >= 0.0 && y >= 0.0 => Ok(Target::Point { x, y }),
                _ => Err(format!(
                    "{tool}: `element` is required -- a reference like e12 from the last look \
                     at the page"
                )),
            }
        };
        Ok(match tool {
            OPEN => {
                let url = text("url").filter(|u| !u.is_empty());
                BrowserAction::Open {
                    url: url.ok_or("browser_open: `url` is required")?.to_string(),
                }
            }
            CLICK => BrowserAction::Click { target: target()? },
            TYPE => {
                let typed = arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("browser_type: `text` is required")?;
                if typed.chars().count() > MAX_TYPED_CHARS {
                    return Err(format!(
                        "browser_type: at most {MAX_TYPED_CHARS} characters at a time"
                    ));
                }
                BrowserAction::Type {
                    target: target()?,
                    text: typed.to_string(),
                    submit: arguments.get("submit").and_then(Value::as_bool).unwrap_or(false),
                }
            }
            SELECT => BrowserAction::Select {
                target: target()?,
                option: text("option")
                    .filter(|o| !o.is_empty())
                    .ok_or("browser_select: `option` is required")?
                    .to_string(),
            },
            PRESS => {
                let name = text("key").unwrap_or_default();
                BrowserAction::Press {
                    key: Key::parse(name).ok_or_else(|| {
                        let names: Vec<&str> = Key::ALL.iter().map(|(n, _)| *n).collect();
                        format!("browser_press: `key` must be one of {}", names.join(", "))
                    })?,
                }
            }
            SCROLL => BrowserAction::Scroll {
                direction: match text("direction").unwrap_or("down") {
                    d if d.eq_ignore_ascii_case("up") => Direction::Up,
                    d if d.eq_ignore_ascii_case("down") => Direction::Down,
                    _ => return Err("browser_scroll: `direction` is \"up\" or \"down\"".into()),
                },
                target: target().ok(),
            },
            BACK => BrowserAction::Back,
            LOOK => BrowserAction::Look,
            CLOSE => BrowserAction::Close,
            other => return Err(format!("{other} is not a browser tool")),
        })
    }

    /// The element this action is aimed at, if it is aimed at one.
    pub fn target(&self) -> Option<&Target> {
        match self {
            BrowserAction::Click { target }
            | BrowserAction::Type { target, .. }
            | BrowserAction::Select { target, .. } => Some(target),
            BrowserAction::Scroll { target, .. } => target.as_ref(),
            _ => None,
        }
    }

    /// Whether this needs a page to be open already.
    pub fn needs_page(&self) -> bool {
        !matches!(self, BrowserAction::Open { .. } | BrowserAction::Close)
    }
}

/// An element as the browser reports it -- read in a JavaScript world of
/// this application's own, which the page's scripts cannot reach into or
/// lie through. Everything a rule or a confirmation needs to say what is
/// really about to be touched.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    /// The browser's own id for the node, which an approval is tied to.
    pub node: i64,
    /// `input`, `button`, `a`...
    pub tag: String,
    /// An `<input>`'s `type`, lowercased.
    pub input_type: String,
    /// An explicit `role`, if the page gave one.
    pub role: String,
    /// What the page calls it: its label, its `aria-label`, its placeholder,
    /// or its text. The page's words.
    pub label: String,
    /// Its `name` and `id` attributes, which say what a field is for when
    /// its label does not.
    pub name: String,
    pub id: String,
    /// Its `autocomplete` attribute, lowercased.
    pub autocomplete: String,
    /// Where it leads, if it is (or is inside) a link.
    pub href: String,
    /// Whether text can be typed into it.
    pub editable: bool,
}

impl Element {
    /// Read the browser's description of a node, `null` when it is gone.
    pub fn from_described(node: i64, described: &Value) -> Option<Self> {
        let text =
            |key: &str| described.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        described.is_object().then(|| Element {
            node,
            tag: text("tag"),
            input_type: text("type"),
            role: text("role"),
            label: flat(&text("label"), 80),
            name: text("name"),
            id: text("id"),
            autocomplete: text("autocomplete"),
            href: text("href"),
            editable: described.get("editable").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// Why nothing may ever be typed into this, if there is a reason: what
    /// kind of field it is, for the sentence that refuses.
    pub fn secret(&self) -> Option<&'static str> {
        if self.tag == "input" && self.input_type == "file" {
            return Some("a file upload");
        }
        if self.tag == "input" && self.input_type == "password" {
            return Some("a password field");
        }
        let auto = self.autocomplete.as_str();
        if auto.contains("password") {
            return Some("a password field");
        }
        if auto.contains("one-time-code") {
            return Some("a one-time code field");
        }
        if auto.split_whitespace().any(|t| t.starts_with("cc-") && t != "cc-name") {
            return Some("a payment card field");
        }
        // What the field is called, by the page and by its own markup. A
        // field for a password, a card or a code that has not said so in
        // its type is still that field.
        let said = format!("{} {} {}", self.label, self.name, self.id).to_lowercase();
        let said = said.replace(['_', '-'], " ");
        const PASSWORD: [&str; 4] = ["password", "passwd", "passphrase", "passcode"];
        const CARD: [&str; 7] =
            ["card number", "cardnumber", "credit card", "cvv", "cvc", "security code", "expiry"];
        const CODE: [&str; 5] =
            ["one time code", "otp", "verification code", "2fa", "authentication code"];
        const IDENTITY: [&str; 2] = ["social security", "ssn"];
        let has = |words: &[&str]| words.iter().any(|w| contains_word(&said, w));
        if has(&PASSWORD) {
            Some("a password field")
        } else if has(&CARD) {
            Some("a payment card field")
        } else if has(&CODE) {
            Some("a one-time code field")
        } else if has(&IDENTITY) {
            Some("an identity number field")
        } else {
            None
        }
    }

    /// The element in a few words, for a confirmation and a refusal: what
    /// it is and what the page calls it. The label is quoted, because it is
    /// the page's own claim.
    pub fn describe(&self) -> String {
        let what = match (self.tag.as_str(), self.input_type.as_str(), self.role.as_str()) {
            (_, _, role) if !role.is_empty() && role != "presentation" && role != "none" => {
                role.to_string()
            }
            ("a", _, _) => "link".into(),
            ("button", _, _) | ("input", "submit" | "button" | "reset" | "image", _) => {
                "button".into()
            }
            ("input", "checkbox", _) => "checkbox".into(),
            ("input", "radio", _) => "option".into(),
            ("input", _, _) | ("textarea", _, _) => "field".into(),
            ("select", _, _) => "list".into(),
            _ if self.editable => "field".into(),
            _ if !self.href.is_empty() => "link".into(),
            (tag, _, _) if !tag.is_empty() => format!("<{tag}>"),
            _ => "element".into(),
        };
        if self.label.is_empty() {
            what
        } else {
            format!("the {what} \u{201c}{}\u{201d}", self.label)
        }
    }
}

/// `text` with its whitespace collapsed and cut to `max` characters, with an
/// ellipsis when it was cut.
pub fn flat(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut cut: String = joined.chars().take(max).collect();
    cut.push('\u{2026}');
    cut
}

/// Whether `word` appears in `text` with a word boundary each side.
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// What [`judge`] is told about the page as the browser has it.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Whether a page is open at all.
    pub page_open: bool,
    /// The site the page is on, from the browser's own address bar.
    pub site: String,
    /// The element the action would touch, if it is aimed at one and it
    /// was found. `None` with a target is an element that is not there.
    pub element: Option<Element>,
}

/// What may happen to an action.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Go ahead.
    Run,
    /// Ask the person first: the kind of question (as
    /// `AgentEvent::ConfirmationRequired::kind` names it) and what is being
    /// asked about.
    Ask { kind: &'static str, subject: String },
    /// Never, with the sentence the model is told.
    Refuse(String),
}

/// Whether `action` may go ahead on the page `facts` describe -- see the
/// module doc for the rules. `mail_read` is the gate's own taint: a
/// stranger's private writing has been read this turn. `confirm_web` is
/// the person's "ask before a search or a page open", which governs the
/// click question as it does a page open; it never governs typing.
///
/// `Open` is not decided here (see the module doc) and comes back `Run`.
pub fn judge(action: &BrowserAction, facts: &Facts, mail_read: bool, confirm_web: bool) -> Verdict {
    if action.needs_page() && !facts.page_open {
        return Verdict::Refuse("No page is open. Use browser_open first.".into());
    }
    let site = if facts.site.is_empty() { "this page" } else { facts.site.as_str() };
    let element = match action.target() {
        Some(_) => match &facts.element {
            Some(element) => Some(element),
            None => {
                return Verdict::Refuse(
                    "That element is not on the page any more. Look at the page again \
                     (browser_look) and use a reference from what comes back."
                        .into(),
                );
            }
        },
        None => None,
    };
    match action {
        BrowserAction::Type { text, .. } => {
            let element = element.expect("typing is always aimed at something");
            if let Some(secret) = element.secret() {
                return Verdict::Refuse(format!(
                    "That is {secret} on {site}. You never type passwords, one-time codes, card \
                     details or files into a page -- tell them to do that part themselves, in \
                     their own browser, if they trust the site."
                ));
            }
            if !element.editable {
                return Verdict::Refuse(format!(
                    "{} cannot be typed into. Choose a text field.",
                    capitalise(&element.describe())
                ));
            }
            Verdict::Ask {
                kind: "type",
                subject: format!(
                    "\u{201c}{}\u{201d} into {} on {site}",
                    flat(text, 300),
                    element.describe()
                ),
            }
        }
        BrowserAction::Click { .. } | BrowserAction::Select { .. }
            if element.is_some_and(|e| e.secret() == Some("a file upload")) =>
        {
            Verdict::Refuse(
                "That opens a file upload, and you never hand a page a file. Tell them to do \
                 that part themselves."
                    .into(),
            )
        }
        BrowserAction::Click { .. } | BrowserAction::Select { .. } if mail_read && confirm_web => {
            let what = element.map(Element::describe).unwrap_or_else(|| "the page".into());
            Verdict::Ask { kind: "click", subject: format!("{what} on {site}") }
        }
        BrowserAction::Press { key } if mail_read && confirm_web => Verdict::Ask {
            kind: "click",
            subject: format!(
                "the {} key on {site}",
                Key::ALL.iter().find(|(_, k)| k == key).map(|(n, _)| *n).unwrap_or("")
            ),
        },
        _ => Verdict::Run,
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(label: &str) -> Element {
        Element {
            node: 7,
            tag: "input".into(),
            input_type: "text".into(),
            label: label.into(),
            editable: true,
            ..Element::default()
        }
    }

    fn on(element: Element) -> Facts {
        Facts { page_open: true, site: "shop.example".into(), element: Some(element) }
    }

    fn typing(text: &str) -> BrowserAction {
        BrowserAction::Type { target: Target::Ref("e1".into()), text: text.into(), submit: false }
    }

    #[test]
    fn typing_always_asks_and_shows_the_text_the_field_and_the_real_site() {
        for (mail_read, confirm_web) in [(false, false), (false, true), (true, true)] {
            let verdict =
                judge(&typing("running shoes"), &on(field("Search")), mail_read, confirm_web);
            assert_eq!(
                verdict,
                Verdict::Ask {
                    kind: "type",
                    subject: "\u{201c}running shoes\u{201d} into the field \u{201c}Search\u{201d} \
                              on shop.example"
                        .into()
                }
            );
        }
    }

    #[test]
    fn secrets_are_never_typed_whatever_the_field_calls_itself() {
        let cases = [
            Element { input_type: "password".into(), ..field("Anything") },
            Element { autocomplete: "current-password".into(), ..field("") },
            Element { autocomplete: "one-time-code".into(), ..field("") },
            Element { autocomplete: "billing cc-number".into(), ..field("") },
            Element { name: "card_number".into(), ..field("") },
            Element { id: "user-password".into(), ..field("") },
            field("CVV"),
            field("Enter your verification code"),
            field("Social Security number"),
        ];
        for element in cases {
            let verdict = judge(&typing("x"), &on(element.clone()), false, false);
            assert!(matches!(verdict, Verdict::Refuse(_)), "{element:?} was {verdict:?}");
        }
        // And a few that only look like it.
        for label in ["Passenger name", "Passport country", "Discount code", "Card message"] {
            let verdict = judge(&typing("x"), &on(field(label)), false, false);
            assert!(matches!(verdict, Verdict::Ask { .. }), "{label} was {verdict:?}");
        }
        assert_eq!(Element { autocomplete: "cc-name".into(), ..field("") }.secret(), None);
    }

    #[test]
    fn a_button_is_not_typed_into_and_a_file_input_is_not_clicked() {
        let button = Element { tag: "button".into(), editable: false, ..field("Go") };
        assert!(matches!(judge(&typing("x"), &on(button), false, false), Verdict::Refuse(_)));
        let file = Element { input_type: "file".into(), editable: false, ..field("Upload") };
        let click = BrowserAction::Click { target: Target::Ref("e1".into()) };
        assert!(matches!(judge(&click, &on(file), false, false), Verdict::Refuse(_)));
    }

    #[test]
    fn a_click_asks_only_once_mail_has_been_read_and_the_person_wants_asking() {
        let link = Element {
            tag: "a".into(),
            href: "https://shop.example/x".into(),
            editable: false,
            ..field("Next page")
        };
        let click = BrowserAction::Click { target: Target::Ref("e3".into()) };
        assert_eq!(judge(&click, &on(link.clone()), false, true), Verdict::Run);
        assert_eq!(judge(&click, &on(link.clone()), true, false), Verdict::Run);
        assert_eq!(
            judge(&click, &on(link), true, true),
            Verdict::Ask {
                kind: "click",
                subject: "the link \u{201c}Next page\u{201d} on shop.example".into()
            }
        );
        let enter = BrowserAction::Press { key: Key::Enter };
        let page = Facts { page_open: true, site: "shop.example".into(), element: None };
        assert_eq!(judge(&enter, &page, false, true), Verdict::Run);
        assert!(matches!(judge(&enter, &page, true, true), Verdict::Ask { kind: "click", .. }));
    }

    #[test]
    fn nothing_is_done_to_a_page_that_is_not_there_or_an_element_that_has_gone() {
        let click = BrowserAction::Click { target: Target::Ref("e3".into()) };
        assert!(matches!(judge(&click, &Facts::default(), false, false), Verdict::Refuse(_)));
        let gone = Facts { page_open: true, site: "a.example".into(), element: None };
        assert!(matches!(judge(&click, &gone, false, false), Verdict::Refuse(_)));
        let open = BrowserAction::Open { url: "https://a.example".into() };
        assert_eq!(judge(&open, &Facts::default(), true, true), Verdict::Run);
        assert_eq!(judge(&BrowserAction::Close, &Facts::default(), true, true), Verdict::Run);
    }

    #[test]
    fn calls_become_actions_and_a_point_is_a_target_too() {
        let action = BrowserAction::from_call(
            TYPE,
            &json!({ "element": "e4", "text": "hello", "submit": true }),
        )
        .unwrap();
        assert_eq!(
            action,
            BrowserAction::Type {
                target: Target::Ref("e4".into()),
                text: "hello".into(),
                submit: true
            }
        );
        let action = BrowserAction::from_call(CLICK, &json!({ "x": 412, "y": 230.5 })).unwrap();
        assert_eq!(action, BrowserAction::Click { target: Target::Point { x: 412.0, y: 230.5 } });
        assert!(BrowserAction::from_call(CLICK, &json!({})).is_err());
        assert!(BrowserAction::from_call(PRESS, &json!({ "key": "F12" })).is_err());
        assert_eq!(
            BrowserAction::from_call(PRESS, &json!({ "key": "pagedown" })).unwrap(),
            BrowserAction::Press { key: Key::PageDown }
        );
        let long = "x".repeat(MAX_TYPED_CHARS + 1);
        assert!(BrowserAction::from_call(TYPE, &json!({ "element": "e1", "text": long })).is_err());
        assert_eq!(
            BrowserAction::from_call(SCROLL, &json!({})).unwrap(),
            BrowserAction::Scroll { direction: Direction::Down, target: None }
        );
    }
}
