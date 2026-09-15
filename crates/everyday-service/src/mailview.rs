//! Turning a stored message into what a sandboxed frame shows, and answering
//! the two other things a rendered message can ask for afterwards: one of
//! its own parts, and a remote image nobody has fetched yet.
//!
//! Shared by both transports -- `everyday-app/src/protocol.rs`'s
//! `everyday://` scheme and `everyday-server`'s equivalent HTTP routes both
//! call the functions in this file directly, the same way they already
//! share [`crate::service::Service::blob_len`] and
//! [`crate::service::Service::blob_range`] for an ordinary attachment.
//! Nothing here is a [`crate::command::Command`] in the ordinary table: a
//! rendered body is bytes served over a byte-serving route, not a JSON
//! result, and duplicating that logic per transport is exactly the mistake
//! `docs/plans/mail.md`'s "Rendering a message" warns against. The three
//! functions a transport actually calls are [`body_document`], [`part`] and
//! [`remote_image`]; the rest of this file is what they are built from, plus
//! the small settings surface `domains::mailview` wraps as commands.
//!
//! # The three walls around a message's HTML
//!
//! `everyday-mail::sanitize` already did the hard work at sync time --
//! `<script>`, event handlers, `javascript:` links and everything else with
//! behaviour is gone before a byte of this file ever runs. What is added
//! here is the second and third wall the plan's "Risks" table promises: a
//! frame with no script capability at all (the caller wraps the string this
//! module returns in `<iframe sandbox srcdoc>`, never `allow-scripts`), and
//! a `Content-Security-Policy` meta tag inside the document itself, so that
//! even a sanitiser bug that let something slip through still has nowhere
//! to phone home to. Nothing in this module ever writes `<script`, and a
//! test in this file's own `tests` module asserts that a document never
//! contains it whatever a message's HTML looked like on the way in.
//!
//! # Why remote images are a separate, later fetch
//!
//! [`sanitize::sanitize`](everyday_mail::sanitize::sanitize) already turned
//! every `<img src="https://…">` into `everyday://mail/img/{token}` at sync
//! time, which is what stops a message phoning home the moment it is
//! opened. [`remote_image`] is what a request to that address actually
//! does: check the sender against the standing allow-list or a one-off
//! grant for this message, and only then fetch -- through the one shared
//! client (`crate::http`), with a size cap, a timeout, and the SSRF check
//! below, because the far end of that fetch is a URL a stranger wrote, not
//! one the person chose.

use std::net::IpAddr;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::id::MailMessageId;
use everyday_core::mail::{Body, PartRef, RemoteImageSettings};
use everyday_core::media;

use crate::error::{CommandError, CommandResult, codes};
use crate::http;
use crate::service::blocking;

// ---- the document -----------------------------------------------------------

/// A rendered body, ready to be handed to the transport as the content of an
/// `everyday://mail/body/{id}` (or the server's equivalent) response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyDocument {
    pub html: String,
    /// True when this message has at least one proxied remote image and
    /// `allow_remote` was `false` for this render -- what the interface
    /// reads to show "Images hidden — show / always show from this
    /// sender", *without* it having to parse the document to find out.
    pub images_hidden: bool,
}

/// The `Content-Security-Policy` every rendered body carries, inside a
/// `<meta http-equiv>` tag rather than only in the transport's own header,
/// because `srcdoc` content has no response of its own for a header to
/// attach to on some platforms and this document must be safe wherever it
/// ends up.
///
/// * `default-src 'none'` — the floor. Nothing loads that is not named
///   below.
/// * `img-src everyday: http://everyday.localhost data:` — exactly the two
///   ways a webview can address this application's own custom scheme
///   (`tauri.conf.json`'s own `img-src` already lists both: `everyday:` is
///   how Linux and macOS's WebKit backends see it, `http://everyday.localhost`
///   is how Windows' WebView2 sees the same scheme, because WebView2 will
///   not register a bare custom scheme and Tauri answers on a synthetic
///   `https`-shaped origin instead) plus `data:` for the rare inline raster
///   image `sanitize::sanitize` left alone (see that module's own docs on
///   why `data:image/*` survives). Never `http:`/`https:` directly: every
///   remote image was already rewritten to the app's own scheme at sync, so
///   there is nothing legitimate for a bare web address to be doing here.
/// * `style-src 'unsafe-inline'` — sanitised mail keeps its own `<style>`
///   block and `style=""` attributes (`sanitize.rs`'s own reasoning: dropping
///   them would leave a newsletter's layout in pieces), and there is no
///   external stylesheet to prefer instead. `'unsafe-inline'` for style is a
///   far smaller grant than for script -- it can misdraw a page, not run
///   code -- and CSS injection through it is exactly the CSS-hazard surface
///   `sanitize::scrub_css` already scrubbed for `url()`, `@import` and
///   `expression()` before this document ever sees the markup.
/// * `font-src data:` — a `data:` embedded font is inert (no network fetch);
///   a remote one was already reduced to nothing by `sanitize::scrub_css`,
///   which classifies (and mostly refuses) every `url()` a `<style>` block
///   or a `style=""` attribute names, fonts included.
///
/// No `script-src` at all: `default-src 'none'` already forbids it, and
/// there is deliberately no directive that could be read as inviting one
/// back. See `tauri.conf.json`'s own `frame-src` for why the *parent*
/// document may embed this one as `srcdoc` at all -- that is the other half
/// of the platform research this constant's neighbours in `protocol.rs`
/// document.
const CSP: &str = "default-src 'none'; img-src everyday: http://everyday.localhost data:; \
     style-src 'unsafe-inline'; font-src data:";

