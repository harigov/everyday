//! The picture on the system clipboard, for a paste the webview did not
//! hand over.
//!
//! Pasting a screenshot into the journal, a note, a message or the assistant
//! is the editors' `paste` handler reading `clipboardData.files`. WebKitGTK --
//! the webview Tauri uses on Linux -- does not fill that in. Measured against
//! WebKitGTK 2.52 under Xvfb, a clipboard holding only `image/png` (what a
//! screenshot tool leaves) reaches the page as a paste event with no types,
//! no files and no items at all; one holding `text/html` of an `<img>` beside
//! the picture (what a browser's "Copy image" leaves) reaches it as that HTML
//! and nothing else, which a rich text schema with no plain image node then
//! drops. The page has nothing to look at, so it asks the shell: the shell
//! may read the clipboard through GTK, which has no such limit.
//!
//! The page asks only *after* a real paste event that carried no files and
//! looked as though it might have been a picture
//! (`ui/src/lib/clipboard-picture.ts`), and the shell answers only a window
//! that has the keyboard -- the one somebody just pressed paste in. A window
//! in the background, or a script that asks on a timer from one, is told
//! there is no picture.
//!
//! The picture is handed over as the program that copied it offered it --
//! PNG first, then JPEG, WebP or GIF -- and never decoded here: decoding is
//! the main thread's time, and a clipboard can hold a picture of any size in
//! any format. The page tells the formats apart by their first bytes.
//!
//! The other desktops do not need this. The WebKit on macOS and WebView2 on
//! Windows hand a pasted picture over as a file, and so does a browser, so
//! there the answer is always "no picture" and the page's own path runs.

use karmayoga_service::error::CommandResult;
#[cfg(target_os = "linux")]
use karmayoga_service::error::{CommandError, codes};
use tauri::AppHandle;

/// The largest picture this will hand back. A screenshot of even a large
/// display is a few megabytes; this is for the clipboard that holds something
/// absurd, which would otherwise be copied out of GTK, across the IPC bridge
/// and into a webview that then has to store and draw it, to no one's
/// benefit.
#[cfg(target_os = "linux")]
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// How long to wait for the program that owns the clipboard to hand its
/// picture over. A well-behaved owner answers in milliseconds; one that has
/// stopped responding (a hung browser, say) never does, and the person
/// pressing paste should get their editor back rather than wait on it.
#[cfg(target_os = "linux")]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The picture formats taken, most wanted first, each with the bytes a file
/// of it starts with. PNG first because it is what screenshot tools and
/// browsers offer and loses nothing; the rest are what a webview draws
/// without help. A WebP's signature is `RIFF`, four bytes of length, then
/// `WEBP`, so only its first four are checked here; the page checks the rest.
#[cfg(target_os = "linux")]
const FORMATS: [(&str, &[u8]); 4] = [
    ("image/png", b"\x89PNG\r\n\x1a\n"),
    ("image/jpeg", b"\xff\xd8\xff"),
    ("image/webp", b"RIFF"),
    ("image/gif", b"GIF8"),
];

/// The picture on the system clipboard, as the bytes of the file it was
/// offered as, or no bytes at all when the clipboard holds no picture, the
/// asking window does not have the keyboard, or this desktop's webview hands
/// pastes over itself.
///
/// Raw bytes rather than JSON, for the reason `put_blob` takes them: a
/// `Vec<u8>` serialised as JSON is an array of numbers, and a screenshot is a
/// few million of them. A `tauri::ipc::Response` is sent as
/// `application/octet-stream` and arrives in the page as an `ArrayBuffer`.
///
/// An error only when there *was* a picture and it could not be had -- too
/// large, or the owner of the clipboard did not answer in time -- so the page
/// can tell the person why nothing appeared.
#[tauri::command]
pub async fn clipboard_image(
    app: AppHandle,
    webview: tauri::Webview,
) -> CommandResult<tauri::ipc::Response> {
    // Pressing paste is done in the window that has the keyboard, so any
    // other window asking is not pasting.
    if !webview.window().is_focused().unwrap_or(false) {
        return Ok(tauri::ipc::Response::new(Vec::new()));
    }
    read(&app).await.map(tauri::ipc::Response::new)
}

#[cfg(not(target_os = "linux"))]
async fn read(_app: &AppHandle) -> CommandResult<Vec<u8>> {
    Ok(Vec::new())
}

/// The clipboard's picture, through GTK, on either display server.
///
/// GTK may only be touched from the main thread, so the requests are made
/// there; each answer comes back through GTK's own callback, which also runs
/// on the main thread once the owner of the clipboard has replied. Nothing
/// here waits *on* the main thread (`wait_for_*` would run a nested main loop
/// inside whatever the window is doing), and nothing is decoded on it.
///
/// What the owner offers is asked first (the `TARGETS` list), and only a
/// format on that list is asked for: GTK requests a format without checking
/// it is offered, and not every owner refuses one it does not have -- xclip,
/// for one, answers any request with whatever it holds, so a clipboard of
/// text came back as "image/png" bytes that were words. The bytes must also
/// start the way a file of that format does.
#[cfg(target_os = "linux")]
async fn read(app: &AppHandle) -> CommandResult<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<CommandResult<Vec<u8>>>();
    app.run_on_main_thread(move || {
        let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
        let targets = gtk::gdk::Atom::intern("TARGETS");
        clipboard.request_contents(&targets, move |clipboard, offered| {
            let offered: Vec<String> = offered
                .targets()
                .unwrap_or_default()
                .into_iter()
                .map(|atom| atom.name().to_string())
                .collect();
            let Some(&(format, signature)) =
                FORMATS.iter().find(|(format, _)| offered.iter().any(|o| o == format))
            else {
                let _ = tx.send(Ok(Vec::new()));
                return;
            };
            // The callback is `FnOnce`, so `tx` goes on to this request and
            // nowhere else.
            let atom = gtk::gdk::Atom::intern(format);
            clipboard.request_contents(&atom, move |_, data| {
                // -1 when the owner went away meanwhile, 0 when it offered
                // the format empty.
                let length = data.length();
                let _ = tx.send(if length <= 0 {
                    Ok(Vec::new())
                } else if length as usize > MAX_BYTES {
                    Err(too_large(length as usize))
                } else {
                    let bytes = data.data();
                    Ok(if bytes.starts_with(signature) { bytes } else { Vec::new() })
                });
            });
        });
    })
    .map_err(|e| CommandError::new(codes::INTERNAL, e.to_string()))?;

    tokio::time::timeout(TIMEOUT, rx)
        .await
        .map_err(|_| {
            CommandError::new(
                codes::TIMED_OUT,
                "the program that copied the picture did not hand it over in time",
            )
        })?
        // The sender is dropped unused only if the main thread dropped the
        // closure without running it, which is the application closing.
        .map_err(|_| CommandError::new(codes::INTERNAL, "the main thread went away"))?
}

#[cfg(target_os = "linux")]
fn too_large(bytes: usize) -> CommandError {
    CommandError::new(
        codes::TOO_LARGE,
        format!(
            "the picture on the clipboard is {} MB, which is too large to paste",
            bytes / (1024 * 1024)
        ),
    )
}
