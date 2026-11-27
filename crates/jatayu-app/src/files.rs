//! The files catalog's desktop half: the pickers, opening a file in another
//! program, saving copies of files, and the chunks of an upload.
//!
//! What the vault does with files -- the catalog, the folder, the reconciler --
//! is [`jatayu_service`]'s, reached through the same `call` every other
//! command goes through, and the bytes of a file are `protocol.rs`'s
//! (`jatayu://files/file/{id}`). What is here is only what needs this
//! machine: a path somebody just picked, a program to open a file with, a
//! folder to save copies in. They are the shell's own commands, by name, and
//! for the same reason `open_vault` is -- a path is a fact about *this* machine,
//! so a vault on another one must never be the one to name it.
//!
//! # Nothing large goes through the webview as one buffer
//!
//! A window on the vault's own machine imports by path: the picker answers
//! paths and the service copies from where the files are. A window looking at a
//! vault on another machine has no such shortcut -- its paths mean nothing
//! there -- so each picked file is uploaded to that machine in [`CHUNK`]-sized
//! pieces, read off the disk one at a time. The same chunks arrive from the
//! interface itself for a paste or a browser's drop, which has bytes and no path
//! ([`upload_chunk`]).
//!
//! # Local only, and why
//!
//! Opening a file in another program and showing it in a file manager are
//! things done to a file on *this* disk. A vault on another machine has no such
//! file here, and what it would be asked to open there is its own business, so
//! both refuse rather than guess.

use jatayu_core::files::{FileEntry, FileOrigin, UploadTicket};
use jatayu_core::id::FileId;
use jatayu_service::ctx::Ctx;
use jatayu_service::error::{CommandError, CommandResult, codes};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::remote::{Remote, Session};
use crate::state::{AppState, SessionHandle};

/// How much of a file goes in one piece of an upload, and comes down in one
/// piece of a copy. Four megabytes: few enough round trips that a film is not
/// a thousand of them, small enough that one is a fraction of a second on a
/// tailnet and the memory held is nothing. Under what a response may be
/// ([`jatayu_core::media::MAX_CHUNK`]) so a download is never answered
/// short.
const CHUNK: usize = 4 * 1024 * 1024;

/// Where a file the person is adding should go, as the interface asked.
/// All of it optional: the service has a folder and a sealing rule for every
/// origin, and these only say otherwise.
struct Placement {
    /// Relative to the files folder; `""` is the top.
    dir: Option<String>,
    origin: Option<FileOrigin>,
    encrypted: Option<bool>,
}

// ---- adding ------------------------------------------------------------------

/// Ask the system's picker for files, and add them. Answers `[]` when the
/// picker was dismissed, which is not an error and must not be drawn as one.
///
/// On the vault's own machine the paths go to the service's `import_files`,
/// which copies from where the files are. Anywhere else each is uploaded.
#[tauri::command]
pub async fn pick_and_import_files(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    dir: Option<String>,
    origin: Option<FileOrigin>,
    encrypted: Option<bool>,
) -> CommandResult<Vec<FileEntry>> {
    let paths = ask_files(&app).await.unwrap_or_default();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let ctx = crate::windows::ctx_for(webview.label());
    let placement = Placement { dir, origin, encrypted };
    let session = state.session().as_session();
    match &session {
        Session::Local(_) => {
            let paths: Vec<String> =
                paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            let args = json!({
                "paths": paths,
                "dir": placement.dir,
                "origin": placement.origin,
                "encrypted": placement.encrypted,
            });
            from_value(session.call(ctx, "import_files", args).await?)
        }
        Session::Remote(_) => {
            let mut added = Vec::with_capacity(paths.len());
            for path in &paths {
                added.push(upload_path(&session, &ctx, path, &placement).await?);
            }
            Ok(added)
        }
    }
}

