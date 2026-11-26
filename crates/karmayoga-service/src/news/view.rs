//! What `karmayoga://news/...` serves: a story's body as a whole document for
//! the sandboxed frame, and the pictures inside it.
//!
//! The same two walls mail's own body has: the HTML was sanitised once, when
//! it arrived, and the document carries its own content-security-policy
//! `<meta>` in case the first wall missed something. A mail-backed story is
//! served by mail's own [`crate::mail::view::body_document`].
//!
//! A story's pane has more than one document in it -- an episode's notes, its
//! transcript, its summary -- and each is asked for by name, as
//! `?part=notes|transcript|summary` on the same address ([`BodyPart`]). Every
//! one is drawn the same way, in the same frame, under the same policy.
//!
//! Both transports call straight into this file -- `karmayoga-app`'s
//! `karmayoga://news/...` and `karmayoga-server`'s `/v1/news/...` -- for the
//! reason `mailview` gives: a rendered body is bytes behind a byte-serving
//! route, not a JSON result, and the logic should not be written twice.
//!
//! # Pictures
//!
//! A story's HTML names its pictures by token, and the token is the only
//! thing the webview can ask for. The address behind it never leaves this
//! file's reach: it is fetched here, over [`crate::http::public_client`],
//! once, and kept in the blob store, so a story read twice -- or read with no
//! network at all, the second time -- costs one request.
//!
//! The fetch is mail's, restated. `mailview` keeps its own copy private, so
//! the size cap, the raster-only sniffing and the check that a literal
//! private address never reaches a socket are written out again below; they
//! are small, and they are the three things that must not differ.

use super::article::{SHOW_NOTES_HEADING, SHOW_NOTES_MARKER};
use super::sanitize;
use crate::error::{CommandError, CommandResult, codes};
// The policy, the refusal pixel and the picture fetch are mail's own: the
// document is drawn by the same frame and a picture is fetched by the same
// rules -- public addresses only, raster only, capped -- whichever app's
// body named it.
use crate::mail::view::{
    self as mailview, BodyDocument, CSP, PartResponse, fetch_remote_image, placeholder_pixel,
};
use crate::service::blocking;
use karmayoga_core::Vault;
use karmayoga_core::files::{FileOrigin, FileStatus};
use karmayoga_core::id::{FileId, StoryId};
use karmayoga_core::media;
use karmayoga_core::news::transcript::{TimedText, format_clock, parse_markdown};
use karmayoga_core::news::{BodyOrigin, Story, StoryBody};
use pulldown_cmark::{Options, Parser};
use std::sync::Arc;