/// The style every rendered body starts from. Deliberately plain CSS baked
/// into the document rather than a `<link>` to a stylesheet -- there is
/// nowhere `img-src`-shaped for a stylesheet to load from inside this CSP,
/// and a rendered message is exactly the one place in this application that
/// should not depend on the theme file `ui/src/styles/theme.css` ships,
/// because that file can change shape and this document has to keep working
/// on its own.
///
/// The colours are the same ones `theme.css` chose (light and dark), copied
/// rather than shared, for the same reason: this document has no build step
/// and no access to a CSS custom property defined outside itself. Setting
/// them on `body` rather than on every element is what "respect the email's
/// own colours, but give plain-text and unstyled mail the app's colours"
/// (the plan's own words) comes down to in practice -- CSS inheritance does
/// the rest. A sanitised message with its own `style=""` or `<style>` block
/// wins by ordinary specificity; the `<pre>` this module builds for a
/// plain-text message carries no styling of its own at all, so it falls
/// straight through to these.
const BASE_STYLE: &str = r#"
:root { color-scheme: light dark; }
body {
  margin: 0;
  padding: 16px;
  font-family: 'Source Sans 3 Variable', 'Source Sans Pro', 'Seravek', 'Ubuntu',
    'Segoe UI Variable Text', 'Segoe UI', -apple-system, BlinkMacSystemFont, system-ui,
    'Noto Sans', sans-serif;
  font-size: 15px;
  line-height: 1.6;
  color: #1c1a17;
  background: #f7f6f3;
  overflow-wrap: break-word;
}
a { color: inherit; }
pre {
  white-space: pre-wrap;
  overflow-wrap: break-word;
  font-family: inherit;
  margin: 0 0 1em;
}
details { margin: 0.5em 0; }
summary { cursor: default; color: #8b857c; }
img { max-width: 100%; height: auto; }
table { max-width: 100%; }
@media (prefers-color-scheme: dark) {
  body { color: #eceaf0; background: #17161a; }
  summary { color: #7d7a86; }
}
"#;

/// Wraps the stored, sanitised body of `message_id` in a complete,
/// self-contained document -- see the module docs for the three walls this
/// builds on top of what `everyday-mail::sanitize` already did.
///
/// `allow_remote` is decided by the caller (`remote_images_allowed`, below,
/// applied against whichever sender and one-off state it has in hand) and
/// changes nothing about the markup: an `<img src="everyday://mail/img/…">`
/// is written into the document exactly the same either way, because the
/// decision about whether that address actually answers with a picture is
/// [`remote_image`]'s, made fresh on its own request. All `allow_remote`
/// does here is decide [`BodyDocument::images_hidden`], which is what tells
/// the interface whether to offer "show images" at all.
pub fn body_document(
    vault: &Vault,
    message_id: MailMessageId,
    allow_remote: bool,
) -> CommandResult<BodyDocument> {
    let body = vault.body(message_id)?;
    let images_hidden = !allow_remote && !body.remote_images.is_empty();

    let inner = if body.html_sanitised.trim().is_empty() {
        plain_text_body(&body)
    } else {
        // Already sanitised at sync; nothing here parses or rewrites it
        // again. `sanitize::sanitize`'s ammonia pass is what makes putting
        // this straight into the document safe.
        body.html_sanitised.clone()
    };

    let html = format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
         <base target=\"_blank\">\n\
         <style>{style}</style>\n\
         </head>\n\
         <body>\n{inner}\n</body>\n\
         </html>\n",
        csp = escape_attribute(CSP),
        style = BASE_STYLE,
        inner = inner,
    );

    Ok(BodyDocument { html, images_hidden })
}

/// A plain-text message: escaped, linkified, and with `Body::quoted_ranges`
/// collapsed behind a script-free `<details>` -- see the plan's own words
/// under "Rendering a message" for both requirements.
///
/// This is the *only* rendering path quote-collapsing applies to.
/// `quoted_ranges` are byte offsets into [`Body::text`]
/// (`crate::mail::records`'s own docs on the field), not into
/// `html_sanitised` -- there is no sound way to splice a `<details>` into
/// arbitrary sanitised HTML at a text-byte offset without risking landing
/// inside a tag, so an HTML message's own quoting (a `blockquote`, an
/// indent, an "On ... wrote:" line ammonia already kept) is shown as the
/// sender wrote it instead.
fn plain_text_body(body: &Body) -> String {
    let mut out = String::new();
    for segment in split_by_ranges(&body.text, &body.quoted_ranges) {
        let rendered = format!("<pre>{}</pre>", linkify(segment.text));
        if segment.quoted {
            out.push_str("<details><summary>Quoted text</summary>");
            out.push_str(&rendered);
            out.push_str("</details>");
        } else {
            out.push_str(&rendered);
        }
    }
    out
}

struct Segment<'a> {
    text: &'a str,
    quoted: bool,
}

/// Splits `text` into segments tagged by whether they fall inside one of
/// `ranges`, covering the whole string, in order. Ranges are clamped to
/// `text`'s length and merged where they touch or overlap -- the same
/// defensive clamping [`Body::model_text`](everyday_core::mail::Body::model_text)
/// applies, and for the same reason: a range computed against a slightly
/// different encoding at sync time must not panic a render years later.
fn split_by_ranges<'a>(text: &'a str, ranges: &[(u32, u32)]) -> Vec<Segment<'a>> {
    let len = text.len();
    let mut cuts: Vec<(usize, usize)> = ranges
        .iter()
        .map(|&(a, b)| (a as usize, b as usize))
        .map(|(a, b)| (a.min(len), b.min(len)))
        .filter(|(a, b)| a < b)
        .collect();
    cuts.sort_unstable();

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in cuts.drain(..) {
        if let Some(last) = merged.last_mut() {
            if a <= last.1 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        merged.push((a, b));
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    for (start, end) in merged {
        let start = char_boundary_at_or_before(text, start.max(pos));
        if start > pos {
            out.push(Segment { text: &text[pos..start], quoted: false });
        }
        let end = char_boundary_at_or_before(text, end.max(start));
        if end > start {
            out.push(Segment { text: &text[start..end], quoted: true });
        }
        pos = end;
    }
    if pos < len {
        let pos = char_boundary_at_or_before(text, pos);
        out.push(Segment { text: &text[pos..len], quoted: false });
    }
    out
}

fn char_boundary_at_or_before(s: &str, at: usize) -> usize {
    let at = at.min(s.len());
    (0..=at).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

/// Escapes `text` and wraps every `http://`/`https://` run in an `<a>`, so a
/// plain-text message's links are clickable without this module ever
/// parsing HTML back out of text it just built.
///
/// Deliberately hand-rolled rather than a regex crate: the grammar is "a
/// scheme, then non-whitespace", which is the same shape
/// `everyday-mail::sanitize`'s own hand-rolled CSS scanner argues for over a
/// dependency -- a few lines of a state machine are less surface than a new
/// crate for one job this small.
fn linkify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_boundary = true;
    let mut rest = text;
    while !rest.is_empty() {
        if at_boundary && (rest.starts_with("http://") || rest.starts_with("https://")) {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (url, tail) = rest.split_at(end);
            let escaped = escape_html(url);
            out.push_str("<a href=\"");
            out.push_str(&escaped);
            out.push_str("\">");
            out.push_str(&escaped);
            out.push_str("</a>");
            rest = tail;
            at_boundary = false;
            continue;
        }
        let ch = rest.chars().next().expect("rest is non-empty");
        at_boundary = ch.is_whitespace();
        escape_char(ch, &mut out);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        escape_char(ch, &mut out);
    }
    out
}

fn escape_char(ch: char, out: &mut String) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(ch),
    }
}