/// Upload one file from this machine to the vault the session is looking at:
/// `begin_upload`, the chunks, `finish_upload`. A chunk that fails cancels the
/// upload, so what was staged for it does not sit on the other machine's disk
/// until something sweeps it.
async fn upload_path(
    session: &Session,
    ctx: &Ctx,
    path: &Path,
    placement: &Placement,
) -> CommandResult<FileEntry> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| CommandError::new(codes::INVALID, "that is not a file"))?;
    let file = std::fs::File::open(path).map_err(|e| io(path, e))?;
    let metadata = file.metadata().map_err(|e| io(path, e))?;
    if !metadata.is_file() {
        return Err(CommandError::new(codes::INVALID, format!("{name} is not a file")));
    }

    let begun = session
        .call(
            ctx.clone(),
            "begin_upload",
            json!({
                "name": name,
                "size": metadata.len(),
                "dir": placement.dir,
                "origin": placement.origin,
                "encrypted": placement.encrypted,
            }),
        )
        .await?;
    let ticket: UploadTicket = from_value(begun)?;

    if let Err(e) = send_chunks(session, ctx, &ticket.upload, file, path).await {
        let cancel = json!({ "upload": ticket.upload });
        let _ = session.call(ctx.clone(), "cancel_upload", cancel).await;
        return Err(e);
    }
    let finished = session.call(ctx.clone(), "finish_upload", json!({ "upload": ticket.upload }));
    from_value(finished.await?)
}

/// Read `file` a chunk at a time and send each where the upload has got to.
/// What the vault answers is the bytes it holds so far, which has to be exactly
/// what was sent so far: anything else is a chunk that did not land, and
/// carrying on would build a file with a hole in it.
async fn send_chunks(
    session: &Session,
    ctx: &Ctx,
    upload: &str,
    mut file: std::fs::File,
    path: &Path,
) -> CommandResult<()> {
    let mut offset = 0u64;
    loop {
        let (back, bytes) = read_chunk(file, path).await?;
        file = back;
        if bytes.is_empty() {
            return Ok(());
        }
        let at = offset;
        offset += bytes.len() as u64;
        let received = session.upload_chunk(ctx.clone(), upload, at, bytes).await?;
        if received != offset {
            return Err(CommandError::new(
                codes::INTERNAL,
                format!("the vault has {received} bytes of that upload; {offset} were sent"),
            ));
        }
    }
}

/// The next chunk of `file`, off the async runtime's threads, with the handle
/// handed back for the one after. Empty at the end of the file.
async fn read_chunk(
    mut file: std::fs::File,
    path: &Path,
) -> CommandResult<(std::fs::File, Vec<u8>)> {
    let path = path.to_path_buf();
    jatayu_service::blocking(move || {
        let mut buffer = vec![0u8; CHUNK];
        let read = fill(&mut file, &mut buffer).map_err(|e| io(&path, e))?;
        buffer.truncate(read);
        Ok((file, buffer))
    })
    .await
}

/// One chunk of an upload, from the interface: raw bytes, with the upload and
/// where in it this goes in the headers `api.uploadChunk` sets. A paste, a
/// browser's drop, or any file that came as bytes and not as a path.
///
/// Raw for the reason `put_blob` is: a `Uint8Array` inside a JSON argument
/// becomes an array of numbers, and a chunk of four million of them is the
/// window stopped for as long as that takes. Answers the bytes the vault has
/// received so far.
#[tauri::command]
pub async fn upload_chunk(
    webview: tauri::Webview,
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CommandResult<u64> {
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        // The slow path, as `put_blob` keeps it: Tauri falls back to
        // `postMessage` when a webview blocks its custom protocol, and there a
        // buffer arrives as an array of numbers.
        tauri::ipc::InvokeBody::Json(value) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map_err(|_| CommandError::new(codes::INVALID, "upload payload was not a buffer"))?,
    };
    let upload = header(&request, "x-jatayu-upload")?;
    let offset = offset_from(&header(&request, "x-jatayu-offset")?)?;
    let ctx = crate::windows::ctx_for(webview.label());
    state.session().as_session().upload_chunk(ctx, &upload, offset, bytes).await
}

fn header(request: &tauri::ipc::Request<'_>, name: &str) -> CommandResult<String> {
    request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string).ok_or_else(|| {
        CommandError::new(codes::INVALID, format!("an upload chunk needs a {name} header"))
    })
}

fn offset_from(text: &str) -> CommandResult<u64> {
    text.trim()
        .parse()
        .map_err(|_| CommandError::new(codes::INVALID, "an upload chunk's offset was not a number"))
}

// ---- the folder ---------------------------------------------------------------