/// What a story starts from. Plain CSS in the document, not a `<link>`:
/// there is nowhere for a stylesheet to load from inside the policy above.
///
/// The colours are the ones mail's document uses, and those are
/// `theme.css`'s, copied for the reason mail gives -- this document has no
/// build step and cannot see a custom property defined outside it. What is
/// different is the typography. An article is read for longer than a message
/// is, so its code blocks keep their lines, its quotes are set apart, and its
/// pictures and tables never push the page sideways.
const STYLE: &str = r#"
:root { color-scheme: light dark; }
body {
  margin: 0;
  padding: 16px;
  font-family: 'Source Sans 3 Variable', 'Source Sans Pro', 'Seravek', 'Ubuntu',
    'Segoe UI Variable Text', 'Segoe UI', -apple-system, BlinkMacSystemFont, system-ui,
    'Noto Sans', sans-serif;
  font-size: 15px;
  line-height: 1.65;
  color: #1c1a17;
  background: #f7f6f3;
  overflow-wrap: break-word;
}
a { color: inherit; }
h1, h2, h3, h4, h5, h6 { line-height: 1.3; }
img, video { max-width: 100%; height: auto; }
figure { margin: 1em 0; }
figcaption { font-size: 13px; color: #8b857c; }
blockquote { margin: 1em 0; padding-left: 1em; border-left: 3px solid #d9d6cf; color: #5d5951; }
pre {
  overflow-x: auto;
  padding: 12px;
  border-radius: 6px;
  background: rgba(128, 128, 128, 0.12);
  font-size: 13px;
}
code { font-family: ui-monospace, 'SF Mono', 'JetBrains Mono', Menlo, Consolas, monospace; }
table { max-width: 100%; border-collapse: collapse; }
td, th { padding: 4px 8px; }
.plain { white-space: pre-line; }
.quiet { color: #8b857c; }
.said { margin: 0 0 1.1em; }
.said p { margin: 0 0 0.5em; }
.when { display: block; font-size: 12px; line-height: 1.4; font-variant-numeric: tabular-nums; color: #8b857c; }
@media (prefers-color-scheme: dark) {
  body { color: #eceaf0; background: #17161a; }
  figcaption, .quiet, .when { color: #7d7a86; }
  blockquote { border-left-color: #3a3841; color: #b4b1bd; }
}
"#;

/// What a story with nothing to read says, in place of a blank frame.
const NOTHING: &str = "Nothing came with this story. Open the original to read it.";

/// What a transcript file with no words in it says.
const NO_WORDS: &str = "This transcript has no words in it.";

/// What stands under a document made from a file that was longer than is read.
const CUT: &str = "The rest of this file is in Files.";

/// The most of a story's file that is read into a document, in bytes. A
/// conversation of four hours is under a megabyte of transcript; past this a
/// file is not one to draw in a pane, and Files has it whole.
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// Which of a story's documents is wanted: the `?part=` of
/// `karmayoga://news/body/{story}?part=...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyPart {
    /// What a story's pane has always drawn, the whole body.
    #[default]
    Whole,
    /// An episode's show notes; for anything else, the body.
    Notes,
    /// An episode's article, written from its transcript; for anything else,
    /// the summary kept in Files.
    Summary,
    /// What was said in an episode, with when.
    Transcript,
}

impl BodyPart {
    /// The part a `?part=` value names. Not a place to fail: a value this
    /// does not know -- a newer interface's, a typed address -- is the whole
    /// body, which is what no value at all is.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "notes" => Self::Notes,
            "summary" => Self::Summary,
            "transcript" => Self::Transcript,
            _ => Self::Whole,
        }
    }

    /// The `?part=` value that asks for this part, or `None` for the whole
    /// body, which is asked for with none.
    pub fn query(self) -> Option<&'static str> {
        match self {
            Self::Whole => None,
            Self::Notes => Some("notes"),
            Self::Summary => Some("summary"),
            Self::Transcript => Some("transcript"),
        }
    }
}

/// The whole document for `story`'s frame, or the part of it `part` names.
/// `images_hidden` is true when the story has pictures and its source has them
/// switched off.
///
/// [`BodyPart::Whole`] has three shapes, by what the story has:
///
/// * A newsletter is mail, and is drawn by mail, under mail's own rules about
///   remote pictures -- the sender allow-list, nothing else. "Show images
///   just this once" lives in a session, and a session is the one thing this
///   function is not handed; it is given in Mail.
/// * A story with a body gets the body the sanitiser stored.
/// * A story with none gets its summary, escaped, so the pane is never a
///   blank rectangle the interface has to explain.
///
/// The other parts are the tabs of a pane:
///
/// * **Notes**: for a body written from a transcript, the feed's own show
///   notes, which `article.rs` keeps below the article; for any other story,
///   as the whole body is, since an article's own words are its notes.
/// * **Summary**: for a body written from a transcript, the article above the
///   show notes; for any other story, the summary kept in Files
///   ([`karmayoga_core::news::StoryFiles::summary`]) as Markdown, or not found.
/// * **Transcript**: the transcript kept in Files, a paragraph at a time with
///   its time, or not found.
pub fn body_document(vault: &Vault, story: StoryId, part: BodyPart) -> CommandResult<BodyDocument> {
    let story = vault.story(story)?;
    match part {
        BodyPart::Whole => whole(vault, &story),
        BodyPart::Notes => notes(vault, &story),
        BodyPart::Summary => summary(vault, &story),
        BodyPart::Transcript => transcript(vault, &story),
    }
}

fn whole(vault: &Vault, story: &Story) -> CommandResult<BodyDocument> {
    if let Some(message) = story.mail_message {
        let allowed = mailview::remote_images_allowed(vault, message, false)?;
        return mailview::body_document(vault, message, allowed);
    }
    stored(vault, story, vault.story_body(story.id)?)
}

/// A story's own body, or failing that its summary: what a story that is not
/// mail has to draw.
fn stored(vault: &Vault, story: &Story, body: Option<StoryBody>) -> CommandResult<BodyDocument> {
    if let Some(body) = body.as_ref().filter(|b| !b.html.trim().is_empty()) {
        let source = vault.news_source(story.source_id)?;
        let images_hidden = !source.images && !body.images.is_empty();
        return Ok(BodyDocument { html: wrap(&body.html, Some(body.origin)), images_hidden });
    }

    // An extraction that came out as text alone is still a body; a story
    // with neither has the summary the list already drew.
    let text = body
        .as_ref()
        .map(|b| b.text.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(story.summary.as_str());
    Ok(BodyDocument { html: wrap(&paragraphs(text), None), images_hidden: false })
}

// ---- an episode's, an article's tabs ------------------------------------

/// What lies above and below the rule an article written from a transcript
/// has between it and the feed's show notes (see `article.rs`): the article,
/// and the notes without their heading. A body with no rule is all article.
fn split_article(html: &str) -> (&str, &str) {
    let (article, notes) = html.split_once(SHOW_NOTES_MARKER).unwrap_or((html, ""));
    let notes = notes.trim_start();
    (article, notes.strip_prefix(SHOW_NOTES_HEADING).unwrap_or(notes))
}

fn notes(vault: &Vault, story: &Story) -> CommandResult<BodyDocument> {
    if story.mail_message.is_some() {
        return whole(vault, story);
    }
    let body = vault.story_body(story.id)?;
    let Some(written) = body.as_ref().filter(|b| b.origin == BodyOrigin::Transcript) else {
        return stored(vault, story, body);
    };

    // The article is the summary's, not the notes'. What is left is what the
    // feed carried, which `show_notes` in `article.rs` kept: a feed's, whatever
    // it was before the article went above it.
    let (_, notes) = split_article(&written.html);
    if notes.trim().is_empty() {
        // An episode whose feed carried nothing: the short description its
        // row drew is all the notes there are.
        return Ok(BodyDocument {
            html: wrap(&paragraphs(&story.summary), None),
            images_hidden: false,
        });
    }
    let source = vault.news_source(story.source_id)?;
    let images_hidden = !source.images && !written.images.is_empty();
    Ok(BodyDocument { html: wrap(notes, Some(BodyOrigin::Feed)), images_hidden })
}

fn summary(vault: &Vault, story: &Story) -> CommandResult<BodyDocument> {
    // An episode's summary is the article written from its transcript, which
    // is its body.
    if let Some(body) = vault.story_body(story.id)?.filter(|b| b.origin == BodyOrigin::Transcript) {
        let (article, _) = split_article(&body.html);
        if !article.trim().is_empty() {
            return Ok(BodyDocument {
                html: wrap(article, Some(BodyOrigin::Transcript)),
                images_hidden: false,
            });
        }
    }

    let kept = file_text(vault, story.files.summary, "This story has no summary yet.")?;
    let mut html = markdown_html(story.id, without_front_matter(&kept.text));
    if kept.cut {
        html.push_str(&format!("<p class=\"quiet\">{CUT}</p>\n"));
    }
    Ok(BodyDocument { html: wrap(&html, None), images_hidden: false })
}

fn transcript(vault: &Vault, story: &Story) -> CommandResult<BodyDocument> {
    let kept = file_text(vault, story.files.transcript, "This story has no transcript yet.")?;
    let said = parse_markdown(&kept.text);
    let mut html = if !said.is_empty() {
        said_html(&said)
    } else if kept.text.trim().is_empty() {
        format!("<p class=\"quiet\">{NO_WORDS}</p>\n")
    } else {
        // Words with no times in front of them -- somebody typed a
        // transcript in -- are still the transcript.
        paragraphs(&kept.text)
    };
    if kept.cut {
        html.push_str(&format!("<p class=\"quiet\">{CUT}</p>\n"));
    }
    // Not a body's origin, so a pane that says "written from the transcript"
    // of one does not say it of the transcript itself.
    Ok(BodyDocument { html: wrap_as(&html, "transcriptFile"), images_hidden: false })
}

/// A story's file, read as text.
struct FileText {
    text: String,
    /// The file is longer than [`MAX_FILE_BYTES`], and this is the start of it.
    cut: bool,
}

/// The text of `file`, which a story names, plain in the files folder or
/// sealed in the vault alike; not found, in `missing`'s words, when the story
/// names none, or it is in the trash or gone from the folder.
///
/// Health's files are never read here: what a clinical document says is
/// Health's to show, behind Health's own gate, and this route has only the
/// Feed's.
fn file_text(vault: &Vault, file: Option<FileId>, missing: &str) -> CommandResult<FileText> {
    let not_found = || CommandError::new(codes::NOT_FOUND, missing);
    let Some(file) = file else { return Err(not_found()) };
    let entry = match vault.file(file) {
        Ok(entry) => entry,
        Err(karmayoga_core::Error::NotFound { .. }) => return Err(not_found()),
        Err(e) => return Err(e.into()),
    };
    if entry.origin == FileOrigin::Health {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "That is one of Health's files, which the Feed does not show.",
        ));
    }
    if !matches!(entry.status, FileStatus::Present) {
        return Err(not_found());
    }
    let text = vault.read_file_text(file, MAX_FILE_BYTES)?;
    Ok(FileText { text, cut: entry.size > MAX_FILE_BYTES as u64 })
}

/// `markdown` as the Feed's HTML: the way `article.rs` makes an article's,
/// raw HTML made text and pictures made words, then through the same
/// sanitiser as any body. A model wrote it, and a person may have edited it;
/// it is not markup this application vouches for.
fn markdown_html(story: StoryId, markdown: &str) -> String {
    let parser = Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, crate::delivery::inert(parser));
    // No base address, as for an article: there is no page it was read from.
    sanitize::body(story, &html, "", BodyOrigin::Transcript).html
}

/// `markdown` without the front matter a file the Feed keeps may begin with:
/// the `---` block `keep.rs` writes, which is for the document index and not
/// for reading.
fn without_front_matter(markdown: &str) -> &str {
    let Some(rest) = markdown.strip_prefix("---\n").or_else(|| markdown.strip_prefix("---\r\n"))
    else {
        return markdown;
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        at += line.len();
        if line.trim_end() == "---" {
            return &rest[at..];
        }
    }
    // Never closed: it was a rule at the top, not front matter.
    markdown
}

/// A transcript's paragraphs, each with the time it was said at as a small
/// label above it, and the speaker in bold when the file names one.
///
/// Every word is escaped and the markup is this function's own, so nothing
/// here needs the sanitiser a body's HTML does: a transcript is a stranger's
/// words, never a stranger's markup.
fn said_html(said: &[TimedText]) -> String {
    let mut out = String::new();
    for line in said {
        out.push_str("<div class=\"said\"><span class=\"when\">");
        out.push_str(&format_clock(line.start_ms));
        out.push_str("</span>");
        // A paragraph a plain-text transcript had more than one of is one
        // entry, with a blank line between; the time is on the first.
        let blocks = line.text.split("\n\n").map(str::trim).filter(|b| !b.is_empty());
        for (i, block) in blocks.enumerate() {
            out.push_str("<p>");
            if let Some(speaker) = line.speaker.as_deref().filter(|_| i == 0) {
                out.push_str("<strong>");
                out.push_str(&escape(speaker));
                out.push_str(":</strong> ");
            }
            out.push_str(&escape(block));
            out.push_str("</p>");
        }
        out.push_str("</div>\n");
    }
    out
}

/// The document around `inner`: the policy, the style, and links that open
/// outside the frame.
fn wrap(inner: &str, origin: Option<BodyOrigin>) -> String {
    // Where the body came from, for the window to say "written from the
    // episode's transcript": a word of ours in the head, never the
    // publisher's.
    let origin = match origin {
        Some(BodyOrigin::Feed) => "feed",
        Some(BodyOrigin::Extracted) => "extracted",
        Some(BodyOrigin::HackerNews) => "hackerNews",
        Some(BodyOrigin::Transcript) => "transcript",
        None => "summary",
    };
    wrap_as(inner, origin)
}

/// [`wrap`] with the word for where it came from given.
fn wrap_as(inner: &str, origin: &str) -> String {
    format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"news-body-origin\" content=\"{origin}\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\n\
         <base target=\"_blank\">\n\
         <style>{STYLE}</style>\n\
         </head>\n\
         <body>\n{inner}\n</body>\n\
         </html>\n"
    )
}