/// Escapes a string for use inside a *double-quoted* HTML attribute. `CSP`
/// is a fixed constant with no `"` in it today, but this is what stands
/// between that staying true and a meta tag silently truncating its own
/// policy. Only `&` and `"` are special inside a double-quoted attribute --
/// `'`, unlike in [`escape_html`]'s text-node use, needs no escaping here,
/// and leaving it alone is what keeps a CSP source expression like
/// `'none'` readable in the document rather than turned into `&#39;none&#39;`.
fn escape_attribute(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---- a part -----------------------------------------------------------------

/// One part of a message, from the blob store, with the headers a
/// transport should answer with.
#[derive(Debug, Clone)]
pub struct PartResponse {
    pub content_type: String,
    pub bytes: Vec<u8>,
    /// `true` when the transport must send `Content-Disposition: attachment`
    /// -- see [`part`] and [`remote_image`] for when each sets it.
    pub attachment: bool,
    pub filename: Option<String>,
}

/// Safe to show inline in an `<img>` inside the sandboxed frame. SVG is
/// never in this list -- see the plan's "Risks": an SVG document can carry
/// its own script and its own references, sanitiser or not, and that stays
/// true of an attachment nobody has rewritten at all.
const INLINE_IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// One part of `message_id`'s body -- an attachment, or an inline image a
/// sanitised `cid:` reference points at -- addressed by
/// [`PartRef::cid`](everyday_core::mail::PartRef::cid) when the part has
/// one, or by its position in [`Body::parts`] (as a plain base-ten string)
/// for an ordinary attachment that does not. That second form is what an
/// attachment chip's download link uses; `cid:` addressing exists only for
/// what `sanitize::sanitize` already rewrote into the message's own HTML.
///
/// The type served is decided from the *bytes*, not the claimed
/// `mime_type`, on [`everyday_core::media::sniff_mime`]'s own reasoning
/// (`crate::websearch::fetch_image`'s docs make the same argument for a
/// fetched cover): what a message's own header says a part is, is a claim
/// by whoever sent it. `text/html` and `image/svg+xml` -- sniffed or
/// claimed -- are never sent inline whatever `mime_type` said, because both
/// can carry their own script; everything else that is not one of
/// [`INLINE_IMAGE_TYPES`] is sent as `application/octet-stream` with
/// `Content-Disposition: attachment`, which is what stops a webview or an
/// OS file handler deciding to open a downloaded attachment as something
/// more capable than a file to save.
pub fn part(
    vault: &Vault,
    message_id: MailMessageId,
    identifier: &str,
) -> CommandResult<PartResponse> {
    let body = vault.body(message_id)?;
    let found = find_part(&body, identifier)
        .ok_or_else(|| CommandError::new(codes::NOT_FOUND, "no such part of this message"))?;
    let blob_id = found.blob.ok_or_else(|| {
        CommandError::new(codes::NOT_FOUND, "that attachment has not finished syncing yet")
    })?;
    let bytes = vault.with_store(|s| s.get_blob(blob_id))?;

    let sniffed = media::sniff_mime(&bytes);
    let declared = found.mime_type.trim().to_ascii_lowercase();

    if sniffed == "text/html" || sniffed == "image/svg+xml" || declared == "text/html" {
        return Ok(PartResponse {
            content_type: "application/octet-stream".to_string(),
            bytes,
            attachment: true,
            filename: found.filename.clone(),
        });
    }

    if INLINE_IMAGE_TYPES.contains(&sniffed) {
        return Ok(PartResponse {
            content_type: sniffed.to_string(),
            bytes,
            attachment: false,
            filename: found.filename.clone(),
        });
    }

    // Whatever it actually is, keep the declared type for a sensible
    // download (a `.pdf` saved as a `.pdf`) rather than flattening every
    // attachment to `octet-stream` -- only HTML and SVG, above, are forced
    // to that, because only they can act on their own once opened.
    let content_type = if sniffed != "application/octet-stream" {
        sniffed.to_string()
    } else if declared.is_empty() {
        "application/octet-stream".to_string()
    } else {
        declared
    };
    Ok(PartResponse { content_type, bytes, attachment: true, filename: found.filename.clone() })
}

fn find_part<'a>(body: &'a Body, identifier: &str) -> Option<&'a PartRef> {
    body.parts
        .iter()
        .find(|p| p.cid.as_deref() == Some(identifier))
        .or_else(|| identifier.parse::<usize>().ok().and_then(|i| body.parts.get(i)))
}

