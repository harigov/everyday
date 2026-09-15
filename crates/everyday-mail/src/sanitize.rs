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

/// Sanitises one message body's HTML: rewrite, then allow-list, then guard.
/// See the module docs for why it is these passes in this order.
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

    // The final wall: see `guard_style_urls`'s own docs for what this
    // catches that the two passes above should already have caught, and
    // why it is worth running anyway.
    let guarded = guard_style_urls(&cleaned, &rewrite.scheme).unwrap_or_else(|_| cleaned.clone());

    let shared =
        Rc::try_unwrap(shared).map(RefCell::into_inner).unwrap_or_else(|rc| rc.borrow().clone());

    Sanitised {
        html: guarded,
        remote_images: shared.remote_images,
        had_tracking_pixels: shared.had_tracking_pixels,
    }
}

/// The final wall, run once more after ammonia has finished: even though
/// every `url()` this module's own rewrite pass sees is already turned into
/// `{scheme}://mail/...` or dropped, a bug anywhere upstream of this line --
/// a character reference [`crate::entities`]'s small table does not cover,
/// a CSS construct [`scrub_css`] parses differently from how ammonia
/// re-serialises it, a property [`ALLOWED_STYLE_PROPERTIES`] was wrong to
/// let through -- would otherwise reach the frame as a live reference this
/// module never decided to allow. This pass does not try to be clever about
/// *why* a `url()` might have survived; it only checks *where* each
/// surviving one points, against exactly the two destinations a rendered
/// message is ever allowed to load from: this application's own protocol,
/// and the handful of inline raster image types [`Shared::classify`]
/// already lets through unproxied. Anything else -- an `https:` URL that
/// slipped past the rewrite, a scheme CSS should never have carried to
/// begin with -- is dropped rather than guessed at, the same conservative
/// default [`Shared::classify`]'s own "everything else becomes nothing"
/// applies.
///
/// Built on [`scrub_css`] rather than a second scanner: the grammar this
/// needs to recognise (`@import`, `expression(`, `image-set(`, `url(`) is
/// identical to the first pass's, so reusing it is less surface for the two
/// to quietly drift apart than writing the same scan twice.
fn guard_style_urls(html: &str, scheme: &str) -> Result<String, lol_html::errors::RewritingError> {
    let scheme_for_attr = scheme.to_string();
    let style_attr = element!("[style]", move |el| {
        if let Some(style) = el.get_attribute("style") {
            let guarded = scrub_css(&style, &mut |url| guard_one_url(url, &scheme_for_attr));
            el.set_attribute("style", &guarded)?;
        }
        Ok(())
    });

    let scheme_for_block = scheme.to_string();
    let style_buffer = Rc::new(RefCell::new(String::new()));
    let style_block = text!("style", move |chunk| {
        style_buffer.borrow_mut().push_str(chunk.as_str());
        if chunk.last_in_text_node() {
            let whole = std::mem::take(&mut *style_buffer.borrow_mut());
            let guarded = scrub_css(&whole, &mut |url| guard_one_url(url, &scheme_for_block));
            // `ContentType::Html`, not `Text` -- see `make_style_content_html_safe`'s
            // own docs for why this has to be paired with that function
            // rather than writing `guarded` back verbatim.
            chunk.replace(&make_style_content_html_safe(&guarded), LolContentType::Html);
        } else {
            chunk.remove();
        }
        Ok(())
    });

    let settings = RewriteStrSettings::new()
        .append_element_content_handler(style_attr)
        .append_element_content_handler(style_block);
    rewrite_str(html, settings)
}

/// `scrub_css`'s `on_url` callback for [`guard_style_urls`]: keeps a `url()`
/// exactly as written when it already points somewhere this pass allows,
/// drops it otherwise.
fn guard_one_url(url: &str, scheme: &str) -> String {
    if is_safe_css_url(url, scheme) { url.to_string() } else { String::new() }
}

