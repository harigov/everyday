//! Turning a message's HTML into something a sandboxed frame can show.
//!
//! This runs once, at sync, never at display (`docs/plans/mail.md`, "Rendering
//! a message") -- the interface is handed a string it can drop straight into
//! `<iframe sandbox srcdoc>` and never parses HTML itself. Two libraries, in
//! order, each doing the part it is actually good at:
//!
//! 1. **[`lol_html`]**, a streaming rewriter, walks every `img src`,
//!    `srcset`, `[background]` and `style` (attribute and `<style>` block)
//!    once and turns every reference to a remote resource into the app's own
//!    `{scheme}://mail/...` protocol. An `http(s)` image becomes
//!    `{scheme}://mail/img/{message id}/{blake3 of the url}`; a `cid:`
//!    reference becomes `{scheme}://mail/part/{message id}/{content id}`.
//!    Both carry the message id as a path segment, not a query parameter --
//!    `mail/img/{token}?m={message}` was this scheme's first shape, and a
//!    route on both transports still accepts it, because a body sanitised
//!    under the old shape is sealed in the vault exactly as it was written
//!    and is never re-sanitised; see [`Shared::proxy`]'s own docs for why
//!    the path form is the one a *new* body writes. Nothing this pass
//!    touches can cause a request to leave the app before the person asks
//!    for it -- the token is opaque, and the protocol handler decides later,
//!    per `docs/plans/mail.md`'s remote-image rule, whether to actually fetch
//!    it. The same pass drops `@import` and CSS `expression()` outright, and
//!    removes images that look like tracking pixels, recording that it did.
//! 2. **[`ammonia`]** then applies an allow-list built for email: tables and
//!    common formatting survive, inline styles survive with their properties
//!    filtered, links get `rel="noopener noreferrer"` and `target="_blank"`,
//!    and everything with behaviour -- `<script>`, `<iframe>`, `<object>`,
//!    `<form>`, event handler attributes, `javascript:` URLs, `<meta
//!    http-equiv="refresh">` -- is gone, because none of those tags are on
//!    the list to begin with.
//!
//! ## Why `<style>` blocks survive
//!
//! A lot of real mail -- newsletters especially -- is laid out with a
//! `<style>` block and classed elements, not inline styles on every tag.
//! Dropping the block (ammonia's default, since `style` is in its
//! `clean_content_tags`) would leave the layout in pieces. So this module
//! puts `style` back on the tag allow-list and takes it back out of
//! `clean_content_tags`, but only after `lol_html` has scrubbed the block's
//! `url()`s and `@import`s the same way it scrubs an inline style -- a
//! `<style>` block is exactly as capable of phoning home through a
//! `background-image` as an `<img>` tag is, and ammonia does not look inside
//! it at all (`filter_style_properties` only ever inspects the `style`
//! *attribute*, not a `<style>` *element*'s content, which is why the
//! rewriting for both happens in the same place: `scrub_css` below).
//!
//! ## What this does not catch
//!
//! - **`data:` images.** Ammonia's default URL scheme list does not include
//!   `data`, so an inline `data:image/png;base64,...` loses its `src`
//!   entirely rather than being shown. That is the safer default -- a
//!   `data:` URI is also how a message could smuggle a large inline payload
//!   past the "remote images are opt-in" rule -- but it does mean a sender
//!   who inlines a logo that way shows a broken image. Worth revisiting if
//!   it turns out to be common.
//! - **Tracking pixels are a heuristic.** A 1x1 image, or one hidden by
//!   `display:none`/`visibility:hidden`/`opacity:0` in its own `style`
//!   attribute, is dropped and counted. A pixel hidden by a rule in a
//!   `<style>` block that targets it by class, or sized by something other
//!   than `width`/`height`/inline `style`, is not detected here -- catching
//!   that needs the same computed-style machinery `text.rs` uses for hidden
//!   text, and is a job for a later pass if it turns out to matter.
//! - **Relative and `data:` URLs inside `style=""`/`<style>` are left as
//!   text.** They cannot resolve to anything inside a sandboxed frame with
//!   no base URL, so there is nothing to protect against; ammonia's own
//!   `UrlRelative` handling covers attributes, and CSS values are outside
//!   its reach either way.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use lol_html::html_content::{ContentType as LolContentType, Element};
use lol_html::{RewriteStrSettings, element, rewrite_str, text};

