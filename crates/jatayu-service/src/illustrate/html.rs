//! An HTML page drawn into pixels by a headless Chrome.
//!
//! For what an SVG is poor at: text that wraps, a table, a card laid out with
//! flexbox or grid. The browser is the one [`crate::browser`] finds and starts
//! -- the person's own Chrome or Chromium, a throwaway profile, driven over a
//! pipe -- but started differently, because what it does is different. It
//! visits nothing: the page is handed to it whole.
//!
//! # Nothing goes out
//!
//! The page is the model's writing, and a model that has read a stranger's
//! mail may have been told what to write -- an `<img>` whose address carries
//! what it read, say. So the page can reach nothing, by three locks:
//!
//! - **A door that is shut.** The browser's only way out is a proxy that
//!   accepts a connection and closes it at once ([`closed_door`]), with
//!   Chrome's own exception for `localhost` taken away, as the browser
//!   module's proxy has it.
//! - **A content policy** at the top of the page, before a word of the
//!   model's: styles and pictures only from inside the page (`data:`), no
//!   scripts, no frames, no forms.
//! - **No scripts** at all, by the same policy: a drawing is what the markup
//!   and its styles say, and nothing runs to change it.
//!
//! # Warm, briefly
//!
//! Starting Chrome is most of a second; drawing a page in one already started
//! is a tenth of that. A reply that draws three cards should pay the start
//! once, so the browser is kept for [`IDLE_CLOSE`] after the last drawing and
//! then ended. One drawing at a time: each is quick, and one browser is a few
//! hundred megabytes.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

use super::{MAX_PIXELS, MAX_SIDE, Rendered};
use crate::browser::cdp::Cdp;
use crate::browser::launch;
use crate::error::{CommandError, CommandResult, codes};

/// How long a browser nobody has drawn with stays open.
const IDLE_CLOSE: Duration = Duration::from_secs(2 * 60);

/// How many device pixels to a CSS pixel, when the page is small enough to
/// draw that sharply. Text drawn at one to one looks soft on any screen made
/// this decade.
pub(crate) const SHARP: u32 = 2;

/// How tall the page is laid out before it is measured, when no height was
/// asked for. Something a `100vh` can mean.
const FIRST_HEIGHT: u32 = 600;

/// What the page may load: only what is written into it. See the module doc.
const POLICY: &str = "default-src 'none'; style-src 'unsafe-inline' data:; img-src data:; \
                      font-src data:; media-src data:; script-src 'none'; frame-src 'none'; \
                      form-action 'none'; base-uri 'none'";

/// Asked of the page once it has been written: wait for its fonts and
/// pictures, then say how big it came out. Run by the browser for us, which
/// the page's own policy does not stop.
///
/// A lazy picture below the first screen would never load, and so never say
/// it had, so every picture is made eager first; and the waiting is capped,
/// so a page that never settles is drawn as it stands rather than holding
/// the browser until the protocol gives up.
const MEASURE: &str = r"(async () => {
  for (const i of document.images) i.loading = 'eager';
  const settled = (i) => i.complete ? null
    : new Promise((done) => { i.onload = i.onerror = done });
  await Promise.race([
    Promise.all([document.fonts.ready, ...[...document.images].map(settled)]),
    new Promise((done) => setTimeout(done, 3000)),
  ]);
  const root = document.documentElement;
  const tall = Math.max(root.getBoundingClientRect().height,
    root.scrollHeight > innerHeight ? root.scrollHeight : 0);
  return [Math.ceil(root.scrollWidth), Math.ceil(tall)];
})()";

/// Whether there is a browser to draw HTML with.
pub fn available() -> bool {
    crate::browser::available()
}

/// The browser kept between drawings, if one is.
struct Warm {
    cdp: Arc<Cdp>,
    child: std::process::Child,
    profile: std::path::PathBuf,
    last_used: Instant,
}

impl Warm {
    /// End the browser and remove its profile, on a thread of its own: both
    /// wait on the process.
    fn end(mut self) {
        let _ = std::thread::Builder::new()
            .name("illustrate-end".into())
            .spawn(move || launch::end(&mut self.child, &self.profile));
    }
}

static WARM: LazyLock<tokio::sync::Mutex<Option<Warm>>> = LazyLock::new(Default::default);