/// The only two kinds of destination a `url()` surviving to this point is
/// allowed to name: this application's own protocol (everything
/// [`Shared::classify`] proxies or points a `cid:` at is written under it),
/// and an inline raster image -- the same four MIME types
/// [`Shared::classify`]'s own `INLINE` list carries an image's bytes
/// without fetching anything.
fn is_safe_css_url(url: &str, scheme: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with(&format!("{scheme}://")) {
        return true;
    }
    const SAFE_DATA_IMAGES: [&str; 4] =
        ["data:image/png", "data:image/gif", "data:image/jpeg", "data:image/webp"];
    SAFE_DATA_IMAGES.iter().any(|p| lower.starts_with(p))
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
                // Decoded before classification for the same reason the
                // `style` attribute is, just below: `get_attribute` hands
                // back the attribute's *source* text, so `?w=600&amp;h=300`
                // arrives with its `&amp;` still spelled out. Left alone,
                // that entity-encoded ampersand ends up hashed and stored
                // as part of the image's `original_url`, and then fetched
                // by the protocol handler literally -- a server asked for
                // `?w=600&amp;h=300` either serves nothing at that query or
                // silently ignores everything after `&amp;h=300` as one
                // opaque parameter, either way not the image the sender
                // meant. Decoding once, here, before the URL is ever
                // hashed or classified, is what keeps the token and the
                // fetch in agreement with what the link actually says.
                let decoded = crate::entities::decode_entities(&src);
                let replacement = shared.borrow_mut().classify(&decoded);
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
                // See the `src` handler above for why this decodes first.
                let decoded = crate::entities::decode_entities(&background);
                let replacement = shared.borrow_mut().classify(&decoded);
                el.set_attribute("background", &replacement)?;
            }
            Ok(())
        })
    };

    let style_attr = {
        let shared = Rc::clone(shared);
        element!("[style]", move |el| {
            if let Some(style) = el.get_attribute("style") {
                // `lol_html::Element::get_attribute` hands back the
                // attribute's source text, character references and all --
                // decoded once, here, before `scrub_css` ever sees it. See
                // `crate::entities`'s module docs for why this has to
                // happen before the scan rather than trusting ammonia's own
                // decoding downstream.
                let decoded = crate::entities::decode_entities(&style);
                let scrubbed = scrub_css(&decoded, &mut |url| shared.borrow_mut().classify(url));
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
                // See `make_style_content_html_safe`'s own docs: `Html`,
                // paired with that function, is what keeps `td > p`
                // surviving this pass without also reopening the door
                // `Text` closed on `</style` and friends.
                chunk.replace(&make_style_content_html_safe(&scrubbed), LolContentType::Html);
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
    // Decoded for the same reason the `style_attr` rewrite handler decodes
    // before scrubbing -- see `crate::entities`'s module docs -- so an
    // entity-encoded `display&#58;none` cannot hide a tracking pixel from
    // this check any more than it can hide the `url()` scrub.
    let style = crate::entities::decode_entities(&style).to_ascii_lowercase();
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

/// Rewrites every `srcset` candidate, decoding each URL first for the same
/// reason the `src`, `background` and `style` handlers all do -- see the
/// `img_src` handler's own doc in [`rewrite_dangerous_refs`]. Decoding
/// happens per candidate, after the list is split on `,`, because a decoded
/// `&amp;` could otherwise be mistaken for punctuation the splitter itself
/// looks for; nothing in the small entity table this crate decodes spells a
/// comma or whitespace, but splitting first keeps that true by construction
/// rather than by checking the table.
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
                    let decoded = crate::entities::decode_entities(url);
                    format!("{} {}", shared.borrow_mut().classify(&decoded), descriptor.trim())
                }
                None => {
                    let decoded = crate::entities::decode_entities(candidate);
                    shared.borrow_mut().classify(&decoded)
                }
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

/// Makes already-`scrub_css`-cleaned CSS safe to write back into a
/// `<style>` element with `ContentType::Html` rather than `ContentType::Text`.
///
/// A `<style>` block is CDATA to a browser, not prose: `lol_html`'s
/// `ContentType::Text` HTML-escapes `>`, `&` and `<` the way it would for
/// any other text node, which is exactly right for a paragraph and exactly
/// wrong for CSS, where `td > p { }` means something specific and
/// `td &gt; p { }` means nothing to a CSS parser at all -- worse, run
/// through this same pass a second time (a message re-sanitised, or one
/// piped through twice by an interface that does not know it already has
/// been), the `&gt;` becomes `&amp;gt;`, and the child selector is gone for
/// good. `ContentType::Html` is the fix -- it writes the string back
/// verbatim, the way the rest of this module's `url()` rewriting already
/// relies on `<style>` content being read (`text!` hands over raw source
/// bytes, no entity decoding, because raw text elements are never decoded
/// by an HTML tokeniser in the first place; see the module docs' note on
/// why `style_attr`'s decode step and a `<style>` block's lack of one are
/// both correct for the same reason).
///
/// Writing arbitrary bytes back as `Html`, though, hands whatever is in
/// `css` the power an HTML tokeniser gives literal markup -- so before that
/// happens, this walks the string once and defuses the three shapes that
/// matter for a value about to be dropped straight into a raw-text
/// element's content:
///
/// - **`</style`**, matched case-insensitively and tolerant of whitespace
///   between the `/` and the tag name (`</  STYLE`, `</\tstyle`) the same
///   way a real tokeniser is when it is hunting for this element's
///   appropriate end tag. Finding one and leaving it alone is how CSS
///   text -- attacker-controlled, since it came from a message -- ends the
///   `<style>` element on its own terms and hands everything after it to
///   ordinary HTML parsing instead of CSS parsing.
/// - **`<!--` and `-->`**, because once `</style` above has closed the
///   element (or a re-parse downstream disagrees with this pass about
///   where it closes), a stray HTML comment marker is one more way to hide
///   a `<script>` from a first pass while a second, more lenient one still
///   finds it.
/// - **`<script`**, belt and braces: no legitimate CSS value is ever
///   spelled this way, so there is no fidelity lost by refusing to let it
///   through regardless of what closed or did not close around it.
///
/// Each is *neutralised* rather than deleted, so ordinary CSS that merely
/// looks similar -- a comment, a stray string -- keeps as much of its
/// original shape as possible: `</style` gets a zero-width space spliced
/// between `<` and `/`, invisible to a person reading the rendered page but
/// fatal to a tokeniser looking for that exact two-character sequence,
/// while `<!--`/`-->`/`<script` are entity-escaped, the same substitution
/// `ContentType::Text` would have made for every character rather than
/// just these.
fn make_style_content_html_safe(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < css.len() {
        let rest = &css[i..];

        if rest.as_bytes().first() == Some(&b'<') && rest.as_bytes().get(1) == Some(&b'/') {
            let after_slash = &rest[2..];
            let trimmed = after_slash.trim_start_matches(|c: char| c.is_whitespace());
            if trimmed.len() >= 5 && trimmed.as_bytes()[..5].eq_ignore_ascii_case(b"style") {
                out.push('<');
                out.push('\u{200b}');
                out.push('/');
                i += 2;
                continue;
            }
        }

        if ci_starts_with(rest, "<!--") {
            out.push_str("&lt;!--");
            i += "<!--".len();
            continue;
        }
        if ci_starts_with(rest, "-->") {
            out.push_str("--&gt;");
            i += "-->".len();
            continue;
        }
        if ci_starts_with(rest, "<script") {
            out.push_str("&lt;script");
            i += "<script".len();
            continue;
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

    // ---- finding 2: a `<style>` block survives ContentType::Html safely ---

    #[test]
    fn a_child_selector_survives_sanitize_byte_for_byte() {
        let out = sanitize(r#"<style>td > p { color: red; }</style>"#, &rewrite());
        assert!(out.html.contains("td > p"), "{}", out.html);
        assert!(!out.html.contains("&gt;"), "{}", out.html);
    }

    #[test]
    fn a_child_selector_survives_the_final_guard_pass_too() {
        // `guard_style_urls` is the second of the two passes that write a
        // `<style>` block's content back -- see its own module docs -- so
        // this checks it does not re-introduce the escaping bug on its own.
        let out = guard_style_urls(r#"<style>td > p { color: red; }</style>"#, "everyday").unwrap();
        assert!(out.contains("td > p"), "{out}");
        assert!(!out.contains("&gt;"), "{out}");
    }

    #[test]
    fn a_style_close_tag_inside_style_content_cannot_break_out() {
        // Exercises `make_style_content_html_safe` directly: this is the
        // string a `<style>` block's content would have to become, by
        // whatever means, for `</style` inside it to matter -- lol_html's
        // own raw-text tokenising already stops that string from ever
        // reaching this function *through* an ordinary crafted email (see
        // the function's own docs), but the neutralisation is defence in
        // depth against exactly this shape regardless of how it might
        // arrive.
        let made_safe = make_style_content_html_safe("</style><script>alert(1)</script>");
        let lower = made_safe.to_ascii_lowercase();
        assert!(!lower.contains("</style"), "{made_safe}");
        assert!(!lower.contains("<script"), "{made_safe}");
    }

    #[test]
    fn whitespace_and_case_do_not_help_a_style_close_tag_survive() {
        for variant in ["</  style>", "</\tSTYLE>", "</ StYlE>"] {
            let made_safe = make_style_content_html_safe(variant);
            assert!(
                !made_safe.to_ascii_lowercase().contains("</style"),
                "{variant} -> {made_safe}"
            );
        }
    }

    #[test]
    fn html_comment_markers_inside_style_text_are_neutralised() {
        let made_safe = make_style_content_html_safe("<!-- p { color: red; } -->");
        assert!(!made_safe.contains("<!--"), "{made_safe}");
        assert!(!made_safe.contains("-->"), "{made_safe}");
    }

    #[test]
    fn a_style_block_containing_a_close_tag_and_script_cannot_break_out_end_to_end() {
        // The end-to-end version of the two unit tests above: even if
        // something upstream of `make_style_content_html_safe` ever handed
        // it a chunk containing this text, `sanitize`'s output must not
        // contain a live `<script` tag or a `</style` sequence that could
        // end the element early.
        let out =
            sanitize(r#"<style>p { color: red; }</style><script>alert(1)</script>"#, &rewrite());
        assert!(!out.html.to_ascii_lowercase().contains("<script"), "{}", out.html);
        assert!(!out.html.contains("alert(1)"), "{}", out.html);
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

    // ---- finding 1: an image URL's entities are decoded before proxying ---

    #[test]
    fn a_named_entity_in_a_src_query_string_is_decoded_before_hashing() {
        let out = sanitize(r#"<img src="https://cdn.example/p.png?w=600&amp;h=300">"#, &rewrite());
        assert_eq!(out.remote_images.len(), 1);
        assert_eq!(out.remote_images[0].original_url, "https://cdn.example/p.png?w=600&h=300");
    }

    #[test]
    fn a_numeric_entity_in_a_src_query_string_is_decoded_before_hashing() {
        let out = sanitize(r#"<img src="https://cdn.example/p.png?w=600&#38;h=300">"#, &rewrite());
        assert_eq!(out.remote_images.len(), 1);
        assert_eq!(out.remote_images[0].original_url, "https://cdn.example/p.png?w=600&h=300");
    }

    #[test]
    fn the_same_decoded_url_from_either_spelling_shares_one_token() {
        let named =
            sanitize(r#"<img src="https://cdn.example/p.png?w=600&amp;h=300">"#, &rewrite());
        let numeric =
            sanitize(r#"<img src="https://cdn.example/p.png?w=600&#38;h=300">"#, &rewrite());
        assert_eq!(named.remote_images[0].token, numeric.remote_images[0].token);
    }

    #[test]
    fn a_srcset_candidates_entities_are_decoded_before_hashing() {
        let out = sanitize(
            r#"<img src="https://cdn.example/p.png" srcset="https://cdn.example/p.png?w=600&amp;h=300 1x">"#,
            &rewrite(),
        );
        assert!(
            out.remote_images
                .iter()
                .any(|i| i.original_url == "https://cdn.example/p.png?w=600&h=300"),
            "{:?}",
            out.remote_images
        );
    }

    #[test]
    fn a_background_attributes_entities_are_decoded_before_hashing() {
        let out = sanitize(
            r#"<table background="https://cdn.example/p.png?w=600&amp;h=300"></table>"#,
            &rewrite(),
        );
        assert_eq!(out.remote_images.len(), 1);
        assert_eq!(out.remote_images[0].original_url, "https://cdn.example/p.png?w=600&h=300");
    }

    // ---- entity-encoded CSS cannot bypass the scrubber ---------------------

    #[test]
    fn decimal_entity_encoded_parens_do_not_survive_scrub() {
        let out = sanitize(
            r#"<div style="background:url&#40;https://evil.example/t.gif&#41;">hi</div>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"), "{}", out.html);
    }

    #[test]
    fn a_hex_entity_encoded_keyword_letter_does_not_survive_scrub() {
        let out = sanitize(
            r#"<div style="background: &#x75;rl(https://evil.example/a.png)">hi</div>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"), "{}", out.html);
    }

    #[test]
    fn mixed_case_hex_entities_do_not_survive_scrub() {
        let out = sanitize(
            r#"<div style="background: &#X75;RL(https://evil.example/a.png)">hi</div>"#,
            &rewrite(),
        );
        assert!(!out.html.to_lowercase().contains("evil.example"), "{}", out.html);
    }

    #[test]
    fn named_lpar_and_rpar_entities_do_not_survive_scrub() {
        let out = sanitize(
            r#"<div style="background:url&lpar;https://evil.example/a.png&rpar;">hi</div>"#,
            &rewrite(),
        );
        assert!(!out.html.contains("evil.example"), "{}", out.html);
    }

    // ---- the final, post-ammonia wall over style urls -----------------------

    #[test]
    fn the_final_guard_drops_a_url_that_reached_it_unproxied() {
        let out = guard_style_urls(
            r#"<p style="background:url(https://evil.example/a.png)">hi</p>"#,
            "everyday",
        )
        .unwrap();
        assert!(!out.contains("evil.example"), "{out}");
    }

    #[test]
    fn the_final_guard_drops_an_unsafe_url_from_a_style_block_too() {
        let out = guard_style_urls(
            r#"<style>p { background: url(https://evil.example/a.png); }</style>"#,
            "everyday",
        )
        .unwrap();
        assert!(!out.contains("evil.example"), "{out}");
    }

    #[test]
    fn the_final_guard_keeps_a_proxied_or_inline_image_url() {
        let out = guard_style_urls(
            r#"<p style="background:url(everyday://mail/img/msg/tok)">hi</p>
               <style>p { background: url(data:image/png;base64,AAAA); }</style>"#,
            "everyday",
        )
        .unwrap();
        assert!(out.contains("everyday://mail/img/msg/tok"), "{out}");
        assert!(out.contains("data:image/png"), "{out}");
    }
}