/// Plain text as paragraphs, escaped. A blank line ends one; a single line
/// break is kept, as the summary was written with it.
fn paragraphs(text: &str) -> String {
    let mut out = String::new();
    for paragraph in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        out.push_str("<p class=\"plain\">");
        out.push_str(&escape(paragraph));
        out.push_str("</p>\n");
    }
    if out.is_empty() {
        out.push_str("<p class=\"quiet\">");
        out.push_str(NOTHING);
        out.push_str("</p>\n");
    }
    out
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---- pictures -----------------------------------------------------------

/// One picture from `story`'s body, by token: from the blob store when it
/// was fetched before, else over [`crate::http::public_client`], then cached.
/// A source with pictures off, or a token the body does not name, gets a
/// transparent pixel, as mail's does.
///
/// In this order, and the order is the safety of it. First whether the source
/// shows pictures at all, checked fresh on every call: a person can switch
/// them on after a document was drawn, and the next request must see that.
/// Refusing is not an error but the pixel, so that nothing about *why*
/// nothing was fetched is visible from outside. Then the token, which must be
/// one the stored body names -- the address is never taken from the request.
/// Then the cache, and only then the network.
pub async fn image(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    story: StoryId,
    token: &str,
) -> CommandResult<PartResponse> {
    image_inner(vault, client, story, token, false).await
}

