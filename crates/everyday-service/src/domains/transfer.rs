//! Taking your data out, and putting it back.
//!
//! The formats are [`everyday_transfer`]'s business; what is here is the
//! nine commands that get an archive across the boundary, and they are nine
//! rather than two for one reason: **an archive does not fit in a reply.**
//!
//! Every other command in this table answers with a value. An export of a
//! journal with photographs in it is hundreds of megabytes, and a JSON string
//! that size would be built, base64-encoded, parsed and held four times over
//! between here and a window. So a transfer is a handle and a series of
//! chunks, exactly as an attachment already is -- see
//! [`Service::blob_range`](crate::service::Service::blob_range) -- and for
//! exactly the same reason.
//!
//! ```text
//!   out:  list_parts -> start_export -> read_export * n -> end_export
//!   in:   start_import -> write_import * n -> read_import -> run_import
//! ```
//!
//! # Why the client saves the file, and not this
//!
//! No path crosses this boundary in either direction. A window picks where to
//! save with the operating system's own dialog and writes the bytes itself; a
//! window picks what to import with a file picker and sends the bytes. That is
//! the same rule `import_calendar` follows, and it is what makes both of these
//! work identically for a browser attached to a vault on another machine --
//! which has no filesystem to name and must not be given one.
//!
//! # The dry run is not optional
//!
//! [`read_import`] answers what an archive holds, and the interface shows it
//! before [`run_import`] is reachable. An import in `replace` mode overwrites
//! records somebody has written; being told what is about to happen is the
//! difference between a feature and an accident.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};
use crate::transfers::MAX_IMPORT;
use base64::Engine;
use everyday_transfer::{Manifest, Mode, Options, PartInfo, Report};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// How much of an archive one `read_export` hands over.
///
/// Four megabytes: large enough that a 200 MB export is fifty round trips
/// rather than fifty thousand, small enough that each one is a reply a
/// webview parses without a visible pause, and small enough that a paired
/// device on a slow link makes visible progress.
const CHUNK: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choose {
    /// Part ids. Empty means everything this vault can offer.
    #[serde(default)]
    pub parts: Vec<String>,
    /// Include photographs, video and cover art.
    #[serde(default)]
    pub media: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handle {
    pub handle: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadChunk {
    pub handle: String,
    pub offset: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteChunk {
    pub handle: String,
    pub offset: u64,
    /// The chunk, base64. Bytes do not survive JSON, and an array of numbers
    /// costs three times what this does.
    pub data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    /// What the file is called, for the messages about it. Never used as a
    /// path: this process opens nothing.
    #[serde(default)]
    pub name: String,
    pub bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunImport {
    pub handle: String,
    pub parts: Vec<String>,
    /// `skip` or `replace`. See [`Mode`].
    pub mode: String,
}

/// An export that has been built and is waiting to be fetched.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHandle {
    pub handle: String,
    pub bytes: u64,
    /// What to call the file, if the client has nothing better.
    pub name: String,
    /// The chunk size the client should ask for.
    pub chunk: u64,
    pub manifest: Manifest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportChunk {
    /// Base64. Empty when there is nothing left.
    pub data: String,
    pub offset: u64,
    pub done: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportUpload {
    pub handle: String,
    pub chunk: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub bytes: u64,
    pub done: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub reports: Vec<Report>,
    pub added: u64,
    pub replaced: u64,
    pub skipped: u64,
}

// ---- out ----------------------------------------------------------------

/// What this vault can hand over, and how much of it there is.
async fn list_parts(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<PartInfo>> {
    let vault = svc.require_unlocked()?;
    blocking(move || Ok(everyday_transfer::survey(&vault)?)).await
}

/// Build the archive. Answers with a handle to read it from.
async fn start_export(svc: Arc<Service>, _ctx: Ctx, args: Choose) -> CommandResult<ExportHandle> {
    let vault = svc.require_unlocked()?;
    let opts = Options { parts: args.parts, media: args.media };
    let (bytes, manifest) =
        blocking(move || Ok(everyday_transfer::export(&vault, &opts, Vec::new())?)).await?;
    let size = bytes.len() as u64;
    let handle = svc.transfers().put(bytes, size)?;
    Ok(ExportHandle { handle, bytes: size, name: manifest.filename(), chunk: CHUNK, manifest })
}

/// The next piece of it.
///
/// The archive is dropped as soon as the last chunk has been handed over, so
/// the ordinary path needs no cleanup call at all -- `end_export` is for the
/// window that was closed halfway.
async fn read_export(svc: Arc<Service>, _ctx: Ctx, args: ReadChunk) -> CommandResult<ExportChunk> {
    let transfers = svc.transfers();
    let total = transfers.len(&args.handle)?;
    let data = transfers.read(&args.handle, args.offset, CHUNK)?;
    let offset = args.offset + data.len() as u64;
    let done = offset >= total;
    if done {
        transfers.drop_one(&args.handle);
    }
    Ok(ExportChunk { data: base64::engine::general_purpose::STANDARD.encode(&data), offset, done })
}

async fn end_export(svc: Arc<Service>, _ctx: Ctx, args: Handle) -> CommandResult<()> {
    svc.transfers().drop_one(&args.handle);
    Ok(())
}

// ---- in -----------------------------------------------------------------

/// Say an archive is coming, and how big it is.
async fn start_import(svc: Arc<Service>, _ctx: Ctx, args: Incoming) -> CommandResult<ImportUpload> {
    svc.require_unlocked()?;
    if args.bytes == 0 {
        return Err(CommandError::new("invalid", "that file is empty"));
    }
    if args.bytes > MAX_IMPORT {
        return Err(CommandError::new(
            "invalid",
            "that file is too large to read in one piece; `everyday import` on the \
             command line will take it",
        ));
    }
    let handle = svc.transfers().expect(args.bytes)?;
    Ok(ImportUpload { handle, chunk: CHUNK })
}

async fn write_import(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: WriteChunk,
) -> CommandResult<ImportProgress> {
    let chunk = base64::engine::general_purpose::STANDARD
        .decode(args.data.as_bytes())
        .map_err(|e| CommandError::new("invalid", format!("that chunk is not base64: {e}")))?;
    let transfers = svc.transfers();
    let bytes = transfers.write(&args.handle, args.offset, &chunk)?;
    Ok(ImportProgress { bytes, done: transfers.complete(&args.handle).is_ok() })
}

/// What is in the archive that has arrived, without changing anything.
async fn read_import(svc: Arc<Service>, _ctx: Ctx, args: Handle) -> CommandResult<Manifest> {
    svc.require_unlocked()?;
    let bytes = svc.transfers().complete(&args.handle)?;
    blocking(move || {
        let archive = everyday_transfer::zip::Reader::open(&bytes)?;
        Ok(everyday_transfer::inspect(&archive)?)
    })
    .await
}

/// Read it in.
///
/// Announces a change for every kind an app could have touched rather than one
/// for the import as a whole. A window listening for `entry` has no way to act
/// on "an import happened", and the point of a change event is that a list
/// reloads itself.
async fn run_import(svc: Arc<Service>, ctx: Ctx, args: RunImport) -> CommandResult<ImportResult> {
    let vault = svc.require_unlocked()?;
    let mode = Mode::parse(&args.mode)
        .ok_or_else(|| CommandError::new("invalid", format!("no such mode: {}", args.mode)))?;
    let bytes = svc.transfers().complete(&args.handle)?;
    let parts = args.parts;

    let reports = blocking(move || {
        let archive = everyday_transfer::zip::Reader::open(&bytes)?;
        Ok(everyday_transfer::import(&vault, &archive, &parts, mode)?)
    })
    .await?;
    svc.transfers().drop_one(&args.handle);

    let origin = ctx.caller.origin().map(str::to_string);
    for report in reports.iter().filter(|r| r.touched() > 0) {
        for kind in kinds_of(&report.part) {
            let change = Change { kind: *kind, op: Op::Updated, id: None, origin: origin.clone() };
            svc.events().changed(change);
        }
    }

    Ok(ImportResult {
        added: reports.iter().map(|r| r.added).sum(),
        replaced: reports.iter().map(|r| r.replaced).sum(),
        skipped: reports.iter().map(|r| r.skipped).sum(),
        reports,
    })
}

async fn end_import(svc: Arc<Service>, _ctx: Ctx, args: Handle) -> CommandResult<()> {
    svc.transfers().drop_one(&args.handle);
    Ok(())
}

/// Which lists an app's import could have changed.
///
/// A map, and the one in this file, because it is the only place in the
/// application where one write touches several kinds at once. Every other
/// command names its single `change:` in its own table entry. A part missing
/// from here costs a list that does not refresh until it is reopened, which is
/// why the fallback is `Settings` -- visible enough to be noticed -- rather
/// than nothing.
fn kinds_of(part: &str) -> &'static [Kind] {
    match part {
        "journal" => &[Kind::Journal, Kind::Entry],
        "notes" => &[Kind::Note],
        "todo" => &[Kind::Project, Kind::Task, Kind::Block],
        "calendar" => &[Kind::Calendar, Kind::Event],
        "library" => &[Kind::Shelf, Kind::Item, Kind::Log],
        "trackers" => &[Kind::Tracker, Kind::Reading],
        "purpose" => &[Kind::Role, Kind::Goal],
        "assistant" => &[Kind::Conversation, Kind::Routine, Kind::Memory],
        _ => &[Kind::Settings],
    }
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_parts", scope: All, effect: Read,
        args: Nothing, returns: "PartInfo[]", signature: &[],
        run: list_parts,
    },
    command! {
        name: "start_export", scope: All, effect: Read,
        // The command `Ctx::proved_at` was written for. An export is the one
        // read that hands the whole vault over in the clear, so when step-up
        // arrives this is where it attaches -- and declaring it now means no
        // client needs a new protocol on the day it does.
        sensitive: true,
        args: Choose, returns: "ExportHandle",
        signature: &[("parts", "string[]", true), ("media", "boolean", true)],
        run: start_export,
    },
    command! {
        name: "read_export", scope: All, effect: Read,
        sensitive: true,
        args: ReadChunk, returns: "ExportChunk",
        signature: &[("handle", "string", true), ("offset", "number", true)],
        run: read_export,
    },
    command! {
        name: "end_export", scope: All, effect: Read,
        args: Handle, returns: "void",
        signature: &[("handle", "string", true)],
        run: end_export,
    },
    command! {
        name: "start_import", scope: All, effect: Read,
        args: Incoming, returns: "ImportUpload",
        signature: &[("name", "string", true), ("bytes", "number", true)],
        run: start_import,
    },
    command! {
        name: "write_import", scope: All, effect: Read,
        args: WriteChunk, returns: "ImportProgress",
        signature: &[
            ("handle", "string", true),
            ("offset", "number", true),
            ("data", "string", true),
        ],
        run: write_import,
    },
    command! {
        name: "read_import", scope: All, effect: Read,
        args: Handle, returns: "ArchiveManifest",
        signature: &[("handle", "string", true)],
        run: read_import,
    },
    command! {
        name: "run_import", scope: All, effect: Destructive,
        args: RunImport, returns: "ImportResult",
        signature: &[
            ("handle", "string", true),
            ("parts", "string[]", true),
            ("mode", "string", true),
        ],
        run: run_import,
    },
    command! {
        name: "end_import", scope: All, effect: Read,
        args: Handle, returns: "void",
        signature: &[("handle", "string", true)],
        run: end_import,
    },
];