/// What to rewrite a message's remote and `cid:` references into.
///
/// `scheme` exists so a test (or a future build with a different protocol
/// name) does not have to hard-code `everyday`; production code uses
/// [`Rewrite::new`], which does.
#[derive(Debug, Clone)]
pub struct Rewrite {
    pub message_id: String,
    pub scheme: String,
}

impl Rewrite {
    pub fn new(message_id: impl Into<String>) -> Self {
        Self { message_id: message_id.into(), scheme: "everyday".to_string() }
    }
}

/// A remote image `lol_html` found and proxied, for the interface to decide
/// whether -- and when -- to actually fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    pub original_url: String,
    /// `blake3(original_url)`, hex-encoded: what the rewritten `src` and the
    /// protocol handler's lookup both use, so neither ever needs the
    /// original URL in the clear on the path between them.
    pub token: String,
}

/// The result of [`sanitize`]: HTML ready for the sandboxed frame, and what
/// happened to it along the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sanitised {
    pub html: String,
    pub remote_images: Vec<RemoteImage>,
    pub had_tracking_pixels: bool,
}

/// Sanitises one message body's HTML: rewrite, then allow-list. See the
/// module docs for why it is these two passes in this order.
pub fn sanitize(html: &str, rewrite: &Rewrite) -> Sanitised {
    let shared = Rc::new(RefCell::new(Shared::new(rewrite.clone())));

    // `rewrite_str` can only fail on a handler error (ours never produce
    // one) or a document too pathological for lol_html's own limits. Either
    // way, falling back to the original markup and still running it through
    // ammonia below keeps the frame safe -- the allow-list is what removes
    // anything with behaviour, and it does not depend on the rewrite pass
    // having run. The only thing lost is image proxying for that message.
    let rewritten = rewrite_dangerous_refs(html, &shared).unwrap_or_else(|_| html.to_string());

    let cleaned = ammonia_clean(&rewritten, &rewrite.scheme);

    let shared =
        Rc::try_unwrap(shared).map(RefCell::into_inner).unwrap_or_else(|rc| rc.borrow().clone());

    Sanitised {
        html: cleaned,
        remote_images: shared.remote_images,
        had_tracking_pixels: shared.had_tracking_pixels,
    }
}

#[derive(Debug, Clone)]
struct Shared {
    rewrite: Rewrite,
    remote_images: Vec<RemoteImage>,
    seen: HashSet<String>,
    had_tracking_pixels: bool,
}

impl Shared {
    fn new(rewrite: Rewrite) -> Self {
        Self {
            rewrite,
            remote_images: Vec::new(),
            seen: HashSet::new(),
            had_tracking_pixels: false,
        }
    }