/// Ask for a folder, for Settings → Files. Answers the path, or `None` when the
/// dialog was dismissed.
///
/// Only a path crosses back, and only one the person just picked: whether it
/// will do is the service's question, asked when it is set up.
#[tauri::command]
pub async fn pick_files_folder(app: tauri::AppHandle) -> CommandResult<Option<String>> {
    Ok(ask_folder(&app, "Choose the files folder").await.map(|p| p.to_string_lossy().into_owned()))
}

// ---- opening and showing ------------------------------------------------------

/// Open a file in the program the system uses for it. A sealed file is
/// decrypted into a private folder outside the files folder first (the vault's
/// `open_file_copy`), which is why this is not left to the interface to do with
/// a path.
///
/// Refuses what would *run* rather than be shown -- see [`runs_a_program`].
#[tauri::command]
pub async fn open_file_externally(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: FileId,
) -> CommandResult<()> {
    open_in_program(app, here(&state)?, id).await
}

/// [`open_file_externally`]'s work, for whoever has the service in hand
/// rather than a window's state: the assistant opening a file for somebody
/// at this computer (`crate::desk`) is held to exactly the refusal and the
/// sealed-file copy Files' own "Open" has, because it is the same code.
pub(crate) async fn open_in_program(
    app: tauri::AppHandle,
    service: std::sync::Arc<jatayu_service::Service>,
    id: FileId,
) -> CommandResult<()> {
    let vault = service.require_unlocked()?;
    let path = jatayu_service::blocking(move || {
        let entry = vault.file(id)?;
        if runs_a_program(&entry.name) {
            return Err(CommandError::new(
                codes::FORBIDDEN,
                format!(
                    "{} is a program, script or web page that can run code when it is opened, \
                     and Jatayu will not open one for you. Show it in its folder to open it \
                     yourself.",
                    entry.name
                ),
            ));
        }
        Ok(vault.open_file_copy(id)?)
    })
    .await?;
    // A process to start, and on some systems a round trip to a service:
    // neither is for the thread the window is running on.
    jatayu_service::blocking(move || {
        app.opener()
            .open_path(path.to_string_lossy().into_owned(), None::<&str>)
            .map_err(|e| CommandError::new(codes::IO, e.to_string()))
    })
    .await
}

/// Show a plain file in the system's file manager. A sealed file has no place
/// on this disk to show.
#[tauri::command]
pub async fn reveal_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: FileId,
) -> CommandResult<()> {
    let vault = here(&state)?.require_unlocked()?;
    let path = jatayu_service::blocking(move || Ok(vault.plain_path(id)?)).await?;
    // On Linux this is a call to the desktop's file manager over D-Bus, made
    // with zbus's own blocking runtime, which panics when it is started from
    // inside an async task -- hence a thread of its own, as `notice.rs` has to
    // for the same reason. Where no file manager answers, the folder the file
    // is in is opened instead, which is nearly as good.
    jatayu_service::blocking(move || {
        let Err(first) = app.opener().reveal_item_in_dir(&path) else { return Ok(()) };
        tracing::debug!(error = %first, "could not reveal a file; opening its folder instead");
        let folder = path.parent().unwrap_or(&path);
        app.opener()
            .open_path(folder.to_string_lossy().into_owned(), None::<&str>)
            .map_err(|e| CommandError::new(codes::IO, e.to_string()))
    })
    .await
}

/// The service, if this window's vault is in this process; an error that says
/// what to do instead if it is on another machine.
fn here(state: &AppState) -> CommandResult<std::sync::Arc<jatayu_service::Service>> {
    match state.session() {
        SessionHandle::Local(service) => Ok(service),
        SessionHandle::Remote(_) => Err(CommandError::new(
            codes::UNSUPPORTED,
            "That file is on another computer. Save a copy of it to open it here.",
        )),
    }
}