// ---- remote images ------------------------------------------------------

/// Is `message_id` allowed to load its remote images right now -- the
/// standing allow-list, or a one-off grant the caller already knows about
/// for this session (`Service::remote_images_allowed_once`, kept in memory
/// rather than in this function, because it is session state and this
/// module has no session of its own).
///
/// Called by a transport before [`body_document`] (to decide
/// `allow_remote`) and, independently, inside [`remote_image`] itself for
/// every actual fetch -- the two calls are deliberately not required to
/// agree: a person can click "show images" after a document has already
/// been rendered, and the next `mail/img` request must see that change
/// without the document being rebuilt first.
pub fn remote_images_allowed(
    vault: &Vault,
    message_id: MailMessageId,
    one_off_allowed: bool,
) -> CommandResult<bool> {
    if one_off_allowed {
        return Ok(true);
    }
    let message = vault.mail_message(message_id)?;
    let settings = vault.remote_image_settings()?;
    Ok(settings.allows(&message.from.email))
}

/// Largest remote image fetched. Generous for a photograph in a newsletter,
/// and finite for the reason every cap in this application is: a server
/// that never stops sending must not be believed.
const MAX_REMOTE_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// What this module sends instead of a name that would tell an image's host
/// which application, and which build of it, just asked -- see the module
/// docs on why the fetch happens through the app's own proxy at all. Not
/// blank: several image hosts refuse a request with no `User-Agent` at all
/// rather than answering it.
const REMOTE_IMAGE_USER_AGENT: &str = "Mozilla/5.0";

/// A transparent 1×1 PNG, the well-known 68-byte encoding of "nothing to
/// see here" every image-blocking tool answers with. Decoded once, from a
/// fixed constant checked by this module's own tests, rather than built
/// byte by byte inline.
const PLACEHOLDER_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACklEQVR4nGNgAAIAAAUAAen63NgAAAAASUVORK5CYII=";

fn placeholder_pixel() -> PartResponse {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(PLACEHOLDER_PNG_BASE64)
        .expect("PLACEHOLDER_PNG_BASE64 is a fixed, valid constant -- see this module's tests");
    PartResponse { content_type: "image/png".to_string(), bytes, attachment: false, filename: None }
}

/// Answers an `everyday://mail/img/{token}` request (or the server's
/// equivalent) for `message_id`.
///
/// Three things happen, in order, and the order is the security of it.
/// First, permission: [`remote_images_allowed`], checked fresh on every call
/// rather than trusted from whatever a document was built with -- a person
/// can grant it after the frame already loaded, and the next request for
/// the same token must see that. Refused permission is not an error: it is
/// the same transparent pixel a blocked tracking pixel would have been,
/// which is what keeps a refusal indistinguishable, from the sender's side,
/// from an image that loaded normally -- nothing about *why* nothing was
/// fetched should be observable from outside this process, and there is
/// nothing to observe regardless, because nothing left it.
///
/// Second, the cache: a `RemoteImage` already carrying `cached_blob` is
/// served from the blob store and the network is never touched, which is
/// what makes reopening a message free.
///
/// Third, the fetch itself, through [`crate::http::client`] -- the one
/// shared client -- with [`resolve_and_check`]'s SSRF wall in front of it,
/// [`MAX_REMOTE_IMAGE_BYTES`] behind [`crate::http::read_capped`], and the
/// result content-sniffed and refused unless it is a raster image. A
/// successful fetch is cached into the blob store and `Body::remote_images`
/// is updated to name it, in the same write, so the second person to open
/// the message -- or the same person reopening it -- pays for the fetch
/// exactly once.
pub async fn remote_image(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    message_id: MailMessageId,
    token: &str,
    one_off_allowed: bool,
) -> CommandResult<PartResponse> {
    remote_image_inner(vault, client, message_id, token, one_off_allowed, false).await
}