    /// Classifies one URL from `src`, `background`, a `srcset` candidate, or
    /// a CSS `url()`, and returns what to put in its place.
    fn classify(&mut self, url: &str) -> String {
        let trimmed = url.trim();
        let lower = trimmed.to_ascii_lowercase();

        if let Some(rest) = lower.strip_prefix("cid:") {
            let content_id = trimmed[trimmed.len() - rest.len()..].trim();
            return format!(
                "{}://mail/part/{}/{}",
                self.rewrite.scheme,
                path_segment(&self.rewrite.message_id),
                path_segment(content_id)
            );
        }

        if lower.starts_with("http://") || lower.starts_with("https://") {
            return self.proxy(trimmed);
        }

        // Protocol-relative: `//host/path`. Real mail from real newsletter
        // platforms uses this more often than you'd expect, inherited from
        // web templates that assume they're loaded over `https:` already.
        if let Some(rest) = trimmed.strip_prefix("//") {
            return self.proxy(&format!("https://{rest}"));
        }

        // Already rewritten, from a message sanitised twice.
        if lower.starts_with(&format!("{}://mail/", self.rewrite.scheme)) {
            return trimmed.to_string();
        }

        // An inline raster image carries its own bytes and fetches nothing.
        // SVG is not on the list: an SVG document can carry script and its
        // own references.
        const INLINE: [&str; 4] =
            ["data:image/png", "data:image/gif", "data:image/jpeg", "data:image/webp"];
        if INLINE.iter().any(|p| lower.starts_with(p)) {
            return trimmed.to_string();
        }

        // Everything else becomes nothing. Leaving an unrecognised reference
        // as it was is how a scheme nobody thought of -- `ftp:`, a relative
        // path the frame would resolve against something, an escape this
        // scanner never decoded -- turns into a request the person did not
        // choose to make. The frame's CSP would refuse it too; this is the
        // wall before that one.
        String::new()
    }

    /// Rewrites a remote `http(s)` URL to `{scheme}://mail/img/{message
    /// id}/{token}` -- the message id first, matching `.../part/{message
    /// id}/{content id}`'s own order, so both mail routes share one shape
    /// rather than the image route being the odd one out with its id in a
    /// query string. That used to be exactly what it was --
    /// `{scheme}://mail/img/{token}?m={message id}` -- until a token alone
    /// in the path turned out to need a second question answered
    /// (`protocol.rs`'s router and `everyday-server`'s equivalent route
    /// both had to read a query parameter neither of the other two mail
    /// addresses needs) before either could look anything up. Both routes
    /// still answer the old shape too, because a body already sanitised
    /// under it is sealed in the vault verbatim and this module never
    /// re-sanitises a stored body to migrate it.
    fn proxy(&mut self, url: &str) -> String {
        let token = blake3::hash(url.as_bytes()).to_hex().to_string();
        if self.seen.insert(url.to_string()) {
            self.remote_images
                .push(RemoteImage { original_url: url.to_string(), token: token.clone() });
        }
        format!(
            "{}://mail/img/{}/{}",
            self.rewrite.scheme,
            path_segment(&self.rewrite.message_id),
            token
        )
    }
}

fn rewrite_dangerous_refs(
    html: &str,
    shared: &Rc<RefCell<Shared>>,
) -> Result<String, lol_html::errors::RewritingError> {
    let style_buffer = Rc::new(RefCell::new(String::new()));

    let img_src = {
        let shared = Rc::clone(shared);
        element!("img[src]", move |el| {
            if looks_like_tracking_pixel(el) {
                shared.borrow_mut().had_tracking_pixels = true;
                el.remove();
                return Ok(());
            }
            if let Some(src) = el.get_attribute("src") {
                let replacement = shared.borrow_mut().classify(&src);
                el.set_attribute("src", &replacement)?;
            }
            Ok(())
        })
    };

    let img_srcset = {
        let shared = Rc::clone(shared);
        element!("img[srcset]", move |el| {
            if let Some(srcset) = el.get_attribute("srcset") {
                el.set_attribute("srcset", &rewrite_srcset(&srcset, &shared))?;
            }
            Ok(())
        })
    };

    let source_srcset = {
        let shared = Rc::clone(shared);
        element!("source[srcset]", move |el| {
            if let Some(srcset) = el.get_attribute("srcset") {
                el.set_attribute("srcset", &rewrite_srcset(&srcset, &shared))?;
            }
            Ok(())
        })
    };

    let background_attr = {
        let shared = Rc::clone(shared);
        element!("[background]", move |el| {
            if let Some(background) = el.get_attribute("background") {
                let replacement = shared.borrow_mut().classify(&background);
                el.set_attribute("background", &replacement)?;
            }
            Ok(())
        })
    };

    let style_attr = {
        let shared = Rc::clone(shared);
        element!("[style]", move |el| {
            if let Some(style) = el.get_attribute("style") {
                let scrubbed = scrub_css(&style, &mut |url| shared.borrow_mut().classify(url));
                el.set_attribute("style", &scrubbed)?;
            }
            Ok(())
        })
    };

    let style_block = {
        let shared = Rc::clone(shared);
        let buffer = Rc::clone(&style_buffer);
        text!("style", move |chunk| {
            buffer.borrow_mut().push_str(chunk.as_str());
            if chunk.last_in_text_node() {
                let whole = std::mem::take(&mut *buffer.borrow_mut());
                let scrubbed = scrub_css(&whole, &mut |url| shared.borrow_mut().classify(url));
                chunk.replace(&scrubbed, LolContentType::Text);
            } else {
                chunk.remove();
            }
            Ok(())
        })
    };

    let settings = RewriteStrSettings::new()
        .append_element_content_handler(img_src)
        .append_element_content_handler(img_srcset)
        .append_element_content_handler(source_srcset)
        .append_element_content_handler(background_attr)
        .append_element_content_handler(style_attr)
        .append_element_content_handler(style_block);

    rewrite_str(html, settings)
}

