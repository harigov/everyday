//! The XPath a site rule names an element by, as the CSS selector `dom_query`
//! runs -- for the part of XPath that CSS can say.
//!
//! FiveFilters' rules (`rules.rs`) are XPath 1.0, and nearly all of them are
//! plain paths: `//div[@id='story']//p`, a class test written out longhand,
//! a `contains` on an attribute, a union of two of those. Those translate
//! one for one, and that is all this does -- with one test on text that
//! `dom_query` has its own pseudo-class for, `contains(., 'words')` as
//! `:contains("words")`. The rest -- an axis
//! (`parent::`, `following-sibling::`), a test on text, string functions,
//! arithmetic on positions -- is refused with `None` rather than guessed at,
//! and a rule that is refused is simply not used: the site's other rules and
//! Readability are still there.
//!
//! A positional predicate is kept only where CSS means the same thing: first
//! in its step, where `div[2]` is the second `div` among its siblings, which
//! is `div:nth-of-type(2)`. After a filter (`div[@class='a'][2]`) XPath
//! counts only what passed the filter, and CSS cannot.

/// What to read from each element a rule's path selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// The element itself: a body to keep, or one to strip.
    Element,
    /// One of its attributes, for a path ending `/@name`.
    Attribute(String),
    /// Its text, for a path ending `/text()`.
    Text,
}

/// One path of a rule, as CSS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Css {
    pub selector: String,
    pub read: Read,
}

/// `xpath` as CSS, one entry for each path of a union (`a | b`), or `None`
/// when any of it is beyond what CSS can say.
pub fn to_css(xpath: &str) -> Option<Vec<Css>> {
    let parts = split_union(xpath)?;
    parts.iter().map(|part| path(part)).collect()
}