/// Whether opening a file by name would start a program rather than show a
/// document: the kinds Windows runs when they are opened, the scripts and
/// shortcuts every desktop will, and installers. The files catalog holds what
/// strangers sent -- a mail attachment, a picture's cousin from a chat -- and
/// "open" on one of these is the oldest way there is to run something somebody
/// else chose. Showing it in its folder is still there for the person who means
/// it.
///
/// Also the kinds that are documents in name only. A web page, an SVG and an
/// MHT archive are opened by the browser, which runs their scripts from a
/// `file://` page that can read other local files; a macro-enabled Office
/// file, a compiled help file and a disk image ask to be run or mounted; a
/// `.url`, `.webloc` or `.lnk` is a shortcut to somewhere else. A list of
/// what is dangerous is never complete, so this one is only as long as what
/// has been seen to be.
fn runs_a_program(name: &str) -> bool {
    const PROGRAMS: &[&str] = &[
        // Run by Windows, or by any desktop.
        "exe",
        "com",
        "bat",
        "cmd",
        "msi",
        "msp",
        "scr",
        "pif",
        "lnk",
        "url",
        "cpl",
        "reg",
        "ps1",
        "psm1",
        "psd1",
        "ps1xml",
        "vbs",
        "vbe",
        "js",
        "jse",
        "mjs",
        "cjs",
        "wsf",
        "wsh",
        "wsc",
        "hta",
        "jar",
        "jnlp",
        "app",
        "command",
        "tool",
        "sh",
        "bash",
        "zsh",
        "ksh",
        "csh",
        "fish",
        "run",
        "desktop",
        "appimage",
        "deb",
        "rpm",
        "pkg",
        "dmg",
        "apk",
        "scpt",
        "workflow",
        "action",
        "inf",
        "gadget",
        "application",
        "appref-ms",
        "diagcab",
        "webloc",
        "terminal",
        // Scripts for the languages an interpreter is usually installed for.
        "py",
        "pyw",
        "pyz",
        "pyc",
        "pl",
        "pm",
        "rb",
        "php",
        "phtml",
        "lua",
        "tcl",
        // Pages and images a browser runs scripts in.
        "html",
        "htm",
        "xhtml",
        "xht",
        "shtml",
        "mht",
        "mhtml",
        "svg",
        "svgz",
        // Documents that carry macros, and what asks to be mounted or run.
        "docm",
        "dotm",
        "xlsm",
        "xltm",
        "xlam",
        "xlsb",
        "pptm",
        "potm",
        "ppam",
        "ppsm",
        "sldm",
        "chm",
        "iqy",
        "slk",
        "iso",
        "img",
        "vhd",
        "vhdx",
    ];
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PROGRAMS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

// ---- saving copies --------------------------------------------------------------

/// Ask where to save copies of these files, and save them there. Answers how
/// many were saved; `0` when the dialog was dismissed.
///
/// Each goes under its own name, or a free one beside it (`report (2).pdf`):
/// never over anything already there. A sealed file is decrypted on the way. A
/// vault on another machine is read by ranges and written here as it arrives,
/// so a film costs a chunk of memory and not the film.
///
/// A file that cannot be saved does not stop the others; what happened is said
/// at the end, with how many did make it.
#[tauri::command]
pub async fn export_files(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    state: State<'_, AppState>,
    ids: Vec<FileId>,
) -> CommandResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let Some(dir) = ask_folder(&app, "Save copies to").await else { return Ok(0) };
    let ctx = crate::windows::ctx_for(webview.label());
    let handle = state.session();

    let mut saved = 0u64;
    let mut first_failure: Option<CommandError> = None;
    for id in &ids {
        match export_one(&handle, &ctx, *id, &dir).await {
            Ok(()) => saved += 1,
            Err(e) => {
                tracing::warn!(%id, error = %e, "could not save a copy of a file");
                if first_failure.is_none() {
                    first_failure = Some(e);
                }
            }
        }
    }
    match first_failure {
        None => Ok(saved),
        Some(e) if saved == 0 => Err(e),
        Some(e) => Err(CommandError::new(
            e.code.as_str(),
            format!("Saved {saved} of {} files. {}", ids.len(), e.message),
        )),
    }
}

async fn export_one(
    handle: &SessionHandle,
    ctx: &Ctx,
    id: FileId,
    dir: &Path,
) -> CommandResult<()> {
    match handle {
        SessionHandle::Local(service) => {
            let vault = service.require_unlocked()?;
            let dir = dir.to_path_buf();
            jatayu_service::blocking(move || {
                let entry = vault.file(id)?;
                vault.export_file(id, &free_name(&dir, &entry.name))?;
                Ok(())
            })
            .await
        }
        SessionHandle::Remote(remote) => {
            let session = handle.as_session();
            let detail = session.call(ctx.clone(), "get_file", json!({ "id": id })).await?;
            let name = detail.pointer("/file/name").and_then(Value::as_str).unwrap_or("file");
            download(remote, id, dir, name).await
        }
    }
}

