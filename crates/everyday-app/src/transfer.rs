//! The two ends of a transfer that only a desktop has: a save dialog and a
//! file picker.
//!
//! The archive itself is [`everyday_transfer`]'s, and moving it across the
//! boundary is `domains::transfer`'s. What is left is the part that needs an
//! operating system: choosing where a file goes, and choosing which file comes
//! in. Both are here for the same reason `open_vault` is -- a path is a fact
//! about *this machine*, and a vault on another one must never be able to name
//! one.
//!
//! # Why the bytes do not go through the webview
//!
//! They could: a browser attached to a paired vault has no shell and does
//! exactly that, assembling the chunks into a `Blob` and downloading it. That
//! works and it costs a copy of the whole archive on the JavaScript heap, plus
//! a base64 string of it on the way. On a desktop there is no reason to pay
//! that -- the shell can pull the chunks straight from the session and write
//! them to the file as they arrive, so a four-gigabyte export costs four
//! megabytes of memory.
//!
//! The session is the same one every other command goes through, so this works
//! unchanged when the vault is on another machine: the chunks come over the
//! pinned connection instead of out of this process, and the file still lands
//! on the desk the person is sitting at.

use base64::Engine;
use everyday_service::ctx::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

/// What a picked file turned out to be.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Picked {
    pub handle: String,
    pub name: String,
    pub bytes: u64,
}

/// Save an export the service has already built.
///
/// Answers `false` when the dialog was dismissed, which is not an error and
/// must not be drawn as one. The archive is released either way: a person who
/// changed their mind should not leave a copy of their journal resident until
/// the vault locks.
#[tauri::command]
pub async fn save_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    handle: String,
    name: String,
) -> CommandResult<bool> {
    let session = state.session().as_session();
    let Some(path) = ask_where(&app, &name).await else {
        let _ = session.call(Ctx::local(), "end_export", json_handle(&handle)).await;
        return Ok(false);
    };

    // Written to a neighbouring file and renamed at the end, so an export
    // interrupted halfway -- a full disk, a lid closing -- cannot be mistaken
    // for a whole one by whoever finds it later.
    let temp = path.with_extension("zip.part");
    let mut file = std::fs::File::create(&temp).map_err(|e| io(&temp, e))?;

    let mut offset: u64 = 0;
    loop {
        let value = session
            .call(
                Ctx::local(),
                "read_export",
                serde_json::json!({ "handle": handle, "offset": offset }),
            )
            .await?;
        let data = value.get("data").and_then(|d| d.as_str()).unwrap_or_default();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| CommandError::new("internal", format!("a chunk was not readable: {e}")))?;
        file.write_all(&bytes).map_err(|e| io(&temp, e))?;
        offset = value.get("offset").and_then(serde_json::Value::as_u64).unwrap_or(offset);
        if value.get("done").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            break;
        }
        if bytes.is_empty() {
            // Neither done nor making progress. Better to stop with a part
            // file than to spin.
            return Err(CommandError::new("internal", "the export stopped part way through"));
        }
    }
    file.sync_all().map_err(|e| io(&temp, e))?;
    drop(file);
    std::fs::rename(&temp, &path).map_err(|e| io(&path, e))?;
    Ok(true)
}

/// Pick an archive and hand it to the service, chunk by chunk.
///
/// Answers `None` when the dialog was dismissed. What comes back is the handle
/// the interface then inspects and imports with -- nothing has been read into
/// the vault at this point, and nothing will be until `run_import`.
#[tauri::command]
pub async fn open_import(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<Picked>> {
    let Some(path) = ask_which(&app).await else { return Ok(None) };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive.zip".into());
    let file = std::fs::File::open(&path).map_err(|e| io(&path, e))?;
    let total = file.metadata().map_err(|e| io(&path, e))?.len();

    let session = state.session().as_session();
    let started = session
        .call(Ctx::local(), "start_import", serde_json::json!({ "name": name, "bytes": total }))
        .await?;
    let handle = started
        .get("handle")
        .and_then(|h| h.as_str())
        .ok_or_else(|| CommandError::new("internal", "the vault gave no handle"))?
        .to_string();
    let chunk = started.get("chunk").and_then(serde_json::Value::as_u64).unwrap_or(4 << 20);

    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0u8; chunk as usize];
    let mut offset: u64 = 0;
    loop {
        let read = fill(&mut reader, &mut buffer).map_err(|e| io(&path, e))?;
        if read == 0 {
            break;
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&buffer[..read]);
        session
            .call(
                Ctx::local(),
                "write_import",
                serde_json::json!({ "handle": handle, "offset": offset, "data": data }),
            )
            .await?;
        offset += read as u64;
    }
    Ok(Some(Picked { handle, name, bytes: offset }))
}

/// Read until the buffer is full or the file ends.
///
/// `Read::read` is allowed to return fewer bytes than asked for at any time,
/// and a chunked upload that treated a short read as end-of-file would send a
/// truncated archive and blame the archive.
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

async fn ask_where(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Save your data")
        .set_file_name(name)
        .add_filter("Archive", &["zip"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

async fn ask_which(app: &tauri::AppHandle) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose what to import")
        .add_filter("Archive", &["zip"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

fn json_handle(handle: &str) -> serde_json::Value {
    serde_json::json!({ "handle": handle })
}

fn io(path: &std::path::Path, e: std::io::Error) -> CommandError {
    CommandError::new("io", format!("{}: {e}", path.display()))
}