/// The properties this module lets an inline `style=""` attribute carry
/// through to the frame: enough for common formatting and email layout,
/// nothing that positions or hides content in a way that matters for
/// anything other than genuine styling (`display` and `visibility` are
/// here deliberately -- see the module docs on hidden content, which is
/// `text.rs`'s job to strip, not this allow-list's).
const ALLOWED_STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background",
    "background-color",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "text-align",
    "text-decoration",
    "text-transform",
    "line-height",
    "letter-spacing",
    "white-space",
    "vertical-align",
    "display",
    "visibility",
    "width",
    "height",
    "max-width",
    "min-width",
    "max-height",
    "min-height",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "border",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-color",
    "border-width",
    "border-style",
    "border-radius",
    "border-collapse",
    "border-spacing",
];

fn ammonia_clean(html: &str, scheme: &str) -> String {
    let schemes = [scheme];
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(&["style"])
        .rm_clean_content_tags(&["style"])
        .add_generic_attributes(&[
            "style",
            "class",
            "id",
            "dir",
            "background",
            "align",
            "valign",
            "border",
            "cellpadding",
            "cellspacing",
            "bgcolor",
            "width",
            "height",
            "color",
            "face",
            "size",
        ])
        .add_tag_attributes("img", &["srcset", "sizes"])
        .add_tag_attributes("source", &["srcset", "sizes", "media", "type"])
        .add_url_schemes(&schemes)
        .set_tag_attribute_value("a", "target", "_blank")
        .filter_style_properties(ALLOWED_STYLE_PROPERTIES.iter().copied().collect());

    builder.clean(html).to_string()
}

/// Heuristics for "this `<img>` exists only to be counted, not seen". A real
/// tracking pixel is 1x1 (or 0x0) or hidden by its own `style`; a decorative
/// spacer or a genuinely tiny icon looks the same to this check, which is
/// why nothing here is destructive to the *message* -- only the one `<img>`
/// tag is removed, never anything the pixel might sit next to.
fn looks_like_tracking_pixel(el: &Element) -> bool {
    let dimension_is_tiny = |attr: &str| {
        el.get_attribute(attr)
            .map(|value| matches!(value.trim().trim_end_matches("px").trim(), "0" | "1"))
            .unwrap_or(false)
    };

    if dimension_is_tiny("width") && dimension_is_tiny("height") {
        return true;
    }

    let Some(style) = el.get_attribute("style") else {
        return false;
    };
    let style = style.to_ascii_lowercase();
    let hidden_by_style = [
        "display:none",
        "display: none",
        "visibility:hidden",
        "visibility: hidden",
        "opacity:0",
        "opacity: 0",
    ];
    if hidden_by_style.iter().any(|needle| style.contains(needle)) {
        return true;
    }

    let width_tiny = style.contains("width:0")
        || style.contains("width:1px")
        || style.contains("width: 0")
        || style.contains("width: 1px");
    let height_tiny = style.contains("height:0")
        || style.contains("height:1px")
        || style.contains("height: 0")
        || style.contains("height: 1px");
    width_tiny && height_tiny
}