/// Copy a file from the vault on the other end of `remote` into `dir`, a range
/// at a time, by way of a neighbouring file that is renamed once it is whole, so
/// a connection lost half way cannot leave half a film under the film's name.
async fn download(remote: &Remote, id: FileId, dir: &Path, name: &str) -> CommandResult<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    // `create_new`: whatever is already called this is somebody's, not ours.
    let temp = dir.join(format!(".jatayu-{}-{n}.part", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| io(&temp, e))?;

    let copied = copy_down(remote, &id.to_string(), &mut file, &temp).await;
    let synced = copied.and_then(|()| file.sync_all().map_err(|e| io(&temp, e)));
    drop(file);
    let finished = synced.and_then(|()| {
        // The name is chosen now, not when the copy began: another file may
        // have taken it in the time a film takes to arrive.
        let dest = free_name(dir, name);
        std::fs::rename(&temp, &dest).map_err(|e| io(&dest, e))
    });
    if finished.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    finished
}

async fn copy_down(
    remote: &Remote,
    id: &str,
    file: &mut std::fs::File,
    temp: &Path,
) -> CommandResult<()> {
    let mut start = 0u64;
    loop {
        let range = format!("bytes={start}-{}", start + CHUNK as u64 - 1);
        let served = remote.client.file_range(id, Some(&range)).await?;
        // A part that begins somewhere else means the file was replaced
        // between two requests: carrying on would join two files.
        if served.start != start {
            return Err(CommandError::new(
                codes::IO,
                "that file changed while it was being copied",
            ));
        }
        file.write_all(&served.bytes).map_err(|e| io(temp, e))?;
        start += served.bytes.len() as u64;
        if start >= served.total {
            return Ok(());
        }
        if served.bytes.is_empty() {
            // Neither done nor making progress. Better to stop with an error
            // than to ask for the same nothing for ever.
            return Err(CommandError::new(codes::IO, "that file stopped part way through"));
        }
    }
}

/// A file name a copy may be saved under: the last part of what the catalog
/// calls it, with nothing in it that could reach into another folder or
/// confuse a file system. The catalog's names come from the folder or from a
/// sender, and this is where they meet somebody else's disk.
fn export_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control())
        // Windows refuses these in a name; a copy of a file from somewhere
        // that does not should still be savable there.
        .map(|c| if cfg!(windows) && "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim();
    if matches!(clean, "" | "." | "..") { "file".to_string() } else { clean.to_string() }
}

/// `name` in `dir` if nothing is there, else `name (2)`, `name (3)` and so on
/// before the extension: never a path that is already taken, so saving a copy
/// cannot replace anything.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let name = export_name(name);
    let taken = |path: &Path| std::fs::symlink_metadata(path).is_ok();
    let first = dir.join(&name);
    if !taken(&first) {
        return first;
    }
    let path = Path::new(&name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&name);
    let extension = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}"));
    (2..=u32::MAX)
        .map(|n| dir.join(format!("{stem} ({n}){}", extension.as_deref().unwrap_or(""))))
        .find(|candidate| !taken(candidate))
        .unwrap_or(first)
}

// ---- the dialogs and the odds and ends --------------------------------------------

async fn ask_files(app: &tauri::AppHandle) -> Option<Vec<PathBuf>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Add files").pick_files(move |paths| {
        let _ = tx.send(paths);
    });
    rx.await
        .ok()
        .flatten()
        .map(|paths| paths.into_iter().filter_map(|p| p.into_path().ok()).collect())
}

