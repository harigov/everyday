//! What `everyday://news/...` serves: a story's body as a whole document for
//! the sandboxed frame, and the pictures inside it.
//!
//! The same two walls mail's own body has: the HTML was sanitised once, when
//! it arrived, and the document carries its own content-security-policy
//! `<meta>` in case the first wall missed something. A mail-backed story is
//! served by mail's own [`crate::mailview::body_document`].
//!
//! Both transports call straight into this file -- `everyday-app`'s
//! `everyday://news/...` and `everyday-server`'s `/v1/news/...` -- for the
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

use crate::error::CommandResult;
// The policy, the refusal pixel and the picture fetch are mail's own: the
// document is drawn by the same frame and a picture is fetched by the same
// rules -- public addresses only, raster only, capped -- whichever app's
// body named it.
use crate::mailview::{
    self, BodyDocument, CSP, PartResponse, fetch_remote_image, placeholder_pixel,
};
use crate::service::blocking;
use everyday_core::Vault;
use everyday_core::id::StoryId;
use everyday_core::media;
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
@media (prefers-color-scheme: dark) {
  body { color: #eceaf0; background: #17161a; }
  figcaption, .quiet { color: #7d7a86; }
  blockquote { border-left-color: #3a3841; color: #b4b1bd; }
}
"#;

/// What a story with nothing to read says, in place of a blank frame.
const NOTHING: &str = "Nothing came with this story. Open the original to read it.";

/// The whole document for `story`'s frame. `images_hidden` is true when the
/// story has pictures and its source has them switched off.
///
/// Three shapes, by what the story has:
///
/// * A newsletter is mail, and is drawn by mail, under mail's own rules about
///   remote pictures -- the sender allow-list, nothing else. "Show images
///   just this once" lives in a session, and a session is the one thing this
///   function is not handed; it is given in Mail.
/// * A story with a body gets the body the sanitiser stored.
/// * A story with none gets its summary, escaped, so the pane is never a
///   blank rectangle the interface has to explain.
pub fn body_document(vault: &Vault, story: StoryId) -> CommandResult<BodyDocument> {
    let story = vault.story(story)?;

    if let Some(message) = story.mail_message {
        let allowed = mailview::remote_images_allowed(vault, message, false)?;
        return mailview::body_document(vault, message, allowed);
    }

    let body = vault.story_body(story.id)?;
    if let Some(body) = body.as_ref().filter(|b| !b.html.trim().is_empty()) {
        let source = vault.news_source(story.source_id)?;
        let images_hidden = !source.images && !body.images.is_empty();
        return Ok(BodyDocument { html: wrap(&body.html), images_hidden });
    }

    // An extraction that came out as text alone is still a body; a story
    // with neither has the summary the list already drew.
    let text = body
        .as_ref()
        .map(|b| b.text.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(story.summary.as_str());
    Ok(BodyDocument { html: wrap(&paragraphs(text)), images_hidden: false })
}

/// The document around `inner`: the policy, the style, and links that open
/// outside the frame.
fn wrap(inner: &str) -> String {
    format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
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
    use everyday_core::id::{AccountId, MailMessageId, PackId, ThreadId};
    use everyday_core::mail::{
        Address, Body, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use everyday_core::news::{BodyOrigin, NewsImage, NewsSource, SourceKind, Story, StoryBody};
    use everyday_core::packstore::PackRef;
    use everyday_core::store::mail::IngestMessage;
    use jiff::Timestamp;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_vault() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = everyday_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = everyday_vault::create(dir.path(), cfg).unwrap();
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
        let doc = body_document(&vault, id).unwrap();
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
        let html = "<p><img src=\"everyday://news/img/s/abc123\"></p>";

        let (off, _) = seeded(&vault, false, Some(html), "https://cdn.example/a.png");
        assert!(body_document(&vault, off).unwrap().images_hidden);

        let (on, _) = seeded(&vault, true, Some(html), "https://cdn.example/a.png");
        assert!(!body_document(&vault, on).unwrap().images_hidden);

        // Off, but nothing to hide: there is no bar to offer.
        let (bare, _) = seeded(&vault, false, Some("<p>text only</p>"), "");
        assert!(!body_document(&vault, bare).unwrap().images_hidden);
    }

    #[test]
    fn a_story_with_no_body_shows_its_summary_escaped() {
        let (_dir, vault) = test_vault();
        let (id, _) = seeded(&vault, true, None, "");
        let doc = body_document(&vault, id).unwrap();
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
        let doc = body_document(&vault, id).unwrap();
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
        let doc = body_document(&vault, id).unwrap();
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

        let source =
            NewsSource::new(SourceKind::Mail { sender: "letter@example.com".into() }, "Letter");
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
        let doc = body_document(&vault, id).unwrap();
        assert!(doc.html.contains("<p>This week in letters</p>"), "{}", doc.html);
        assert!(doc.html.contains("default-src 'none'"));
        // Mail's wrapper, not the article one: its own stylesheet is absent.
        assert!(!doc.html.contains(".plain"), "{}", doc.html);
        assert!(!doc.images_hidden);
    }

    #[test]
    fn a_story_that_does_not_exist_is_not_found() {
        let (_dir, vault) = test_vault();
        let e = body_document(&vault, StoryId::new()).unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
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