/// The one door this module opens for a test to walk a real fetch through a
/// local server on `127.0.0.1` without weakening [`remote_image`] itself.
/// Only ever compiled into a test binary -- see [`resolve_and_check`] for
/// where `allow_loopback` actually changes anything.
#[cfg(test)]
async fn remote_image_allowing_loopback_for_test(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    message_id: MailMessageId,
    token: &str,
    one_off_allowed: bool,
) -> CommandResult<PartResponse> {
    remote_image_inner(vault, client, message_id, token, one_off_allowed, true).await
}

enum Ready {
    Placeholder,
    Cached(Vec<u8>),
    Fetch { url: String },
}

async fn remote_image_inner(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    message_id: MailMessageId,
    token: &str,
    one_off_allowed: bool,
    allow_loopback: bool,
) -> CommandResult<PartResponse> {
    let plan = {
        let vault = vault.clone();
        let token = token.to_string();
        blocking(move || {
            if !remote_images_allowed(&vault, message_id, one_off_allowed)? {
                return Ok(Ready::Placeholder);
            }
            let body = vault.body(message_id)?;
            let Some(found) = body.remote_images.iter().find(|r| r.token == token) else {
                return Err(CommandError::new(codes::NOT_FOUND, "no such remote image"));
            };
            if let Some(blob) = found.cached_blob {
                let bytes = vault.with_store(|s| s.get_blob(blob))?;
                return Ok(Ready::Cached(bytes));
            }
            Ok(Ready::Fetch { url: found.original_url.clone() })
        })
        .await?
    };

    let bytes = match plan {
        Ready::Placeholder => return Ok(placeholder_pixel()),
        Ready::Cached(bytes) => bytes,
        Ready::Fetch { url } => {
            let fetched = fetch_remote_image(client, &url, allow_loopback).await?;
            let vault = vault.clone();
            let cache = fetched.clone();
            let token = token.to_string();
            blocking(move || {
                let blob = vault.put_blob(&cache)?;
                let mut body = vault.body(message_id)?;
                if let Some(entry) = body.remote_images.iter_mut().find(|r| r.token == token) {
                    entry.cached_blob = Some(blob);
                }
                vault.save_body(&body)?;
                Ok(())
            })
            .await?;
            fetched
        }
    };

    let content_type = media::sniff_mime(&bytes).to_string();
    Ok(PartResponse { content_type, bytes, attachment: false, filename: None })
}

async fn fetch_remote_image(
    client: &reqwest::Client,
    url: &str,
    allow_loopback: bool,
) -> CommandResult<Vec<u8>> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| CommandError::new(codes::INVALID, "that is not a web address"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(CommandError::new(codes::FORBIDDEN, "only http and https images are fetched"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| CommandError::new(codes::INVALID, "that address has no host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| CommandError::new(codes::INVALID, "that address has no port"))?;

    resolve_and_check(host, port, allow_loopback).await?;

    let response = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "image/*")
        .header(reqwest::header::USER_AGENT, REMOTE_IMAGE_USER_AGENT)
        .send()
        .await
        .map_err(|e| {
            CommandError::new(
                codes::NETWORK,
                format!("the image could not be fetched: {}", http::strip_url(&e.to_string())),
            )
        })?;

    if !response.status().is_success() {
        return Err(CommandError::new(
            codes::NETWORK,
            format!("the image server answered {}", response.status()),
        ));
    }

    let bytes = http::read_capped(response, MAX_REMOTE_IMAGE_BYTES, || {
        format!(
            "that image is larger than {} MB, which is more than this application will fetch",
            MAX_REMOTE_IMAGE_BYTES / 1_048_576
        )
    })
    .await?;

    let mime = media::sniff_mime(&bytes);
    if !mime.starts_with("image/") || mime == "image/svg+xml" {
        return Err(CommandError::new(
            codes::NOT_AN_IMAGE,
            "that address did not return a picture, so nothing has been shown",
        ));
    }
    Ok(bytes)
}

/// Resolves `host` and refuses to go on if any answer is a private,
/// link-local or loopback address -- the wall against SSRF the plan's
/// "Build" section asks for by name: a message can name any URL it likes,
/// including one that resolves to this machine's own loopback interface or
/// to an address on whatever network this process happens to be running on,
/// and answering that request the normal way would let a message probe a
/// person's home router or a work laptop's internal services from inside an
/// application that is supposed to make only the request the person chose.
///
/// `allow_loopback` is always `false` in this module's own production
/// callers -- see [`remote_image`] -- and exists only so that
/// [`remote_image_allowing_loopback_for_test`] can point a real fetch at a
/// test server bound to `127.0.0.1` without weakening the check every other
/// caller relies on.
async fn resolve_and_check(host: &str, port: u16, allow_loopback: bool) -> CommandResult<()> {
    let addrs = tokio::net::lookup_host((host, port)).await.map_err(|_| {
        CommandError::new(codes::NETWORK, "that image's address could not be resolved")
    })?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_forbidden_address(addr.ip(), allow_loopback) {
            return Err(CommandError::new(
                codes::FORBIDDEN,
                "that address points at a private network and will not be fetched",
            ));
        }
    }
    if !saw_any {
        return Err(CommandError::new(
            codes::NETWORK,
            "that image's address did not resolve to anything",
        ));
    }
    Ok(())
}

/// Loopback, link-local, unspecified, broadcast, RFC 1918 private, and
/// carrier-grade NAT (RFC 6598) addresses -- everything a residential or
/// office network hands out to something that is not meant to be reached
/// from the open internet, plus the addresses that mean "this machine"
/// rather than any of those. Written against raw octets and segments rather
/// than the standard library's own `is_private`/`is_unique_local` family:
/// several of those stabilised well after this application's minimum
/// supported Rust version (`docs/`'s own note that local `rustc` lags CI),
/// and the ranges themselves are fixed by the RFCs regardless of which
/// release of the standard library happens to name them.
fn is_forbidden_address(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let loopback = o[0] == 127;
            if allow_loopback && loopback {
                return false;
            }
            loopback
                || o == [0, 0, 0, 0]
                || o == [255, 255, 255, 255]
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            let loopback = segments == [0, 0, 0, 0, 0, 0, 0, 1];
            if allow_loopback && loopback {
                return false;
            }
            if loopback || segments == [0; 8] {
                return true;
            }
            if let Some(mapped) = ipv4_mapped(&segments) {
                return is_forbidden_address(IpAddr::V4(mapped), allow_loopback);
            }
            // `fe80::/10`, link-local, and `fc00::/7`, unique local -- the
            // IPv6 counterparts of `169.254.0.0/16` and the RFC 1918 ranges
            // above.
            let seg0 = segments[0];
            (seg0 & 0xffc0 == 0xfe80) || (seg0 & 0xfe00 == 0xfc00)
        }
    }
}