async fn ask_folder(app: &tauri::AppHandle, title: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title(title).pick_folder(move |path| {
        let _ = tx.send(path);
    });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// Read until the buffer is full or the file ends.
///
/// `Read::read` may return fewer bytes than asked for at any time, and a chunk
/// that treated a short read as the end of the file would upload a truncated one
/// and report it whole.
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

fn from_value<T: serde::de::DeserializeOwned>(value: Value) -> CommandResult<T> {
    serde_json::from_value(value).map_err(|e| {
        CommandError::new(codes::INTERNAL, format!("the vault's answer was not understood: {e}"))
    })
}

fn io(path: &Path, e: std::io::Error) -> CommandError {
    CommandError::new(codes::IO, format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_never_takes_a_name_that_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(free_name(dir.path(), "report.pdf"), dir.path().join("report.pdf"));

        std::fs::write(dir.path().join("report.pdf"), b"somebody's own").unwrap();
        assert_eq!(free_name(dir.path(), "report.pdf"), dir.path().join("report (2).pdf"));

        std::fs::write(dir.path().join("report (2).pdf"), b"and another").unwrap();
        assert_eq!(free_name(dir.path(), "report.pdf"), dir.path().join("report (3).pdf"));

        // The extension is the last one, and a name with none is still numbered.
        std::fs::write(dir.path().join("a.tar.gz"), b"x").unwrap();
        std::fs::write(dir.path().join("notes"), b"x").unwrap();
        assert_eq!(free_name(dir.path(), "a.tar.gz"), dir.path().join("a.tar (2).gz"));
        assert_eq!(free_name(dir.path(), "notes"), dir.path().join("notes (2)"));
    }

    #[test]
    fn a_name_from_the_catalog_cannot_reach_into_another_folder() {
        for (given, want) in [
            ("invoice.pdf", "invoice.pdf"),
            ("../../.config/autostart/x.desktop", "x.desktop"),
            ("..\\..\\evil.exe", "evil.exe"),
            ("/etc/passwd", "passwd"),
            (".bashrc", ".bashrc"),
            ("a\u{0}b\n.txt", "ab.txt"),
            ("  spaced.txt  ", "spaced.txt"),
            ("", "file"),
            ("..", "file"),
            ("folder/", "file"),
        ] {
            assert_eq!(export_name(given), want, "{given:?}");
        }
    }

    #[test]
    fn opening_a_program_is_refused_and_opening_a_document_is_not() {
        for program in [
            "setup.exe",
            "SETUP.EXE",
            "run.sh",
            "x.desktop",
            "a.b.bat",
            "link.lnk",
            // What a browser or an interpreter runs, and what carries macros.
            "page.html",
            "PAGE.HTM",
            "a.xhtml",
            "logo.svg",
            "saved.mht",
            "saved.mhtml",
            "tool.py",
            "tool.pyw",
            "tool.pl",
            "tool.rb",
            "index.php",
            "app.js",
            "app.mjs",
            "run.vbs",
            "run.ps1",
            "budget.docm",
            "budget.xlsm",
            "deck.pptm",
            "help.chm",
            "disk.iso",
            "disk.img",
            "app.jnlp",
            "x.desktop",
            "x.AppImage",
            "bundle.jar",
            "site.url",
            "site.webloc",
            "a.lnk",
            "x.scr",
            "x.msi",
            "x.hta",
            "x.reg",
            "x.command",
            "x.tool",
        ] {
            assert!(runs_a_program(program), "{program}");
        }
        for document in [
            "photo.jpg",
            "film.mov",
            "paper.pdf",
            "a.tar.gz",
            "notes",
            "x.jsonl",
            "n.txt",
            "sheet.xlsx",
            "memo.docx",
            "deck.pptx",
            "song.mp3",
            "photo.png",
            "data.json",
            "page.html.txt",
            "export.xml",
        ] {
            assert!(!runs_a_program(document), "{document}");
        }
    }

    #[test]
    fn an_upload_chunks_offset_is_a_number() {
        assert_eq!(offset_from("0").unwrap(), 0);
        assert_eq!(offset_from(" 4194304 ").unwrap(), 4_194_304);
        for bad in ["", "-1", "soon", "1.5"] {
            assert_eq!(offset_from(bad).unwrap_err().code, codes::INVALID, "{bad:?}");
        }
    }

    /// A file is read in whole chunks and then nothing -- never a short chunk
    /// in the middle, which an upload would take for the end.
    #[tokio::test]
    async fn a_file_is_read_in_whole_chunks_and_then_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("film.mov");
        let size = CHUNK * 2 + 123;
        std::fs::write(&path, vec![7u8; size]).unwrap();

        let mut file = std::fs::File::open(&path).unwrap();
        let mut lengths = Vec::new();
        loop {
            let (back, bytes) = read_chunk(file, &path).await.unwrap();
            file = back;
            if bytes.is_empty() {
                break;
            }
            lengths.push(bytes.len());
        }
        assert_eq!(lengths, [CHUNK, CHUNK, 123]);
    }

    #[test]
    fn a_chunk_is_under_what_a_response_may_be() {
        assert!((CHUNK as u64) < jatayu_core::media::MAX_CHUNK);
    }
}