/// Draw `markup` `width` CSS pixels wide, and `height` tall or as tall as it
/// comes out.
pub async fn render(markup: &str, width: u32, height: Option<u32>) -> CommandResult<Rendered> {
    let mut warm = WARM.lock().await;
    if warm.as_ref().is_none_or(|w| w.cdp.is_closed()) {
        if let Some(gone) = warm.take() {
            gone.end();
        }
        *warm = Some(start().await?);
        reap_when_idle();
    }
    let markup = without_refresh(markup)?;
    let browser = warm.as_mut().expect("started above");
    let drawn = draw(&browser.cdp, &markup, width, height).await;
    browser.last_used = Instant::now();
    if drawn.is_err()
        && browser.cdp.is_closed()
        && let Some(gone) = warm.take()
    {
        gone.end();
    }
    // The browser's failure, not the markup's: said so, so that it is not
    // rewritten and sent again for a fault that is not in it.
    drawn.map_err(|e| {
        CommandError::new(codes::INTERNAL, format!("the browser could not draw that page: {e}"))
    })
}

async fn start() -> CommandResult<Warm> {
    let unavailable = |why: String| CommandError::new(codes::UNSUPPORTED, why);
    let executable = launch::executable()
        .ok_or_else(|| unavailable("no Chrome or Chromium was found on this computer".into()))?;
    let door = closed_door().map_err(unavailable)?;
    let launched = tokio::task::spawn_blocking(move || launch::launch(&executable, door))
        .await
        .map_err(|e| unavailable(e.to_string()))?
        .map_err(unavailable)?;
    let cdp = Cdp::over(launched.reader, launched.writer);
    let warm =
        Warm { cdp, child: launched.child, profile: launched.profile, last_used: Instant::now() };
    // The browser takes a moment to be ready to answer at all.
    let mut ready = Err(String::new());
    for _ in 0..50 {
        ready = warm.cdp.call(None, "Browser.getVersion", json!({})).await;
        if ready.is_ok() || warm.cdp.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if let Err(e) = ready {
        warm.end();
        return Err(unavailable(format!("the browser did not start: {e}")));
    }
    Ok(warm)
}

/// End the warm browser once nobody has drawn with it for [`IDLE_CLOSE`].
/// One watcher, on the browser module's own runtime, for as long as there
/// is a browser to watch.
fn reap_when_idle() {
    static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if WATCHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    crate::browser::background().spawn(async {
        loop {
            tokio::time::sleep(IDLE_CLOSE / 4).await;
            // Not while one is drawing: that is the opposite of idle.
            let Ok(mut warm) = WARM.try_lock() else { continue };
            match warm.as_ref() {
                Some(w) if w.last_used.elapsed() < IDLE_CLOSE && !w.cdp.is_closed() => {}
                Some(_) => {
                    if let Some(gone) = warm.take() {
                        gone.end();
                    }
                    WATCHING.store(false, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
                None => {
                    WATCHING.store(false, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
        }
    });
}

/// Draw one page in a tab of its own, and close the tab whatever happened.
async fn draw(
    cdp: &Cdp,
    markup: &str,
    width: u32,
    height: Option<u32>,
) -> Result<Rendered, String> {
    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await?;
    let target_id = target.get("targetId").and_then(Value::as_str).ok_or("no tab")?.to_string();
    let drawn = async {
        let attached = cdp
            .call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }))
            .await?;
        let tab = attached.get("sessionId").and_then(Value::as_str).ok_or("no tab")?;
        draw_in(cdp, tab, markup, width, height).await
    }
    .await;
    let _ = cdp.call(None, "Target.closeTarget", json!({ "targetId": target_id })).await;
    drawn
}

async fn draw_in(
    cdp: &Cdp,
    tab: &str,
    markup: &str,
    width: u32,
    height: Option<u32>,
) -> Result<Rendered, String> {
    let tab = Some(tab);
    let viewport = |w: u32, h: u32, scale: u32| json!({ "width": w, "height": h, "deviceScaleFactor": scale, "mobile": false });
    cdp.call(tab, "Page.enable", json!({})).await?;
    cdp.call(
        tab,
        "Emulation.setDeviceMetricsOverride",
        viewport(width, height.unwrap_or(FIRST_HEIGHT), SHARP),
    )
    .await?;
    let tree = cdp.call(tab, "Page.getFrameTree", json!({})).await?;
    let frame = tree.pointer("/frameTree/frame/id").and_then(Value::as_str).ok_or("no frame")?;
    cdp.call(tab, "Page.setDocumentContent", json!({ "frameId": frame, "html": document(markup) }))
        .await?;
    let measured = cdp
        .call(
            tab,
            "Runtime.evaluate",
            json!({ "expression": MEASURE, "awaitPromise": true, "returnByValue": true }),
        )
        .await?;
    let size = measured.pointer("/result/value").and_then(Value::as_array);
    let dimension = |i: usize| {
        size.and_then(|s| s.get(i))
            .and_then(Value::as_u64)
            .map(|n| n.min(u64::from(u32::MAX)) as u32)
    };
    let (Some(content_w), Some(content_h)) = (dimension(0), dimension(1)) else {
        return Err("the page could not be measured".into());
    };

    let mut notes = Vec::new();
    if content_w > width + 1 {
        notes.push(format!(
            "The page's content is {content_w}px wide, wider than the {width}px it was drawn at; \
             what is past the right edge is cut off."
        ));
    }
    let (w, h, scale) = fit(width, height.unwrap_or(content_h).max(1));
    if height.is_none() && h < content_h {
        notes.push(format!(
            "The page came out {content_h}px tall; only the first {h}px were drawn."
        ));
    }
    cdp.call(tab, "Emulation.setDeviceMetricsOverride", viewport(w, h, scale)).await?;
    let shot = cdp
        .call(
            tab,
            "Page.captureScreenshot",
            json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": false }),
        )
        .await?;
    let data = shot.get("data").and_then(Value::as_str).ok_or("no picture")?;
    let png = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?;
    Ok(Rendered { png, width: w * scale, height: h * scale, notes })
}

/// The CSS size to draw at and the scale to draw it at: sharp when that
/// fits within [`MAX_SIDE`] and [`MAX_PIXELS`], one to one when only that
/// does, and cut at the bottom when even that does not.
fn fit(width: u32, height: u32) -> (u32, u32, u32) {
    let fits = |w: u32, h: u32, s: u32| {
        w * s <= MAX_SIDE && h * s <= MAX_SIDE && u64::from(w * s) * u64::from(h * s) <= MAX_PIXELS
    };
    if fits(width, height, SHARP) {
        return (width, height, SHARP);
    }
    let by_area = (MAX_PIXELS / u64::from(width.max(1))).min(u64::from(MAX_SIDE)) as u32;
    (width, height.min(by_area).max(1), 1)
}

/// `markup` as a whole page, under the policy. A whole document the model
/// wrote itself is fine inside this one: a second `<html>` or `<body>` only
/// adds its attributes to the first, and its `<style>`s apply wherever they
/// are.
fn document(markup: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{POLICY}\">\
         </head><body>{markup}</body></html>"
    )
}

/// `markup` with `http-equiv` taken off every `<meta>`, read by an HTML
/// parser as the browser reads it -- so `&#114;efresh`, odd case and odd
/// quoting are all found. A refresh is the one way a page with no scripts
/// can leave itself: no policy stops it, and a page that has gone somewhere
/// else -- even nowhere, through the closed door -- cannot be drawn. Every
/// other `http-equiv` the model could write would only narrow the policy
/// that comes before its markup, so losing them loses nothing.
fn without_refresh(markup: &str) -> CommandResult<String> {
    use lol_html::{RewriteStrSettings, element, rewrite_str};
    let settings =
        RewriteStrSettings::new().append_element_content_handler(element!("meta", |el| {
            el.remove_attribute("http-equiv");
            Ok(())
        }));
    rewrite_str(markup, settings)
        .map_err(|e| CommandError::new(codes::INVALID, format!("that page could not be read: {e}")))
}

/// A proxy address that goes nowhere: a listener on this machine's loopback
/// that closes every connection the moment it is made. Bound once, kept for
/// the life of the process. See the module doc.
fn closed_door() -> Result<SocketAddr, String> {
    static DOOR: OnceLock<Result<SocketAddr, String>> = OnceLock::new();
    DOOR.get_or_init(|| {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("illustrate-door".into())
            .spawn(move || {
                for connection in listener.incoming() {
                    drop(connection);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(address)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_drawn_sharp_until_that_would_be_too_big() {
        assert_eq!(fit(800, 600), (800, 600, 2));
        assert_eq!(fit(800, 2048), (800, 2048, 2));
        // Past the side at two to one: drawn one to one instead.
        assert_eq!(fit(800, 3000), (800, 3000, 1));
        // Past even that: cut at the bottom.
        assert_eq!(fit(800, 9000), (800, MAX_SIDE, 1));
    }

    #[test]
    fn the_policy_comes_before_anything_the_model_wrote() {
        let page = document("<img src=\"https://example.com/x.png\">");
        let policy = page.find("Content-Security-Policy").unwrap();
        let theirs = page.find("<img").unwrap();
        assert!(policy < theirs);
        assert!(POLICY.contains("script-src 'none'"));
        assert!(POLICY.contains("img-src data:"));
    }

    #[test]
    fn a_page_cannot_send_itself_elsewhere() {
        for written in [
            "<META HTTP-EQUIV = 'Refresh' content=\"0;url=x\"><p>",
            "<meta http-equiv=refresh content=0>",
            // A character reference is the same word to the browser.
            "<meta http-equiv=\"&#114;efresh\" content=\"0;url=x\">",
            "<meta http-equiv=\"&#x72;efresh\" content=\"0\">",
        ] {
            let out = without_refresh(written).unwrap();
            assert!(!out.to_ascii_lowercase().contains("http-equiv"), "{out}");
        }
        assert_eq!(
            without_refresh("<p>plain http-equiv words</p>").unwrap(),
            "<p>plain http-equiv words</p>"
        );
    }

    #[test]
    fn the_door_closes_every_connection() {
        use std::io::Read;
        let door = closed_door().unwrap();
        let mut stream = std::net::TcpStream::connect(door).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 16];
        // Closed from the far side: end of stream, or a reset.
        assert!(matches!(stream.read(&mut buf), Ok(0) | Err(_)));
    }

    /// A real page in a real browser, when this computer has one: drawn, at
    /// twice the size, the colour it was painted, and with a picture from the
    /// web left undrawn.
    #[tokio::test]
    async fn a_page_comes_out_as_written_and_reaches_nothing() {
        if !available() {
            eprintln!("no Chrome or Chromium here; skipping");
            return;
        }
        let page = "<style>body{margin:0;background:#00ff00}</style>\
                    <div style=\"height:150px\"></div>\
                    <img src=\"https://example.com/never.png\" width=\"10\" height=\"10\">";
        let out = render(page, 300, None).await.unwrap();
        let img = image::load_from_memory(&out.png).unwrap().to_rgba8();
        assert_eq!((out.width, out.height), (img.width(), img.height()));
        assert_eq!(img.width(), 600);
        assert!(img.height() >= 300, "{}", img.height());
        assert_eq!(img.get_pixel(10, 10).0, [0, 255, 0, 255]);
    }

    /// A lazy picture far below the first screen is loaded, not waited on
    /// until the protocol gives up.
    #[tokio::test]
    async fn a_lazy_picture_below_the_fold_does_not_stall_the_drawing() {
        if !available() {
            eprintln!("no Chrome or Chromium here; skipping");
            return;
        }
        let dot = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let page = format!(
            "<div style=\"height:1500px\"></div>\
             <img loading=\"lazy\" src=\"data:image/png;base64,{dot}\" width=\"40\" height=\"40\">"
        );
        let started = std::time::Instant::now();
        let out = render(&page, 300, None).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(out.height >= 1500, "{}", out.height);
    }

    /// Everything a page could use to reach out -- a picture, a stylesheet,
    /// a font, a frame, a script, a redirect -- aimed at a server on this
    /// machine, which must hear nothing.
    #[tokio::test]
    async fn a_page_cannot_reach_a_server_on_this_computer() {
        if !available() {
            eprintln!("no Chrome or Chromium here; skipping");
            return;
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let at = format!("http://{}", listener.local_addr().unwrap());
        let page = format!(
            "<link rel=\"stylesheet\" href=\"{at}/s.css\">\
             <style>@font-face{{font-family:x;src:url({at}/f.woff2)}}\
             body{{font-family:x;background:url({at}/bg.png)}}</style>\
             <img src=\"{at}/i.png\"><iframe src=\"{at}/frame\"></iframe>\
             <script src=\"{at}/s.js\"></script>\
             <meta http-equiv=\"refresh\" content=\"0;url={at}/away\">\
             <meta http-equiv=\"&#114;efresh\" content=\"0;url={at}/sly\">\
             <p>hello</p>"
        );
        render(&page, 400, Some(300)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "the page reached the server"
        );
    }
}