/// The one door a test walks a real fetch through a server on `127.0.0.1`
/// by, without weakening [`image`] itself. Only compiled into a test binary.
#[cfg(test)]
async fn image_allowing_loopback_for_test(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    story: StoryId,
    token: &str,
) -> CommandResult<PartResponse> {
    image_inner(vault, client, story, token, true).await
}

enum Plan {
    Pixel,
    Cached(Vec<u8>),
    Fetch { url: String },
}

async fn image_inner(
    vault: &Arc<Vault>,
    client: &reqwest::Client,
    story: StoryId,
    token: &str,
    allow_loopback: bool,
) -> CommandResult<PartResponse> {
    let plan = {
        let vault = vault.clone();
        let token = token.to_string();
        blocking(move || {
            let story = vault.story(story)?;
            if !vault.news_source(story.source_id)?.images {
                return Ok(Plan::Pixel);
            }
            let Some(body) = vault.story_body(story.id)? else { return Ok(Plan::Pixel) };
            let Some(found) = body.images.iter().find(|i| i.token == token) else {
                return Ok(Plan::Pixel);
            };
            if let Some(blob) = found.cached_blob {
                return Ok(Plan::Cached(vault.blob(blob)?));
            }
            Ok(Plan::Fetch { url: found.url.clone() })
        })
        .await?
    };

    let bytes = match plan {
        Plan::Pixel => return Ok(placeholder_pixel()),
        Plan::Cached(bytes) => bytes,
        Plan::Fetch { url } => {
            let fetched = fetch_remote_image(client, &url, allow_loopback).await?;
            keep(vault, story, token, &fetched).await;
            fetched
        }
    };
    let content_type = media::sniff_mime(&bytes).to_string();
    Ok(PartResponse { content_type, bytes, attachment: false, filename: None })
}