fn rewrite_srcset(value: &str, shared: &Rc<RefCell<Shared>>) -> String {
    value
        .split(',')
        .filter_map(|candidate| {
            let candidate = candidate.trim();
            if candidate.is_empty() {
                return None;
            }
            Some(match candidate.split_once(char::is_whitespace) {
                Some((url, descriptor)) => {
                    format!("{} {}", shared.borrow_mut().classify(url), descriptor.trim())
                }
                None => shared.borrow_mut().classify(candidate),
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Scrubs the CSS-hazard surface shared by a `style=""` attribute and a
/// `<style>` block's content: `@import` is dropped outright (it would fetch
/// a whole stylesheet, the same tracking-and-exfiltration shape as a remote
/// image but for text no one asked to load), `expression(...)` is dropped
/// outright (an Internet Explorer relic that ran arbitrary script from a
/// CSS value; nothing legitimate in 2026 mail needs it), and every `url()`
/// goes through the same [`Shared::classify`] an `<img src>` does.
///
/// This is a hand-rolled scanner, not a CSS parser -- the grammar it needs
/// to understand is three keywords and balanced parentheses, and a real
/// parser would need to also *re-serialise* correctly-parsed CSS, which is
/// far more surface for something to go subtly wrong on hostile input.
///
/// Three things make that small grammar honest. Comments are removed first,
/// so `url/**/(` cannot hide a reference between two tokens. CSS with a
/// backslash anywhere is dropped whole, because escapes (`u\72l(`,
/// `@\69mport`) are the other way to spell a keyword this scanner looks
/// for, and decoding them correctly is the parser this is avoiding;
/// legitimate mail almost never needs one. And `image-set(` fetches without
/// saying `url(`, so it is dropped like `expression(`.
fn scrub_css(css: &str, on_url: &mut dyn FnMut(&str) -> String) -> String {
    if css.contains('\\') {
        return String::new();
    }
    let css = strip_css_comments(css);
    let css = css.as_str();
    let mut out = String::with_capacity(css.len());
    let mut i = 0usize;

    while i < css.len() {
        let rest = &css[i..];

        if ci_starts_with(rest, "@import") {
            let end = rest.find(';').map(|pos| pos + 1).unwrap_or(rest.len());
            i += end;
            continue;
        }

        if let Some(name) =
            ["expression", "-webkit-image-set", "image-set"].into_iter().find(|name| {
                ci_starts_with(rest, name) && rest.as_bytes().get(name.len()) == Some(&b'(')
            })
        {
            match matching_close_paren(rest, name.len()) {
                Some(close) => i += close + 1,
                // Unclosed: nothing after it can be trusted to mean what it says.
                None => break,
            }
            continue;
        }

        if ci_starts_with(rest, "url(") {
            if let Some(close) = matching_close_paren(rest, "url".len()) {
                let inner = unquote(rest["url(".len()..close].trim());
                let replacement = on_url(inner);
                out.push_str("url(\"");
                out.push_str(&replacement.replace('"', "%22"));
                out.push_str("\")");
                i += close + 1;
                continue;
            }
            // An unclosed `url(` swallows the rest of the text in a browser;
            // here it ends it.
            break;
        }

        let ch = rest.chars().next().expect("i < css.len() implies a char here");
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// `/* ... */` removed, and an unterminated comment taken to the end, as a
/// browser does.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Percent-encodes everything but the characters a message or content id is
/// normally made of, so an id cannot close a quote, add a path segment, or
/// start a query in the URL it is written into.
fn path_segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || b"@._-+".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn ci_starts_with(haystack: &str, needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

/// `s[open_paren_at]` must be `(`. Returns the byte offset of the matching
/// `)`, respecting quoted strings so a `)` inside `url("a)b.png")` does not
/// end things early.
fn matching_close_paren(s: &str, open_paren_at: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(open_paren_at) != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = open_paren_at;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q && bytes.get(i.wrapping_sub(1)) != Some(&b'\\') {
                quote = None;
            }
        } else {
            match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite() -> Rewrite {
        Rewrite::new("msg-1@example.com")
    }

    #[test]
    fn a_script_tag_does_not_survive() {
        let out = sanitize(r#"<p>hi</p><script>alert(1)</script>"#, &rewrite());
        assert!(!out.html.contains("script"));
        assert!(!out.html.contains("alert"));
    }

    #[test]
    fn an_onerror_handler_does_not_survive() {
        let out =
            sanitize(r#"<img src="https://example.com/a.png" onerror="alert(1)">"#, &rewrite());
        assert!(!out.html.contains("onerror"));
    }

    #[test]
    fn a_javascript_link_does_not_survive() {
        let out = sanitize(r#"<a href="javascript:alert(1)">click</a>"#, &rewrite());
        assert!(!out.html.contains("javascript:"));
    }

    #[test]
    fn an_svg_with_a_script_does_not_survive() {
        let out = sanitize(r#"<svg onload="alert(1)"><script>alert(2)</script></svg>"#, &rewrite());
        assert!(!out.html.contains("script"));
        assert!(!out.html.contains("alert"));
    }

    #[test]
    fn an_at_import_is_dropped_from_a_style_block() {
        let out = sanitize(
            r#"<style>@import url("https://evil.example/track.css"); p { color: red; }</style>"#,
            &rewrite(),
        );
        assert!(!out.html.to_ascii_lowercase().contains("@import"));
        assert!(!out.html.contains("evil.example"));
        // The rest of the block survives -- that's the point of keeping <style> at all.
        assert!(out.html.contains("color"));
    }

    #[test]
    fn a_css_expression_is_dropped() {
        let out = sanitize(r#"<div style="width: expression(alert(1))">hi</div>"#, &rewrite());
        assert!(!out.html.contains("expression"));
        assert!(!out.html.contains("alert"));
    }

    #[test]
    fn a_css_escape_cannot_spell_a_reference_past_the_scanner() {
        let out = sanitize(
            r#"<div style="background: u\72l(https://evil.example/a.png)">hi</div><style>@\69mport 'https://evil.example/x.css';</style>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"));
    }

    #[test]
    fn a_comment_cannot_split_url_from_its_parenthesis() {
        let out = sanitize(
            r#"<div style="background: url/**/(https://evil.example/a.png)">hi</div>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"));
    }

    #[test]
    fn image_set_and_unclosed_url_fetch_nothing() {
        let out = sanitize(
            r#"<style>p { background: image-set("https://evil.example/a.png" 1x); } q { background: url(https://evil.example/b.png</style>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"));
    }

    #[test]
    fn a_scheme_nobody_listed_becomes_nothing() {
        let out =
            sanitize(r#"<img src="ftp://evil.example/a.png" width="200" height="60">"#, &rewrite());
        assert!(!out.html.contains("evil.example"));
        assert!(out.remote_images.is_empty());
    }

    #[test]
    fn a_content_id_cannot_break_out_of_its_url() {
        let out = sanitize(r#"<img src='cid:a"/../b?x'>"#, &rewrite());
        assert!(out.html.contains("everyday://mail/part/msg-1@example.com/a%22%2F..%2Fb%3Fx"));
    }

    #[test]
    fn a_tracking_pixel_is_removed_and_counted() {
        let out = sanitize(
            r#"<p>hi</p><img src="https://track.example/open.gif" width="1" height="1">"#,
            &rewrite(),
        );
        assert!(out.had_tracking_pixels);
        assert!(!out.html.contains("track.example"));
        assert!(out.remote_images.is_empty());
    }

    #[test]
    fn a_hidden_pixel_by_style_is_also_removed() {
        let out = sanitize(
            r#"<img src="https://track.example/open.gif" style="display:none">"#,
            &rewrite(),
        );
        assert!(out.had_tracking_pixels);
    }

    #[test]
    fn a_normal_remote_image_is_proxied_not_dropped() {
        let out = sanitize(
            r#"<img src="https://cdn.example.com/logo.png" width="200" height="60">"#,
            &rewrite(),
        );
        assert!(!out.had_tracking_pixels);
        assert_eq!(out.remote_images.len(), 1);
        assert_eq!(out.remote_images[0].original_url, "https://cdn.example.com/logo.png");
        assert!(out.html.contains(&format!(
            "everyday://mail/img/msg-1@example.com/{}",
            out.remote_images[0].token
        )));
        assert!(!out.html.contains("cdn.example.com"));
    }

    #[test]
    fn a_remote_images_address_carries_the_message_id_in_its_path() {
        // The shape `everyday-app/src/protocol.rs` and
        // `everyday-server/src/routes.rs` both parse without a `?m=` query
        // parameter -- see `Shared::proxy`'s own docs for why the message id
        // moved into the path.
        let out = sanitize(r#"<img src="https://cdn.example.com/a.png">"#, &rewrite());
        assert!(!out.html.contains("?m="));
        let expected =
            format!("everyday://mail/img/msg-1@example.com/{}", out.remote_images[0].token);
        assert!(out.html.contains(&expected), "{}", out.html);
    }

    #[test]
    fn a_message_id_needing_escaping_is_percent_encoded_in_the_image_address() {
        // Most real `Message-ID` headers are already path-safe, but nothing
        // stops a server minting one with a `/` or a space in it -- and this
        // is the same `path_segment` the `cid:` address already relies on
        // to keep a hostile id from adding a path segment of its own.
        let out = sanitize(
            r#"<img src="https://cdn.example.com/a.png">"#,
            &Rewrite::new("weird id/with space"),
        );
        assert!(out.html.contains("everyday://mail/img/weird%20id%2Fwith%20space/"));
    }

    #[test]
    fn a_cid_image_points_at_the_message_and_its_part() {
        let out = sanitize(r#"<img src="cid:logo@example.com">"#, &rewrite());
        assert!(out.html.contains("everyday://mail/part/msg-1@example.com/logo@example.com"));
    }

    #[test]
    fn srcset_candidates_are_each_proxied() {
        let out = sanitize(
            r#"<img src="https://cdn.example.com/a.png" srcset="https://cdn.example.com/a.png 1x, https://cdn.example.com/a@2x.png 2x">"#,
            &rewrite(),
        );
        assert_eq!(out.remote_images.len(), 2);
        assert!(!out.html.contains("cdn.example.com"));
        assert!(out.html.contains("1x"));
        assert!(out.html.contains("2x"));
    }

    #[test]
    fn tables_and_inline_formatting_survive() {
        let out = sanitize(
            r#"<table><tr><td style="color:red;font-weight:bold">Total</td></tr></table>"#,
            &rewrite(),
        );
        assert!(out.html.contains("<table>"));
        assert!(out.html.contains("color:red") || out.html.contains("color: red"));
    }

    #[test]
    fn links_get_target_blank_and_noopener() {
        let out = sanitize(r#"<a href="https://example.com">hi</a>"#, &rewrite());
        assert!(out.html.contains("target=\"_blank\""));
        assert!(out.html.contains("noopener"));
    }
}