/// `::ffff:a.b.c.d` unpacked to the `Ipv4Addr` it maps, without
/// `Ipv6Addr::to_ipv4_mapped` -- see [`is_forbidden_address`]'s docs on
/// avoiding methods this crate's minimum Rust version might not have yet.
fn ipv4_mapped(segments: &[u16; 8]) -> Option<std::net::Ipv4Addr> {
    if segments[0..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        let a = (segments[6] >> 8) as u8;
        let b = (segments[6] & 0xff) as u8;
        let c = (segments[7] >> 8) as u8;
        let d = (segments[7] & 0xff) as u8;
        Some(std::net::Ipv4Addr::new(a, b, c, d))
    } else {
        None
    }
}

// ---- the standing allow-list ---------------------------------------------

/// Adds `sender` and/or `domain` to the standing, sealed allow-list --
/// `domains::mailview::allow_remote_images`' wrapper for the persisted half
/// of that command. The one-off, per-message grant the same command can
/// also express is deliberately not here: it lives in memory, on
/// [`crate::service::Service`], because it must not outlive the session
/// that granted it.
pub fn allow_remote_images(
    vault: &Vault,
    sender: Option<&str>,
    domain: Option<&str>,
) -> CommandResult<()> {
    let mut settings = vault.remote_image_settings()?;
    if let Some(sender) = sender {
        settings.allow_sender(sender);
    }
    if let Some(domain) = domain {
        settings.allow_domain(domain);
    }
    vault.save_remote_image_settings(&settings)?;
    Ok(())
}

pub fn list_remote_image_allowances(vault: &Vault) -> CommandResult<RemoteImageSettings> {
    Ok(vault.remote_image_settings()?)
}