/// Remember a fetched picture: into the blob store, and into the body's
/// entry for it, in one pass over a body read afresh -- an extraction can
/// have replaced the body while the picture was on its way.
///
/// Best effort. A vault that cannot be written -- a read-only copy, a full
/// disk -- should still show the picture it just fetched, and only pay for
/// it again next time.
async fn keep(vault: &Arc<Vault>, story: StoryId, token: &str, bytes: &[u8]) {
    let (vault, token, bytes) = (vault.clone(), token.to_string(), bytes.to_vec());
    let kept = blocking(move || {
        let blob = vault.put_blob(&bytes)?;
        let Some(mut body) = vault.story_body(story)? else { return Ok(()) };
        if let Some(image) = body.images.iter_mut().find(|i| i.token == token) {
            image.cached_blob = Some(blob);
        }
        Ok(vault.save_story_body(&body)?)
    })
    .await;
    if let Err(e) = kept {
        tracing::info!(error = %e, "could not keep a story's picture");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::codes;
    use crate::http;
    use jiff::Timestamp;
    use karmayoga_core::id::{AccountId, MailMessageId, PackId, ThreadId};
    use karmayoga_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use karmayoga_core::news::{NewsImage, NewsSource, SourceKind};
    use karmayoga_core::packstore::PackRef;
    use karmayoga_core::store::mail::IngestMessage;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_vault() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = karmayoga_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = karmayoga_vault::create(dir.path(), cfg).unwrap();
        (dir, Arc::new(vault))
    }

    fn png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 16]);
        png
    }

    const SUMMARY: &str = "A <b>short</b> & plain summary";

    /// A web source, with pictures on or off, and one story under it with
    /// `html` for a body (or none) naming a picture at `image_url`.
    fn seeded(
        vault: &Vault,
        images: bool,
        html: Option<&str>,
        image_url: &str,
    ) -> (StoryId, String) {
        seeded_with(vault, images, html, image_url, SUMMARY)
    }

    fn seeded_with(
        vault: &Vault,
        images: bool,
        html: Option<&str>,
        image_url: &str,
        summary: &str,
    ) -> (StoryId, String) {
        let mut source =
            NewsSource::new(SourceKind::Web { url: "https://blog.example/feed".into() }, "Blog");
        source.images = images;
        vault.save_news_source(&source).unwrap();

        let token = "abc123".to_string();
        let mut story = Story::new(
            source.id,
            vault.news_key("https://blog.example/post").unwrap(),
            "A post",
            Timestamp::now(),
        );
        story.summary = summary.to_string();
        let body = html.map(|html| StoryBody {
            story_id: story.id,
            html: html.to_string(),
            text: String::new(),
            origin: BodyOrigin::Feed,
            images: if image_url.is_empty() {
                Vec::new()
            } else {
                let (token, url) = (token.clone(), image_url.to_string());
                vec![NewsImage { token, url, cached_blob: None }]
            },
        });
        let id = story.id;
        vault.insert_stories(&[(story, body)]).unwrap();
        (id, token)
    }

    // ---- the document -----------------------------------------------------

    #[test]
    fn a_body_is_wrapped_with_the_policy_and_never_gains_a_script() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>Hello</p>"), "");
        let doc = body_document(&vault, id, BodyPart::Whole).unwrap();
        assert!(doc.html.contains("<p>Hello</p>"));
        assert!(doc.html.contains("Content-Security-Policy"));
        assert!(doc.html.contains("default-src 'none'"));
        assert!(doc.html.contains("<base target=\"_blank\">"));
        assert!(!doc.html.to_ascii_lowercase().contains("<script"), "{}", doc.html);
        assert!(!doc.images_hidden);
    }

    #[test]
    fn pictures_are_hidden_only_when_the_source_has_them_off_and_the_body_names_some() {
        let (_dir, vault) = test_vault();
        let html = "<p><img src=\"karmayoga://news/img/s/abc123\"></p>";

        let (off, _) = seeded(&vault, false, Some(html), "https://cdn.example/a.png");
        assert!(body_document(&vault, off, BodyPart::Whole).unwrap().images_hidden);

        let (on, _) = seeded(&vault, true, Some(html), "https://cdn.example/a.png");
        assert!(!body_document(&vault, on, BodyPart::Whole).unwrap().images_hidden);

        // Off, but nothing to hide: there is no bar to offer.
        let (bare, _) = seeded(&vault, false, Some("<p>text only</p>"), "");
        assert!(!body_document(&vault, bare, BodyPart::Whole).unwrap().images_hidden);
    }

    #[test]
    fn a_story_with_no_body_shows_its_summary_escaped() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let doc = body_document(&vault, id, BodyPart::Whole).unwrap();
        let escaped = "A &lt;b&gt;short&lt;/b&gt; &amp; plain summary";
        assert!(doc.html.contains(escaped), "{}", doc.html);
        assert!(!doc.html.contains("<b>short</b>"));
        assert!(doc.html.contains("Content-Security-Policy"));
        assert!(!doc.images_hidden);
    }

    #[test]
    fn a_story_with_nothing_at_all_says_so() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded_with(&vault, true, None, "", "");
        let doc = body_document(&vault, id, BodyPart::Whole).unwrap();
        assert!(doc.html.contains(NOTHING), "{}", doc.html);
    }

    #[test]
    fn a_body_that_is_only_text_is_shown_as_text() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let body = StoryBody {
            story_id: id,
            html: String::new(),
            text: "First.\n\nSecond <one>.".into(),
            origin: BodyOrigin::Extracted,
            images: Vec::new(),
        };
        vault.save_story_body(&body).unwrap();
        let doc = body_document(&vault, id, BodyPart::Whole).unwrap();
        assert!(doc.html.contains("<p class=\"plain\">First.</p>"), "{}", doc.html);
        assert!(doc.html.contains("Second &lt;one&gt;."));
    }

    /// A newsletter story: a source that follows one sender, a message from
    /// them whose stored body is `html`, and a story pointing at it.
    fn newsletter(vault: &Vault, html: &str) -> StoryId {
        let account = AccountId::new();
        let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        let id = MailMessageId::new();
        let message = Message {
            id,
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{id}@letter.example>"),
            date: Timestamp::now(),
            from: Address::bare("letter@example.com"),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Issue 1".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
            invite: None,
        };
        vault
            .ingest_mail(account, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
            .unwrap();
        vault
            .save_body(&Body {
                message_id: id,
                html_sanitised: html.to_string(),
                text: String::new(),
                quoted_ranges: Vec::new(),
                signature_range: None,
                parts: Vec::new(),
                remote_images: Vec::new(),
            })
            .unwrap();

        let source = NewsSource::new(
            SourceKind::Mail { sender: "letter@example.com".into(), name: String::new() },
            "Letter",
        );
        vault.save_news_source(&source).unwrap();
        let key = vault.news_key(&id.to_string()).unwrap();
        let mut story = Story::new(source.id, key, "Issue 1", Timestamp::now());
        story.mail_message = Some(id);
        let story_id = story.id;
        vault.insert_stories(&[(story, None)]).unwrap();
        story_id
    }

    #[test]
    fn a_newsletter_is_drawn_by_mail_not_by_this_file() {
        let (_dir, vault) = test_vault();
        let id = newsletter(&vault, "<p>This week in letters</p>");
        let doc = body_document(&vault, id, BodyPart::Whole).unwrap();
        assert!(doc.html.contains("<p>This week in letters</p>"), "{}", doc.html);
        assert!(doc.html.contains("default-src 'none'"));
        // Mail's wrapper, not the article one: its own stylesheet is absent.
        assert!(!doc.html.contains(".plain"), "{}", doc.html);
        assert!(!doc.images_hidden);
    }

    #[test]
    fn a_story_that_does_not_exist_is_not_found() {
        let (_dir, vault) = test_vault();
        for part in [BodyPart::Whole, BodyPart::Notes, BodyPart::Summary, BodyPart::Transcript] {
            let e = body_document(&vault, StoryId::new(), part).unwrap_err();
            assert_eq!(e.code, codes::NOT_FOUND, "{part:?}");
        }
    }

    // ---- the parts of a pane ----------------------------------------------

    use karmayoga_core::files::{FileEntry, ImportOptions};

    fn part(vault: &Vault, story: StoryId, part: BodyPart) -> BodyDocument {
        body_document(vault, story, part).unwrap()
    }

    /// A story whose body was written from a transcript: the article, then
    /// (when `notes` is given) the rule and the show notes under their heading,
    /// as `article.rs` stores them.
    fn episode(vault: &Vault, notes: Option<&str>) -> StoryId {
        let (id, _) = seeded(vault, true, None, "");
        let html = match notes {
            Some(notes) => {
                format!(
                    "<h1>The article</h1><p>Written.</p>{SHOW_NOTES_MARKER}{SHOW_NOTES_HEADING}{notes}"
                )
            }
            None => "<h1>The article</h1><p>Written.</p>".to_string(),
        };
        let body = StoryBody {
            story_id: id,
            html,
            text: String::new(),
            origin: BodyOrigin::Transcript,
            images: Vec::new(),
        };
        vault.save_story_body(&body).unwrap();
        id
    }

    fn keep(vault: &Vault, name: &str, text: &str, origin: FileOrigin, sealed: bool) -> FileEntry {
        let options = ImportOptions { encrypted: Some(sealed), ..ImportOptions::new(origin) };
        vault.import_bytes(text.as_bytes(), name, &options).unwrap()
    }

    fn origin_of(doc: &BodyDocument) -> &str {
        let (_, after) = doc.html.split_once("name=\"news-body-origin\" content=\"").unwrap();
        after.split('"').next().unwrap()
    }

    #[test]
    fn a_part_is_read_from_the_query_value_and_anything_else_is_the_whole_body() {
        assert_eq!(BodyPart::parse("notes"), BodyPart::Notes);
        assert_eq!(BodyPart::parse("summary"), BodyPart::Summary);
        assert_eq!(BodyPart::parse("transcript"), BodyPart::Transcript);
        assert_eq!(BodyPart::parse(" Transcript "), BodyPart::Transcript);
        for other in ["", "whole", "everything", "../notes", "notes,summary"] {
            assert_eq!(BodyPart::parse(other), BodyPart::Whole, "{other:?}");
        }
        // And back: the value a client puts on the address.
        for part in [BodyPart::Whole, BodyPart::Notes, BodyPart::Summary, BodyPart::Transcript] {
            assert_eq!(BodyPart::parse(part.query().unwrap_or("")), part);
        }
    }

    #[test]
    fn a_body_written_from_a_transcript_splits_at_the_marker() {
        let (_dir, vault) = test_vault();
        let id = episode(&vault, Some("<p>Guest: Anna</p>"));

        let notes = part(&vault, id, BodyPart::Notes);
        assert!(notes.html.contains("<p>Guest: Anna</p>"), "{}", notes.html);
        assert!(!notes.html.contains("Show notes"), "the heading is the tab's: {}", notes.html);
        assert!(!notes.html.contains("The article"));
        assert!(!notes.html.contains("data-show-notes"));
        // What the feed carried, not what was written from the transcript.
        assert_eq!(origin_of(&notes), "feed");

        let summary = part(&vault, id, BodyPart::Summary);
        assert!(summary.html.contains("<h1>The article</h1><p>Written.</p>"), "{}", summary.html);
        assert!(!summary.html.contains("Guest: Anna"));
        assert!(!summary.html.contains("data-show-notes"));
        assert_eq!(origin_of(&summary), "transcript");

        // The whole is as it always was: both, and the rule between them.
        let whole = part(&vault, id, BodyPart::Whole);
        assert!(whole.html.contains("The article") && whole.html.contains("Guest: Anna"));
        assert!(whole.html.contains("data-show-notes"));
        assert_eq!(origin_of(&whole), "transcript");

        // Every part keeps the head: the policy, the base, the style.
        for doc in [&notes, &summary] {
            assert!(doc.html.contains("default-src 'none'"));
            assert!(doc.html.contains("<base target=\"_blank\">"));
            assert!(doc.html.contains(".when {"));
        }
    }

    #[test]
    fn show_notes_with_pictures_are_held_back_as_the_whole_body_is() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, false, None, "");
        let body = StoryBody {
            story_id: id,
            html: format!(
                "<p>Written.</p>{SHOW_NOTES_MARKER}{SHOW_NOTES_HEADING}\
                 <p><img src=\"karmayoga://news/img/s/abc123\"></p>"
            ),
            text: String::new(),
            origin: BodyOrigin::Transcript,
            images: vec![NewsImage {
                token: "abc123".into(),
                url: "https://cdn.example/a.png".into(),
                cached_blob: None,
            }],
        };
        vault.save_story_body(&body).unwrap();
        assert!(part(&vault, id, BodyPart::Notes).images_hidden);
        // The article above them has none to hide.
        assert!(!part(&vault, id, BodyPart::Summary).images_hidden);
    }

    #[test]
    fn an_article_written_with_no_show_notes_under_it_has_the_rows_description_for_notes() {
        let (_dir, vault) = test_vault();
        let id = episode(&vault, None);

        let notes = part(&vault, id, BodyPart::Notes);
        assert!(notes.html.contains("A &lt;b&gt;short&lt;/b&gt; &amp; plain summary"));
        assert!(!notes.html.contains("The article"), "{}", notes.html);
        let summary = part(&vault, id, BodyPart::Summary);
        assert!(summary.html.contains("<h1>The article</h1>"), "{}", summary.html);
    }

    #[test]
    fn the_notes_of_anything_else_are_the_whole_body() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>Hello</p>"), "");
        let (notes, whole) = (part(&vault, id, BodyPart::Notes), part(&vault, id, BodyPart::Whole));
        assert_eq!(notes.html, whole.html);

        // A story with no body at all keeps its summary, as the whole does.
        let (bare, _) = seeded(&vault, true, None, "");
        assert_eq!(
            part(&vault, bare, BodyPart::Notes).html,
            part(&vault, bare, BodyPart::Whole).html
        );

        // And a newsletter is mail's, in either.
        let issue = newsletter(&vault, "<p>This week in letters</p>");
        let notes = part(&vault, issue, BodyPart::Notes);
        assert!(notes.html.contains("<p>This week in letters</p>"), "{}", notes.html);
        assert!(!notes.html.contains(".plain"));
    }

    #[test]
    fn a_transcript_file_is_drawn_a_paragraph_at_a_time_with_its_time_and_escaped() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>Notes</p>"), "");
        let file = keep(
            &vault,
            "transcript.md",
            "# Episode 41: The <i>clocks</i>\n\nThe Slow Hour · 2026-11-20 · transcript\n\n\
             [00:00:00] Welcome <b>back</b>, all.\n\n\
             [01:02:03] **Anna:** The mechanism & the \"church\".\n\n\
             A second thought, still hers.\n",
            FileOrigin::Feed,
            true,
        );
        vault.update_story_files(id, |files| files.transcript = Some(file.id)).unwrap();

        let doc = part(&vault, id, BodyPart::Transcript);
        assert!(doc.html.contains("<span class=\"when\">00:00:00</span>"), "{}", doc.html);
        assert!(doc.html.contains("<span class=\"when\">01:02:03</span>"));
        assert!(doc.html.contains("<p>Welcome &lt;b&gt;back&lt;/b&gt;, all.</p>"), "{}", doc.html);
        assert!(doc.html.contains(
            "<p><strong>Anna:</strong> The mechanism &amp; the &quot;church&quot;.</p><p>A second thought, still hers.</p>"
        ));
        // The heading is not speech, and nothing of the file is markup.
        assert!(
            !doc.html.contains("Episode 41")
                && !doc.html.contains("<b>")
                && !doc.html.contains("<i>")
        );
        assert_eq!(doc.html.matches("class=\"said\"").count(), 2);
        assert!(doc.html.contains("default-src 'none'"));
        assert_eq!(origin_of(&doc), "transcriptFile");
        assert!(!doc.images_hidden);
    }

    #[test]
    fn a_transcript_that_is_a_plain_file_in_the_folder_is_read_as_well() {
        let (_dir, vault) = test_vault();
        let folder = tempfile::tempdir().unwrap();
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        let (id, _) = seeded(&vault, true, None, "");
        let file =
            keep(&vault, "plain.md", "[00:00:07] Said in the open.\n", FileOrigin::Feed, false);
        assert!(!file.is_sealed());
        vault.update_story_files(id, |files| files.transcript = Some(file.id)).unwrap();

        let doc = part(&vault, id, BodyPart::Transcript);
        assert!(doc.html.contains("<span class=\"when\">00:00:07</span>"), "{}", doc.html);
        assert!(doc.html.contains("<p>Said in the open.</p>"));
    }

    #[test]
    fn a_transcript_with_no_times_is_text_and_one_with_nothing_says_so() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let typed = keep(
            &vault,
            "typed.md",
            "Typed in <by> hand.\n\nTwo paragraphs.\n",
            FileOrigin::Files,
            true,
        );
        vault.update_story_files(id, |files| files.transcript = Some(typed.id)).unwrap();
        let doc = part(&vault, id, BodyPart::Transcript);
        assert!(
            doc.html.contains("<p class=\"plain\">Typed in &lt;by&gt; hand.</p>"),
            "{}",
            doc.html
        );
        assert!(doc.html.contains("<p class=\"plain\">Two paragraphs.</p>"));
        assert!(!doc.html.contains("class=\"when\""));

        let empty = keep(&vault, "empty.md", "  \n", FileOrigin::Feed, true);
        vault.update_story_files(id, |files| files.transcript = Some(empty.id)).unwrap();
        assert!(part(&vault, id, BodyPart::Transcript).html.contains(NO_WORDS));
    }

    #[test]
    fn a_file_longer_than_is_read_is_cut_and_says_where_the_rest_is() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let mut text = String::from("[00:00:00] ");
        while text.len() <= MAX_FILE_BYTES + 1024 {
            text.push_str("word ");
        }
        let file = keep(&vault, "long.md", &text, FileOrigin::Feed, true);
        vault.update_story_files(id, |files| files.transcript = Some(file.id)).unwrap();

        let doc = part(&vault, id, BodyPart::Transcript);
        assert!(doc.html.contains(CUT));
        assert!(doc.html.len() < MAX_FILE_BYTES + 64 * 1024, "{}", doc.html.len());
    }

    #[test]
    fn a_story_with_no_transcript_or_summary_is_not_found_and_so_is_one_whose_file_is_gone() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>Hello</p>"), "");
        for which in [BodyPart::Summary, BodyPart::Transcript] {
            let e = body_document(&vault, id, which).unwrap_err();
            assert_eq!(e.code, codes::NOT_FOUND, "{which:?}");
        }

        // Linked, then thrown away: not a document to draw.
        let file = keep(&vault, "gone.md", "[00:00:01] Words.\n", FileOrigin::Feed, true);
        vault
            .update_story_files(id, |files| {
                files.transcript = Some(file.id);
                files.summary = Some(file.id);
            })
            .unwrap();
        assert!(body_document(&vault, id, BodyPart::Transcript).is_ok());
        vault.trash_files(&[file.id]).unwrap();
        for which in [BodyPart::Summary, BodyPart::Transcript] {
            let e = body_document(&vault, id, which).unwrap_err();
            assert_eq!(e.code, codes::NOT_FOUND, "{which:?}");
        }

        // A link to a file the catalog never had.
        vault.update_story_files(id, |files| files.transcript = Some(FileId::new())).unwrap();
        let e = body_document(&vault, id, BodyPart::Transcript).unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
    }

    #[test]
    fn a_summary_file_is_rendered_from_markdown_and_sanitised_like_any_body() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>Hello</p>"), "");
        let file = keep(
            &vault,
            "summary.md",
            "---\ntitle: \"A post\"\nsource: \"Blog\"\n---\n\n# A post\n\nA **plain** claim, [linked](https://example.com/a).\n\n\
             <script>alert(1)</script>\n\n<img src=\"https://tracker.example/p.png\">\n\n\
             ![alt](https://tracker.example/q.png)\n\n- one\n- two\n",
            FileOrigin::Feed,
            true,
        );
        vault.update_story_files(id, |files| files.summary = Some(file.id)).unwrap();

        let doc = part(&vault, id, BodyPart::Summary);
        assert!(doc.html.contains("<h1>A post</h1>"), "{}", doc.html);
        assert!(doc.html.contains("<strong>plain</strong>"));
        assert!(doc.html.contains("href=\"https://example.com/a\""));
        assert!(doc.html.contains("<li>one</li>"));
        // The front matter is for the index, not the reader.
        assert!(!doc.html.contains("source:") && !doc.html.contains("title:"), "{}", doc.html);
        // Nothing that runs and nothing that fetches by itself.
        let lower = doc.html.to_ascii_lowercase();
        assert!(!lower.contains("<script") && !lower.contains("<img"), "{}", doc.html);
        assert!(doc.html.contains("default-src 'none'"));
        assert_eq!(origin_of(&doc), "summary");
        assert!(!doc.images_hidden);

        // The body itself is untouched by having a summary beside it.
        assert!(part(&vault, id, BodyPart::Whole).html.contains("<p>Hello</p>"));
    }

    #[test]
    fn a_newsletter_has_a_summary_in_files_like_any_other_story() {
        let (_dir, vault) = test_vault();
        let issue = newsletter(&vault, "<p>This week in letters</p>");
        assert_eq!(
            body_document(&vault, issue, BodyPart::Summary).unwrap_err().code,
            codes::NOT_FOUND
        );

        let file = keep(&vault, "issue.md", "The week in **brief**.\n", FileOrigin::Feed, true);
        vault.update_story_files(issue, |files| files.summary = Some(file.id)).unwrap();
        let doc = part(&vault, issue, BodyPart::Summary);
        assert!(doc.html.contains("<strong>brief</strong>"), "{}", doc.html);
    }

    #[test]
    fn a_health_file_is_never_drawn_by_the_feed() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let report =
            keep(&vault, "labs.md", "[00:00:00] A clinical thing.\n", FileOrigin::Health, true);
        vault
            .update_story_files(id, |files| {
                files.transcript = Some(report.id);
                files.summary = Some(report.id);
            })
            .unwrap();
        for which in [BodyPart::Summary, BodyPart::Transcript] {
            let e = body_document(&vault, id, which).unwrap_err();
            assert_eq!(e.code, codes::FORBIDDEN, "{which:?}");
        }
    }

    #[test]
    fn front_matter_is_taken_off_only_when_it_is_closed() {
        assert_eq!(without_front_matter("---\na: 1\n---\n\nBody"), "\nBody");
        assert_eq!(without_front_matter("---\r\na: 1\r\n---\r\nBody"), "Body");
        // A rule at the top and nothing closing it.
        assert_eq!(without_front_matter("---\nWords"), "---\nWords");
        assert_eq!(without_front_matter("Words\n---\nMore"), "Words\n---\nMore");
    }

    // ---- pictures ---------------------------------------------------------

    #[test]
    fn the_pixel_decodes_to_a_valid_small_png() {
        let served = placeholder_pixel();
        assert_eq!(served.content_type, "image/png");
        assert!(served.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(served.bytes.len(), 68);
    }

    #[tokio::test]
    async fn a_source_with_pictures_off_gets_the_pixel_and_nothing_is_fetched() {
        let server = spawn_server(|hits| {
            hits.fetch_add(1, Ordering::SeqCst);
            response("image/png", &png())
        })
        .await;
        let (_dir, vault) = test_vault();
        let (id, token) =
            seeded(&vault, false, Some("<p>x</p>"), &format!("http://{}/a.png", server.addr));
        let client = http::client().unwrap();

        let served = image_allowing_loopback_for_test(&vault, client, id, &token).await.unwrap();
        assert_eq!(served.bytes, placeholder_pixel().bytes);
        assert_eq!(server.hits(), 0);
    }

    #[tokio::test]
    async fn a_token_the_body_does_not_name_gets_the_pixel() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, Some("<p>x</p>"), "https://cdn.example/a.png");
        let client = http::client().unwrap();

        let served = image(&vault, client, id, "not-a-token").await.unwrap();
        assert_eq!(served.bytes, placeholder_pixel().bytes);
    }

    #[tokio::test]
    async fn a_picture_is_fetched_once_and_then_served_from_the_vault() {
        let server = spawn_server(|hits| {
            hits.fetch_add(1, Ordering::SeqCst);
            response("image/png", &png())
        })
        .await;
        let (_dir, vault) = test_vault();
        let (id, token) =
            seeded(&vault, true, Some("<p>x</p>"), &format!("http://{}/a.png", server.addr));
        let client = http::client().unwrap();

        let first = image_allowing_loopback_for_test(&vault, client, id, &token).await.unwrap();
        assert_eq!(first.content_type, "image/png");
        assert_eq!(first.bytes, png());
        assert_eq!(server.hits(), 1);

        // Recorded on the body, so a second request never leaves the vault.
        let body = vault.story_body(id).unwrap().unwrap();
        assert!(body.images[0].cached_blob.is_some());
        let second = image_allowing_loopback_for_test(&vault, client, id, &token).await.unwrap();
        assert_eq!(second.bytes, png());
        assert_eq!(server.hits(), 1, "a cached picture must not be fetched again");
    }

    #[tokio::test]
    async fn something_that_is_not_a_picture_is_refused() {
        let server =
            spawn_server(|_| response("image/png", b"<html><script>alert(1)</script>")).await;
        let (_dir, vault) = test_vault();
        let (id, token) =
            seeded(&vault, true, Some("<p>x</p>"), &format!("http://{}/a.png", server.addr));
        let client = http::client().unwrap();

        let e = image_allowing_loopback_for_test(&vault, client, id, &token).await.unwrap_err();
        assert_eq!(e.code, codes::NOT_AN_IMAGE);
    }

    #[tokio::test]
    async fn an_svg_is_refused_whatever_the_server_called_it() {
        let server = spawn_server(|_| {
            response("image/png", br#"<svg onload="alert(1)"><script>x</script></svg>"#)
        })
        .await;
        let (_dir, vault) = test_vault();
        let (id, token) =
            seeded(&vault, true, Some("<p>x</p>"), &format!("http://{}/a.png", server.addr));
        let client = http::client().unwrap();

        let e = image_allowing_loopback_for_test(&vault, client, id, &token).await.unwrap_err();
        assert_eq!(e.code, codes::NOT_AN_IMAGE);
    }

    #[tokio::test]
    async fn a_picture_on_a_private_address_is_never_requested() {
        let server = spawn_server(|hits| {
            hits.fetch_add(1, Ordering::SeqCst);
            response("image/png", &png())
        })
        .await;
        let (_dir, vault) = test_vault();
        let (id, token) =
            seeded(&vault, true, Some("<p>x</p>"), &format!("http://{}/a.png", server.addr));
        let client = http::client().unwrap();

        // The real door, not the test one: loopback is a private address.
        let e = image(&vault, client, id, &token).await.unwrap_err();
        assert_eq!(e.code, codes::FORBIDDEN);
        assert_eq!(server.hits(), 0);
    }

    #[tokio::test]
    async fn a_picture_of_a_story_that_is_gone_is_not_found() {
        // Nothing to serve and nothing to fall back to: the pixel is for a
        // picture that was refused, not for a story that does not exist.
        let (_dir, vault) = test_vault();
        let client = http::client().unwrap();
        let e = image(&vault, client, StoryId::new(), "t").await.unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
    }

    // ---- a tiny local HTTP server, for the fetch tests above --------------

    struct TestServer {
        addr: std::net::SocketAddr,
        hits: Arc<AtomicUsize>,
    }

    impl TestServer {
        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    fn response(content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// Answers every request with whatever `reply` builds, counting through
    /// the counter it is handed. One connection per request, closed after.
    async fn spawn_server(
        reply: impl Fn(&AtomicUsize) -> Vec<u8> + Send + Sync + 'static,
    ) -> TestServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let reply = Arc::new(reply);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let (counter, reply) = (counter.clone(), reply.clone());
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 8 * 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(&(*reply)(counter.as_ref())).await;
                    let _ = socket.flush().await;
                });
            }
        });
        TestServer { addr, hits }
    }
}