/// The paths of a union, split at each `|` that is not inside a predicate,
/// a call or a string.
fn split_union(xpath: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None::<u8>, 0);
    for (i, b) in xpath.bytes().enumerate() {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(b),
            (None, b'[' | b'(') => depth += 1,
            (None, b']' | b')') => depth -= 1,
            (None, b'|') if depth == 0 => {
                parts.push(xpath[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return None;
    }
    parts.push(xpath[start..].trim());
    parts.iter().all(|p| !p.is_empty()).then_some(parts)
}

/// One location path: steps joined by `/` (a child) or `//` (anywhere
/// below), ending at an element, an attribute or text.
fn path(src: &str) -> Option<Css> {
    let mut p = Parser { s: src.as_bytes(), i: 0 };
    let rooted = if p.eat(".//") || p.eat("//") || p.eat("./") { false } else { p.eat("/") };
    let mut selector = String::new();
    // The combinator before the next step, written only once that step turns
    // out to be an element's and not the attribute or text a path ends at.
    let mut combinator = "";
    loop {
        if p.peek() == Some(b'@') {
            p.i += 1;
            let name = p.name()?;
            return (!selector.is_empty() && p.done())
                .then_some(Css { selector, read: Read::Attribute(name) });
        }
        if p.eat("text()") {
            return (!selector.is_empty() && p.done())
                .then_some(Css { selector, read: Read::Text });
        }
        let next = step(&mut p)?;
        selector.push_str(combinator);
        selector.push_str(&next);
        if p.done() {
            break;
        }
        combinator = if p.eat("//") {
            " "
        } else if p.eat("/") {
            " > "
        } else {
            return None;
        };
    }
    // `/html/body/...` from the root is the same path anywhere; any other
    // root is not one an HTML page has.
    if rooted && !(selector == "html" || selector.starts_with("html ")) {
        return None;
    }
    Some(Css { selector, read: Read::Element })
}

/// One step: an element's name or `*`, then its predicates.
fn step(p: &mut Parser) -> Option<String> {
    let tag = if p.eat("*") { String::new() } else { p.name()?.to_ascii_lowercase() };
    if p.peek() == Some(b':') || p.peek() == Some(b'(') {
        // An axis (`parent::`) or a function in the place of a step.
        return None;
    }
    let mut out = tag.clone();
    let mut first = true;
    while p.skip_ws_then(b'[') {
        out.push_str(&predicate(p, &tag, first)?);
        first = false;
    }
    if out.is_empty() {
        out.push('*');
    }
    Some(out)
}

/// `[...]`, as the CSS for the same test, the opening bracket already taken.
fn predicate(p: &mut Parser, tag: &str, first: bool) -> Option<String> {
    p.ws();
    if let Some(n) = p.number() {
        p.ws();
        if !p.eat("]") || !first || n == 0 {
            return None;
        }
        return Some(if tag.is_empty() {
            format!(":nth-child({n})")
        } else {
            format!(":nth-of-type({n})")
        });
    }
    let test = or_expr(p)?;
    p.ws();
    p.eat("]").then_some(test)
}

fn or_expr(p: &mut Parser) -> Option<String> {
    let mut alternatives = vec![and_expr(p)?];
    while p.keyword("or") {
        alternatives.push(and_expr(p)?);
    }
    Some(if alternatives.len() == 1 {
        alternatives.remove(0)
    } else {
        format!(":is({})", alternatives.join(", "))
    })
}

fn and_expr(p: &mut Parser) -> Option<String> {
    let mut all = unary(p)?;
    while p.keyword("and") {
        all.push_str(&unary(p)?);
    }
    Some(all)
}

fn unary(p: &mut Parser) -> Option<String> {
    p.ws();
    if p.keyword_call("not") {
        let inner = or_expr(p)?;
        p.ws();
        return p.eat(")").then(|| format!(":not({inner})"));
    }
    if p.eat("(") {
        let inner = or_expr(p)?;
        p.ws();
        return p.eat(")").then_some(inner);
    }
    atom(p)
}

/// One test on the element's attributes.
fn atom(p: &mut Parser) -> Option<String> {
    p.ws();
    if p.eat("@") {
        let name = p.name()?;
        p.ws();
        if p.eat("!=") {
            p.ws();
            // False in XPath for an element without the attribute at all.
            return Some(format!("[{name}]:not([{name}={}])", quoted(&p.literal()?)));
        }
        if p.eat("=") {
            p.ws();
            return Some(format!("[{name}={}]", quoted(&p.literal()?)));
        }
        return Some(format!("[{name}]"));
    }
    if p.keyword_call("contains") {
        p.ws();
        if p.peek() == Some(b'.') && !p.s.get(p.i + 1).is_some_and(|&b| b == b'/' || b == b'.') {
            // `contains(., 'words')`: the element's text, its children's
            // included, has them -- `dom_query`'s own `:contains`.
            p.i += 1;
            p.ws();
            if !p.eat(",") {
                return None;
            }
            p.ws();
            let words = p.literal()?;
            p.ws();
            return (p.eat(")") && !words.is_empty())
                .then(|| format!(":contains({})", quoted(&words)));
        }
        if p.keyword_call("concat") {
            // `contains(concat(' ', normalize-space(@class), ' '), ' word ')`,
            // the longhand for "has this class".
            let name = word_test_attribute(p)?;
            p.ws();
            if !p.eat(",") {
                return None;
            }
            p.ws();
            let word = p.literal()?;
            p.ws();
            let word = word.trim();
            let single = !word.is_empty() && !word.contains(char::is_whitespace);
            return (p.eat(")") && single).then(|| format!("[{name}~={}]", quoted(word)));
        }
        let name = attribute_argument(p)?;
        let value = p.literal()?;
        p.ws();
        if !p.eat(")") {
            return None;
        }
        return Some(if value.is_empty() {
            format!("[{name}]")
        } else {
            format!("[{name}*={}]", quoted(&value))
        });
    }
    if p.keyword_call("starts-with") {
        let name = attribute_argument(p)?;
        let value = p.literal()?;
        p.ws();
        return p.eat(")").then(|| format!("[{name}^={}]", quoted(&value)));
    }
    None
}

/// `@name ,` -- a function's first argument when it is an attribute -- with
/// the space after the comma taken.
fn attribute_argument(p: &mut Parser) -> Option<String> {
    p.ws();
    if !p.eat("@") {
        return None;
    }
    let name = p.name()?;
    p.ws();
    if !p.eat(",") {
        return None;
    }
    p.ws();
    Some(name)
}

/// `' ', normalize-space(@name), ' ')` inside the `concat(` of a word test,
/// giving the attribute's name.
fn word_test_attribute(p: &mut Parser) -> Option<String> {
    let space = |p: &mut Parser| {
        p.ws();
        p.literal().is_some_and(|s| s == " ")
    };
    let comma = |p: &mut Parser| {
        p.ws();
        p.eat(",")
    };
    if !(space(p) && comma(p)) {
        return None;
    }
    p.ws();
    if !p.keyword_call("normalize-space") {
        return None;
    }
    p.ws();
    if !p.eat("@") {
        return None;
    }
    let name = p.name()?;
    p.ws();
    if !(p.eat(")") && comma(p) && space(p)) {
        return None;
    }
    p.ws();
    p.eat(")").then_some(name)
}

/// A CSS string holding `value`.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            // A line break cannot be in a CSS string as itself.
            '\n' | '\r' | '\x0c' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        let found = self.s[self.i..].starts_with(token.as_bytes());
        if found {
            self.i += token.len();
        }
        found
    }

    /// Past any space, whether `b` is next; taken if it is.
    fn skip_ws_then(&mut self, b: u8) -> bool {
        let at = self.i;
        self.ws();
        if self.peek() == Some(b) {
            self.i += 1;
            true
        } else {
            self.i = at;
            false
        }
    }

    /// `word`, after any space, as a whole word: `or` but not `order`.
    fn keyword(&mut self, word: &str) -> bool {
        let at = self.i;
        self.ws();
        let whole = self.s[self.i..].starts_with(word.as_bytes())
            && !self.s.get(self.i + word.len()).is_some_and(|&b| is_name_byte(b));
        if whole {
            self.i += word.len();
        } else {
            self.i = at;
        }
        whole
    }

    /// `name(`, after any space: a call to the function `name`.
    fn keyword_call(&mut self, name: &str) -> bool {
        let at = self.i;
        if self.keyword(name) {
            self.ws();
            if self.eat("(") {
                return true;
            }
        }
        self.i = at;
        false
    }

    /// An element or attribute name that is also a CSS identifier, so it can
    /// be written into a selector as it is. A namespaced name is refused.
    fn name(&mut self) -> Option<String> {
        let start = self.i;
        while self.peek().is_some_and(is_name_byte) {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        let first = name.bytes().next()?;
        (first.is_ascii_alphabetic() || first == b'_').then(|| name.to_string())
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    /// A string in single or double quotes, without them.
    fn literal(&mut self) -> Option<String> {
        let quote = self.peek().filter(|&b| b == b'\'' || b == b'"')?;
        let start = self.i + 1;
        let len = self.s[start..].iter().position(|&b| b == quote)?;
        self.i = start + len + 1;
        std::str::from_utf8(&self.s[start..start + len]).ok().map(str::to_string)
    }
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(xpath: &str) -> Option<String> {
        let parts = to_css(xpath)?;
        assert!(parts.iter().all(|c| c.read == Read::Element), "{xpath}: {parts:?}");
        Some(parts.into_iter().map(|c| c.selector).collect::<Vec<_>>().join(", "))
    }

    #[test]
    fn plain_paths_become_the_same_selector() {
        let cases = [
            ("//article", "article"),
            (
                "//div[@id='content-left']/div[@class='post']",
                "div[id=\"content-left\"] > div[class=\"post\"]",
            ),
            ("//*[(@id = \"articlebody\")]", "[id=\"articlebody\"]"),
            ("//main//p", "main p"),
            (".//div[@itemprop]", "div[itemprop]"),
            ("/html/body/div", "html > body > div"),
            ("//div[contains(@class, 'post-content')]", "div[class*=\"post-content\"]"),
            (
                "//div[contains(concat(' ',normalize-space(@class),' '),' article-body ')]",
                "div[class~=\"article-body\"]",
            ),
            (
                "//article[contains(concat(' ', normalize-space(@id), ' '), ' post ')]",
                "article[id~=\"post\"]",
            ),
            ("//a[starts-with(@href, '/tag/')]", "a[href^=\"/tag/\"]"),
            (
                "//div[@data-type='x' and @role!='nav']",
                "div[data-type=\"x\"][role]:not([role=\"nav\"])",
            ),
            ("//p[@class='a' or @class='b']", "p:is([class=\"a\"], [class=\"b\"])"),
            ("//div[not(contains(@class,'ad'))]", "div:not([class*=\"ad\"])"),
            ("//p[contains(., 'Read more')]", "p:contains(\"Read more\")"),
            ("//article[1]", "article:nth-of-type(1)"),
            ("//*[2]", ":nth-child(2)"),
            (
                "//section[@name=\"articleBody\"] | //header/figure[contains(@class, 'sizeMedium')]",
                "section[name=\"articleBody\"], header > figure[class*=\"sizeMedium\"]",
            ),
        ];
        for (xpath, want) in cases {
            assert_eq!(css(xpath).as_deref(), Some(want), "{xpath}");
        }
    }

    #[test]
    fn a_value_is_read_from_the_end_of_a_path() {
        assert_eq!(
            to_css("//meta[@name=\"byl\"]/@content"),
            Some(vec![Css {
                selector: "meta[name=\"byl\"]".into(),
                read: Read::Attribute("content".into())
            }])
        );
        assert_eq!(
            to_css("//span[@class='by']//text()"),
            Some(vec![Css { selector: "span[class=\"by\"]".into(), read: Read::Text }])
        );
        assert_eq!(to_css("@content"), None, "an attribute of nothing");
    }

    #[test]
    fn what_css_cannot_say_is_refused() {
        for xpath in [
            "//div[@x]/parent::section",
            "//h2[.='Details']/following-sibling::div",
            "//div[@class='single']/*[not(self::figure)]",
            "//div[@class='a'][2]",
            "(//div)[1]",
            "//p[last()]",
            "//*[@*='x']",
            "//span[text()='By']",
            "//p[contains(.//a, 'x')]",
            "//div[substring-after(@id, 'x')]",
            "//div[@class='story_text']/..",
            "//div[contains(concat(' ',normalize-space(@class),' '),' two words ')]",
            "//div[@id='unclosed",
            "//div[",
            "//div | ",
            "/div",
            "//xml:lang",
        ] {
            assert_eq!(to_css(xpath), None, "{xpath}");
        }
    }

    #[test]
    fn a_value_cannot_break_out_of_its_string() {
        assert_eq!(css(r#"//div[@title='a"b\c']"#).as_deref(), Some(r#"div[title="a\"b\\c"]"#));
        // A bar inside a string is not a union.
        assert_eq!(css("//a[@title='x|y']").as_deref(), Some("a[title=\"x|y\"]"));
    }

    #[test]
    fn every_selector_made_is_one_the_html_reader_accepts() {
        for xpath in [
            "//p[@class='a' or @class='b']",
            "//div[not(contains(@class,'ad'))]",
            "//div[contains(concat(' ',normalize-space(@class),' '),' article-body ')]",
            "//*[2]",
            r#"//div[@title='a"b\c']"#,
            "//p[contains(., 'it\"s')]",
        ] {
            for part in to_css(xpath).unwrap() {
                assert!(dom_query::Matcher::new(&part.selector).is_ok(), "{}", part.selector);
            }
        }
    }
}