pub fn revoke_remote_image_allowance(
    vault: &Vault,
    sender: Option<&str>,
    domain: Option<&str>,
) -> CommandResult<()> {
    let mut settings = vault.remote_image_settings()?;
    settings.revoke(sender, domain);
    vault.save_remote_image_settings(&settings)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::id::AccountId;
    use everyday_core::mail::{
        Address, Mailbox, MailboxRole, Message, MessageFlags, PartRef, RemoteImage,
    };
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;

    fn test_vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = everyday_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = everyday_vault::create(dir.path(), cfg).unwrap();
        (dir, vault)
    }

    use jiff::Timestamp;

    /// A message with no meaningful pack address -- every test here reads
    /// a message's headers and its `Body`, never its raw bytes in the pack
    /// store, so an empty [`PackRef`] is a fine stand-in.
    fn fresh_message(account: AccountId, from: &str) -> Message {
        let id = MailMessageId::new();
        Message {
            id,
            account_id: account,
            thread_id: everyday_core::id::ThreadId::new(),
            message_id_header: format!("<{id}@mailview.example>"),
            date: Timestamp::now(),
            from: Address::bare(from),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "hi".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            pack: PackRef {
                account: account.to_string(),
                pack: everyday_core::id::PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
        }
    }

    fn seeded_message(vault: &Vault, from: &str, html: &str, text: &str) -> MailMessageId {
        let account = AccountId::new();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        let message = fresh_message(account, from);
        vault
            .ingest_mail(
                account,
                vec![IngestMessage { message: message.clone(), mailbox: mailbox.id, uid: 1 }],
            )
            .unwrap();
        let body = Body {
            message_id: message.id,
            html_sanitised: html.to_string(),
            text: text.to_string(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        vault.save_body(&body).unwrap();
        message.id
    }

    #[test]
    fn a_body_document_never_contains_script_and_always_carries_the_csp() {
        // Run through the real sync-time sanitiser first -- exactly what a
        // message's `html_sanitised` is in production -- so this test
        // exercises the whole pipeline rather than assuming `body_document`
        // re-sanitises anything itself. It does not: see this module's own
        // docs on why sanitising once, at sync, is the point.
        let sanitised = everyday_mail::sanitize::sanitize(
            r#"<p onclick="x">hi</p><script>alert(1)</script>"#,
            &everyday_mail::sanitize::Rewrite::new("msg-1@example.com"),
        );
        assert!(!sanitised.html.to_ascii_lowercase().contains("<script"));

        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", &sanitised.html, "");
        let doc = body_document(&vault, id, false).unwrap();
        assert!(!doc.html.to_ascii_lowercase().contains("<script"), "{}", doc.html);
        assert!(doc.html.contains("Content-Security-Policy"));
        assert!(doc.html.contains("default-src 'none'"));
    }

    #[test]
    fn plain_text_is_escaped() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "", "<b>not bold</b> & friends");
        let doc = body_document(&vault, id, false).unwrap();
        assert!(!doc.html.contains("<b>not bold</b>"));
        assert!(doc.html.contains("&lt;b&gt;not bold&lt;/b&gt;"));
        assert!(doc.html.contains("&amp; friends"));
    }

    #[test]
    fn a_plain_text_link_is_linkified() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "", "see https://example.com/x for it");
        let doc = body_document(&vault, id, false).unwrap();
        assert!(doc.html.contains(r#"<a href="https://example.com/x">https://example.com/x</a>"#));
    }

    #[test]
    fn quoted_text_is_collapsed_behind_details() {
        let (_dir, vault) = test_vault();
        let account = AccountId::new();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        let message = fresh_message(account, "a@example.com");
        vault
            .ingest_mail(
                account,
                vec![IngestMessage { message: message.clone(), mailbox: mailbox.id, uid: 1 }],
            )
            .unwrap();
        let text = "Sure thing.\n\n> the original message";
        let quote_start = text.find("> the original message").unwrap() as u32;
        let body = Body {
            message_id: message.id,
            html_sanitised: String::new(),
            text: text.to_string(),
            quoted_ranges: vec![(quote_start, text.len() as u32)],
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        vault.save_body(&body).unwrap();

        let doc = body_document(&vault, message.id, false).unwrap();
        assert!(doc.html.contains("<details>"));
        assert!(doc.html.contains("the original message"));
        // The unquoted line sits outside any `<details>`.
        let before_details = doc.html.split("<details>").next().unwrap();
        assert!(before_details.contains("Sure thing."));
    }

    #[test]
    fn a_part_refuses_html_inline() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "<p>hi</p>", "hi");
        let blob = vault.put_blob(b"<script>alert(1)</script>").unwrap();
        let mut body = vault.body(id).unwrap();
        body.parts.push(PartRef {
            cid: Some("evil".into()),
            filename: Some("evil.html".into()),
            mime_type: "text/html".into(),
            size: 10,
            blob: Some(blob),
        });
        vault.save_body(&body).unwrap();

        let served = part(&vault, id, "evil").unwrap();
        assert_eq!(served.content_type, "application/octet-stream");
        assert!(served.attachment);
    }

    #[test]
    fn a_part_refuses_svg_inline() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "<p>hi</p>", "hi");
        let blob =
            vault.put_blob(br#"<svg onload="alert(1)"><script>alert(2)</script></svg>"#).unwrap();
        let mut body = vault.body(id).unwrap();
        body.parts.push(PartRef {
            cid: Some("evil.svg".into()),
            filename: Some("evil.svg".into()),
            mime_type: "image/svg+xml".into(),
            size: 10,
            blob: Some(blob),
        });
        vault.save_body(&body).unwrap();

        let served = part(&vault, id, "evil.svg").unwrap();
        assert_eq!(served.content_type, "application/octet-stream");
        assert!(served.attachment);
    }

    #[test]
    fn a_part_serves_a_safe_image_inline() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "<p>hi</p>", "hi");
        // A minimal PNG header is enough for `sniff_mime` to recognise it.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 16]);
        let blob = vault.put_blob(&png).unwrap();
        let mut body = vault.body(id).unwrap();
        body.parts.push(PartRef {
            cid: Some("logo".into()),
            filename: None,
            mime_type: "image/png".into(),
            size: png.len() as u64,
            blob: Some(blob),
        });
        vault.save_body(&body).unwrap();

        let served = part(&vault, id, "logo").unwrap();
        assert_eq!(served.content_type, "image/png");
        assert!(!served.attachment);
    }

    #[test]
    fn a_part_is_addressable_by_index_when_it_has_no_content_id() {
        let (_dir, vault) = test_vault();
        let id = seeded_message(&vault, "a@example.com", "<p>hi</p>", "hi");
        let blob = vault.put_blob(b"some file").unwrap();
        let mut body = vault.body(id).unwrap();
        body.parts.push(PartRef {
            cid: None,
            filename: Some("notes.txt".into()),
            mime_type: "text/plain".into(),
            size: 9,
            blob: Some(blob),
        });
        vault.save_body(&body).unwrap();

        let served = part(&vault, id, "0").unwrap();
        assert!(served.attachment);
        assert_eq!(served.filename.as_deref(), Some("notes.txt"));
    }

    // ---- remote-image permission and the placeholder ----------------------

    fn seeded_with_remote_image(
        vault: &Vault,
        from: &str,
        image_url: &str,
    ) -> (MailMessageId, String) {
        let id = seeded_message(vault, from, "<img src=\"everyday://mail/img/tok\">", "");
        let mut body = vault.body(id).unwrap();
        let token = "tok".to_string();
        body.remote_images.push(RemoteImage {
            original_url: image_url.to_string(),
            token: token.clone(),
            cached_blob: None,
        });
        vault.save_body(&body).unwrap();
        (id, token)
    }

    #[tokio::test]
    async fn a_remote_image_is_refused_without_permission() {
        let (_dir, vault) = test_vault();
        let vault = std::sync::Arc::new(vault);
        let (id, token) =
            seeded_with_remote_image(&vault, "a@example.com", "https://example.com/x.png");
        let client = http::client().unwrap();
        let served = remote_image(&vault, client, id, &token, false).await.unwrap();
        // Refused permission answers with the placeholder, not an error --
        // see `remote_image`'s own docs on why a refusal must not be
        // distinguishable from an ordinary blocked image.
        assert_eq!(served.content_type, "image/png");
        assert_eq!(served.bytes.len(), 68);
    }

    #[tokio::test]
    async fn a_remote_image_is_allowed_once_permitted() {
        let server = spawn_test_image_server().await;
        let (_dir, vault) = test_vault();
        let vault = std::sync::Arc::new(vault);
        let (id, token) = seeded_with_remote_image(
            &vault,
            "a@example.com",
            &format!("http://{}/x.png", server.addr),
        );
        let client = http::client().unwrap();

        let served = remote_image_allowing_loopback_for_test(&vault, client, id, &token, true)
            .await
            .unwrap();
        assert_eq!(served.content_type, "image/png");
        assert_eq!(server.hits(), 1);

        // A second request is served from the cache and never reaches the
        // test server again.
        let _ = remote_image_allowing_loopback_for_test(&vault, client, id, &token, true)
            .await
            .unwrap();
        assert_eq!(server.hits(), 1, "a cached image must not be refetched");
    }

    #[tokio::test]
    async fn an_oversize_response_is_aborted() {
        let server = spawn_test_oversize_server().await;
        let (_dir, vault) = test_vault();
        let vault = std::sync::Arc::new(vault);
        let (id, token) = seeded_with_remote_image(
            &vault,
            "a@example.com",
            &format!("http://{}/big.png", server.addr),
        );
        let client = http::client().unwrap();

        let err = remote_image_allowing_loopback_for_test(&vault, client, id, &token, true)
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::TOO_LARGE);
    }

    // ---- SSRF -----------------------------------------------------------

    #[test]
    fn loopback_and_private_addresses_are_forbidden() {
        let forbidden = [
            "127.0.0.1",
            "127.5.5.5",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
        ];
        for addr in forbidden {
            let ip: IpAddr = addr.parse().unwrap();
            assert!(is_forbidden_address(ip, false), "{addr} should be forbidden");
        }
    }

    #[test]
    fn ordinary_public_addresses_are_allowed() {
        let allowed = ["93.184.216.34", "8.8.8.8", "2001:4860:4860::8888"];
        for addr in allowed {
            let ip: IpAddr = addr.parse().unwrap();
            assert!(!is_forbidden_address(ip, false), "{addr} should be allowed");
        }
    }

    #[test]
    fn the_loopback_override_only_ever_lifts_loopback_itself() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!is_forbidden_address(loopback, true));
        let private: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(
            is_forbidden_address(private, true),
            "the override must not also lift private ranges"
        );
    }

    #[test]
    fn the_placeholder_pixel_decodes_to_a_valid_small_png() {
        let placeholder = placeholder_pixel();
        assert_eq!(placeholder.content_type, "image/png");
        assert!(placeholder.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn remote_image_settings_allows_by_sender_and_by_domain() {
        let mut settings = RemoteImageSettings::default();
        settings.allow_sender("Newsletter@Example.com");
        settings.allow_domain("trusted.example");
        assert!(settings.allows("newsletter@example.com"));
        assert!(settings.allows("anyone@trusted.example"));
        assert!(settings.allows("anyone@mail.trusted.example"));
        assert!(!settings.allows("anyone@evil-trusted.example"));
        assert!(!settings.allows("stranger@example.org"));
    }

    // ---- a tiny local HTTP server, for the fetch tests above -------------

    struct TestServer {
        addr: std::net::SocketAddr,
        hits: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl TestServer {
        fn hits(&self) -> usize {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    async fn spawn_test_image_server() -> TestServer {
        spawn_test_server(|hits| {
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            png.extend_from_slice(&[0u8; 16]);
            response(200, "image/png", &png)
        })
        .await
    }

    async fn spawn_test_oversize_server() -> TestServer {
        spawn_test_server(|hits| {
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut body = b"\x89PNG\r\n\x1a\n".to_vec();
            body.extend(std::iter::repeat_n(0u8, MAX_REMOTE_IMAGE_BYTES + 1));
            response(200, "image/png", &body)
        })
        .await
    }

    fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
        let reason = if status == 200 { "OK" } else { "Error" };
        let mut out = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// A one-shot-per-connection HTTP server on `127.0.0.1`, just capable
    /// enough to answer the fetch tests above: it reads (and discards) one
    /// request line and its headers, then writes whatever `handler` built.
    async fn spawn_test_server(
        handler: impl Fn(&Arc<std::sync::atomic::AtomicUsize>) -> Vec<u8> + Send + Sync + 'static,
    ) -> TestServer {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_task = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let hits = hits_task.clone();
                let out = handler(&hits);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    // Best-effort: read whatever the client sent so far and
                    // move on: this is a test double, not a parser.
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(&out).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        TestServer { addr, hits }
    }
}
