//! Files: the catalog, the files folder, albums and uploads. The work is
//! [`karmayoga_core::Vault`]'s (`vault::files`) and [`crate::files`]'; these
//! are its doors.
//!
//! Scope `Files` for everything. Three things are narrower still:
//!
//! - **`import_files` takes paths on this machine**, so only the shell on
//!   the vault's own machine may call it ([`Caller::Local`] or
//!   [`Caller::Window`]): a paired device or a browser naming
//!   `~/.ssh/id_ed25519` must not be able to copy it into a folder it can
//!   then read. Everything else uploads, in chunks ([`upload_chunk`]).
//! - **Choosing the folder is the same power**: a caller that could name
//!   `~/.ssh` as the files folder would read it through the catalog without
//!   importing anything. So `save_files_settings` changes its `folder`, and
//!   `set_up_files_folder` takes a `path`, only from that same shell; anyone
//!   else may set the folder up at the default place if it never was.
//! - **`save_mail_part_to_files` reads mail**, so it needs `Mail` as well.
//! - **Health's files need `Health` as well**: a clinical document's pages
//!   and a meal's photographs are listed only to a connection that holds it,
//!   and every command naming files by id refuses one to a connection that
//!   does not ([`sees_health`]).
//!
//! Every command that adds a file answers as soon as the bytes are in place
//! and leaves reading what the file says about itself, and drawing its
//! thumbnail, to a background job ([`crate::files::probe_later`]) that
//! announces each batch it finishes.
//!
//! [`Caller::Local`]: crate::ctx::Caller::Local
//! [`Caller::Window`]: crate::ctx::Caller::Window

use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::files::disk;
use karmayoga_core::files::{
    Album, AlbumView, FileEntry, FileFilter, FileOrigin, FilePage, FileQuery, FileSort, FileStatus,
    FileStorage, FileUpdate, FileUse, FilesFacets, FilesSettings, FilesUsage, FolderListing,
    FolderState, ImportOptions, LegacyReport, LegacySummary, ScanReport, UploadTicket,
};
use karmayoga_core::id::{AlbumId, FileId, MailMessageId};
use serde::{Deserialize, Serialize};

use crate::backup::{self, BackupError};
use crate::command;
use crate::ctx::{Ctx, Scope};
use crate::error::{CommandError, CommandResult, codes};
use crate::events::Notification;
use crate::files;
use crate::mail::view as mailview;
use crate::service::{Service, blocking};

use super::Nothing;

/// What the Files app and Settings → Files read first. Mirrors the TS
/// `FilesStatus`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesStatus {
    /// Whether this vault keeps a files catalog at all.
    pub supported: bool,
    pub state: FolderState,
    pub settings: FilesSettings,
    pub usage: FilesUsage,
    /// A pass of the reconciler is running now.
    pub scanning: bool,
    pub last_scan: Option<Timestamp>,
    pub last_report: Option<ScanReport>,
    /// Pictures and files still kept the old way, inside records.
    pub legacy: LegacySummary,
    /// Whether this caller is the shell on the vault's own machine, and so
    /// may pick files by path (`import_files`) and open them in another
    /// program. A paired device or a browser uploads instead.
    pub local: bool,
}

/// One file with everything said about it. Mirrors the TS `FileDetail`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetail {
    pub file: FileEntry,
    pub uses: Vec<FileUse>,
    pub albums: Vec<Album>,
}

// ---- arguments ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFilesSettings {
    pub settings: FilesSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetUpFolder {
    /// Absolute; the default folder when absent.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFiles {
    pub query: FileQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFolder {
    /// Relative to the files folder; `""` is the top.
    #[serde(default)]
    pub dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRef {
    pub id: FileId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIds {
    pub ids: Vec<FileId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFiles {
    /// Absolute paths on the vault's machine, from a file picker or a drop.
    pub paths: Vec<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub origin: Option<FileOrigin>,
    #[serde(default)]
    pub encrypted: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginUpload {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub origin: Option<FileOrigin>,
    #[serde(default)]
    pub encrypted: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRef {
    pub upload: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFolder {
    #[serde(default)]
    pub dir: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFile {
    pub id: FileId,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFiles {
    pub ids: Vec<FileId>,
    #[serde(default)]
    pub dir: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFolder {
    pub path: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFolder {
    pub path: String,
    #[serde(default)]
    pub into: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRef {
    pub path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFiles {
    pub ids: Vec<FileId>,
    pub update: FileUpdate,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFiles {
    pub top: FileId,
    pub others: Vec<FileId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEncrypted {
    pub id: FileId,
    pub encrypted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetThumbnail {
    pub id: FileId,
    /// A JPEG or PNG, base64 (standard alphabet, padded).
    pub image: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAlbum {
    pub album: Album,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRef {
    pub id: AlbumId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumFiles {
    pub id: AlbumId,
    pub files: Vec<FileId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMailPart {
    pub message_id: MailMessageId,
    /// The part, as `karmayoga://mail/part/{message}/{part}` names it: a
    /// content id or an index.
    pub part: String,
    #[serde(default)]
    pub dir: Option<String>,
}

// ---- the commands ------------------------------------------------------------

/// The largest chunk of an upload taken in one call. The interface sends
/// 4 MB; this is room for a client that sends more, and a bound on what a
/// client that sends nonsense can make this allocate.
const MAX_CHUNK: usize = 64 * 1024 * 1024;

/// Why a caller that is not the shell on this machine may not name a folder.
const FOLDER_IS_LOCAL: &str = "the files folder can be chosen only from this computer's own \
    window: a folder named from anywhere else could be any folder this computer can read";

/// The folder Settings offers when nothing has been chosen.
fn suggested_folder() -> String {
    disk::default_folder().map(|p| p.display().to_string()).unwrap_or_default()
}

/// What the Files app and Settings → Files read first. A vault that keeps no
/// catalog answers a status with defaults, `supported: false`, rather than an
/// error, so the pane can say so instead of failing.
fn status_of(vault: &Vault, local: bool) -> karmayoga_core::Result<FilesStatus> {
    if !vault.supports_files() {
        return Ok(FilesStatus {
            supported: false,
            state: FolderState::NotSetUp { suggested: suggested_folder() },
            settings: FilesSettings::default(),
            usage: FilesUsage::default(),
            scanning: false,
            last_scan: None,
            last_report: None,
            legacy: LegacySummary::default(),
            local,
        });
    }
    let last = files::task::last_scan(vault);
    Ok(FilesStatus {
        supported: true,
        state: vault.folder_state()?,
        settings: vault.files_settings()?,
        usage: vault.files_usage()?,
        scanning: files::task::scanning(vault),
        last_scan: last.as_ref().map(|(at, _)| *at),
        last_report: last.map(|(_, report)| report),
        legacy: legacy_summary(vault)?,
        local,
    })
}

/// How many pictures are still kept the old way, counted once per vault per
/// process and remembered: counting opens every entry, note and message,
/// which on a large vault is seconds, and the Files app reads its status on
/// every visit. The number only ever falls -- a vault with a catalog keeps
/// every new picture in it -- so the one thing that changes it,
/// [`bring_in_attachments`], forgets the count ([`forget_legacy_summary`]).
fn legacy_summary(vault: &Vault) -> karmayoga_core::Result<LegacySummary> {
    let key = vault.path().to_path_buf();
    if let Some(known) = legacy_counts().get(&key) {
        return Ok(known.clone());
    }
    let summary = vault.legacy_attachments()?;
    legacy_counts().insert(key, summary.clone());
    Ok(summary)
}

fn forget_legacy_summary(vault: &Vault) {
    legacy_counts().remove(vault.path());
}

fn legacy_counts()
-> std::sync::MutexGuard<'static, std::collections::HashMap<std::path::PathBuf, LegacySummary>> {
    static COUNTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, LegacySummary>>,
    > = std::sync::OnceLock::new();
    COUNTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

async fn files_status(svc: Arc<Service>, ctx: Ctx, _args: Nothing) -> CommandResult<FilesStatus> {
    let local = ctx.is_local();
    svc.on_vault(move |vault| status_of(vault, local)).await
}

/// Save the settings. The folder in them is the one place a caller names a
/// path on this machine without importing anything, so it is held to the
/// same rule as [`import_files`]: only the shell here may change it.
async fn save_files_settings(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SaveFilesSettings,
) -> CommandResult<FilesStatus> {
    let local = ctx.is_local();
    let vault = svc.require()?;
    let status = blocking(move || {
        if !local && vault.files_settings()?.folder != args.settings.folder {
            return Err(CommandError::new(codes::FORBIDDEN, FOLDER_IS_LOCAL));
        }
        vault.save_files_settings(&args.settings)?;
        Ok(status_of(&vault, local)?)
    })
    .await?;
    // A new folder is a folder to watch and to look through; a different
    // trash period is nothing to wait five minutes for.
    files::task::wake();
    // And what is indexed -- on or off, which folders -- is in these settings:
    // the documents' index has a pass to make now, not in five minutes.
    crate::documents::task::wake();
    Ok(status)
}

async fn set_up_files_folder(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SetUpFolder,
) -> CommandResult<FilesStatus> {
    let local = ctx.is_local();
    let vault = svc.require()?;
    let status = blocking(move || {
        // From anywhere else, only the first setup, at the default place.
        if !local && (args.path.is_some() || vault.files_settings()?.folder.is_some()) {
            return Err(CommandError::new(codes::FORBIDDEN, FOLDER_IS_LOCAL));
        }
        vault.set_up_files_folder(args.path.as_deref().map(Path::new))?;
        Ok(status_of(&vault, local)?)
    })
    .await?;
    files::task::wake();
    Ok(status)
}

// ---- Health's files ---------------------------------------------------------

/// Whether `ctx` may see Health's files -- a clinical document's pages, a
/// meal's photographs. They answer to Health's scope as well as this one: a
/// connection allowed `files` and not `health` reads the folder, not
/// somebody's lab reports. It is shown none of them -- a listing would hand
/// it their blob addresses, which `/v1/blob` answers -- and is refused any
/// act on one by id. The window and a paired copy of the app hold `All`, and
/// see everything.
fn sees_health(ctx: &Ctx) -> bool {
    ctx.holds(Scope::Health)
}

/// Refuse, for a connection that may not see Health's files, any of `ids`
/// that is one of them. A file that is not there is left for the command's
/// own read to report.
fn refuse_healths(ctx: &Ctx, vault: &Vault, ids: &[FileId]) -> CommandResult<()> {
    if sees_health(ctx) {
        return Ok(());
    }
    for id in ids {
        if vault.file(*id).is_ok_and(|f| f.origin == FileOrigin::Health) {
            return Err(CommandError::new(
                codes::FORBIDDEN,
                "that file is one of Health's, which this connection is not allowed to reach",
            ));
        }
    }
    Ok(())
}

/// [`Service::on_vault`] for a command that names files by id: refused
/// first if any is one of Health's and `ctx` may not see them.
async fn on_files<T, F>(svc: &Service, ctx: Ctx, ids: Vec<FileId>, f: F) -> CommandResult<T>
where
    F: FnOnce(&Vault) -> karmayoga_core::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let vault = svc.require()?;
    blocking(move || {
        refuse_healths(&ctx, &vault, &ids)?;
        Ok(f(&vault)?)
    })
    .await
}

async fn list_files(svc: Arc<Service>, ctx: Ctx, args: ListFiles) -> CommandResult<FilePage> {
    let mut query = args.query;
    if !sees_health(&ctx) {
        query.filter.exclude_origins.push(FileOrigin::Health);
    }
    svc.on_vault(move |vault| vault.files(&query)).await
}

async fn list_folder(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ListFolder,
) -> CommandResult<FolderListing> {
    let mut listing = svc.on_vault(move |vault| vault.list_folder(&args.dir)).await?;
    if !sees_health(&ctx) {
        // Health seals what it keeps, so one of its files is in a folder
        // only after somebody wrote it out there; it is still Health's.
        listing.files.retain(|f| f.origin != FileOrigin::Health);
    }
    Ok(listing)
}

async fn get_file(svc: Arc<Service>, ctx: Ctx, args: FileRef) -> CommandResult<FileDetail> {
    on_files(&svc, ctx, vec![args.id], move |vault| {
        Ok(FileDetail {
            file: vault.file(args.id)?,
            uses: vault.file_uses(args.id)?,
            albums: vault.file_albums(args.id)?,
        })
    })
    .await
}

async fn files_facets(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<FilesFacets> {
    svc.on_vault(|vault| vault.files_facets()).await
}

/// Copy files from paths on this machine into the catalog.
///
/// Refused to every caller but the shell here: a path is read with this
/// process's own rights, so a paired device or a browser naming
/// `~/.ssh/id_ed25519` would be reading it into a folder it can then list.
///
/// Answers once the copying is done and reads what the files say about
/// themselves, and draws their thumbnails, afterwards (see
/// [`files::probe_later`]). One that cannot be copied does not stop the
/// others: what went wrong is said in a notice, and only when nothing at all
/// could be added is the call an error.
async fn import_files(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ImportFiles,
) -> CommandResult<Vec<FileEntry>> {
    if !ctx.is_local() {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "files can be picked by path only from this computer's own window; upload them \
             instead",
        ));
    }
    if let Some(relative) = args.paths.iter().find(|p| !Path::new(p).is_absolute()) {
        return Err(CommandError::new(
            codes::INVALID,
            format!("{relative:?} is not an absolute path"),
        ));
    }
    let vault = svc.require()?;
    let options = import_options(args.dir, args.origin, args.encrypted);
    let (imported, failed) = {
        let vault = vault.clone();
        blocking(move || {
            let mut imported = Vec::new();
            let mut failed = Vec::new();
            for path in &args.paths {
                match vault.import_file(Path::new(path), &options) {
                    Ok(entry) => imported.push(entry),
                    Err(e) => failed.push((path.clone(), e)),
                }
            }
            Ok((imported, failed))
        })
        .await?
    };
    if imported.is_empty()
        && let Some((path, e)) = failed.first()
    {
        return Err(CommandError::new(e.code(), format!("{path}: {e}")));
    }
    if !failed.is_empty() {
        let listed: Vec<String> =
            failed.iter().take(3).map(|(path, e)| format!("{path}: {e}")).collect();
        let more = failed.len().saturating_sub(listed.len());
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        let title = match failed.len() {
            1 => "A file could not be added".to_string(),
            n => format!("{n} files could not be added"),
        };
        svc.events().notify(
            Notification::warning(title)
                .body(format!("{}{tail}.", listed.join("; ")))
                .domain(Scope::Files),
        );
    }
    files::probe_later(&svc, &vault, imported.iter().map(|f| f.id).collect());
    Ok(imported)
}

fn import_options(
    dir: Option<String>,
    origin: Option<FileOrigin>,
    encrypted: Option<bool>,
) -> ImportOptions {
    ImportOptions { dir, name: None, origin: origin.unwrap_or(FileOrigin::Files), encrypted }
}

async fn begin_upload(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: BeginUpload,
) -> CommandResult<UploadTicket> {
    let options = import_options(args.dir, args.origin, args.encrypted);
    svc.on_vault(move |vault| vault.begin_upload(&args.name, args.size, &options)).await
}

/// One chunk of an upload. Not in the command table: its body is bytes, not
/// JSON, as `put_blob`'s is -- the desktop shell's `upload_chunk` command
/// and the server's `POST /v1/files/upload/{upload}?offset=` both call this.
/// Answers the bytes received so far.
pub async fn upload_chunk(
    svc: Arc<Service>,
    ctx: Ctx,
    upload: String,
    offset: u64,
    bytes: Vec<u8>,
) -> CommandResult<u64> {
    ctx.require(Scope::Files)?;
    if bytes.len() > MAX_CHUNK {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!(
                "a chunk is {} MB; send an upload in pieces of at most {} MB",
                bytes.len() / 1_048_576,
                MAX_CHUNK / 1_048_576
            ),
        ));
    }
    svc.on_vault(move |vault| vault.upload_chunk(&upload, offset, &bytes)).await
}

async fn finish_upload(svc: Arc<Service>, _ctx: Ctx, args: UploadRef) -> CommandResult<FileEntry> {
    let vault = svc.require()?;
    let entry = {
        let vault = vault.clone();
        blocking(move || Ok(vault.finish_upload(&args.upload)?)).await?
    };
    files::probe_later(&svc, &vault, vec![entry.id]);
    Ok(entry)
}

async fn cancel_upload(svc: Arc<Service>, _ctx: Ctx, args: UploadRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.cancel_upload(&args.upload)).await
}

async fn create_folder(svc: Arc<Service>, _ctx: Ctx, args: CreateFolder) -> CommandResult<String> {
    svc.on_vault(move |vault| vault.create_folder(&args.dir, &args.name)).await
}

async fn rename_file(svc: Arc<Service>, ctx: Ctx, args: RenameFile) -> CommandResult<FileEntry> {
    on_files(&svc, ctx, vec![args.id], move |vault| vault.rename_file(args.id, &args.name)).await
}

async fn move_files(svc: Arc<Service>, ctx: Ctx, args: MoveFiles) -> CommandResult<Vec<FileEntry>> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.move_files(&args.ids, &args.dir)).await
}

async fn rename_folder(svc: Arc<Service>, _ctx: Ctx, args: RenameFolder) -> CommandResult<String> {
    svc.on_vault(move |vault| vault.rename_folder(&args.path, &args.name)).await
}

async fn move_folder(svc: Arc<Service>, _ctx: Ctx, args: MoveFolder) -> CommandResult<String> {
    svc.on_vault(move |vault| vault.move_folder(&args.path, &args.into)).await
}

async fn update_files(
    svc: Arc<Service>,
    ctx: Ctx,
    args: UpdateFiles,
) -> CommandResult<Vec<FileEntry>> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.update_files(&args.ids, &args.update))
        .await
}

async fn stack_files(svc: Arc<Service>, ctx: Ctx, args: StackFiles) -> CommandResult<()> {
    let named: Vec<FileId> = std::iter::once(args.top).chain(args.others.iter().copied()).collect();
    on_files(&svc, ctx, named, move |vault| vault.stack_files(args.top, &args.others)).await
}

async fn unstack_files(svc: Arc<Service>, ctx: Ctx, args: FileIds) -> CommandResult<()> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.unstack_files(&args.ids)).await
}

async fn trash_files(svc: Arc<Service>, ctx: Ctx, args: FileIds) -> CommandResult<Vec<FileEntry>> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.trash_files(&args.ids)).await
}

async fn trash_folder(svc: Arc<Service>, _ctx: Ctx, args: FolderRef) -> CommandResult<u64> {
    svc.on_vault(move |vault| vault.trash_folder(&args.path)).await
}

async fn restore_files(
    svc: Arc<Service>,
    ctx: Ctx,
    args: FileIds,
) -> CommandResult<Vec<FileEntry>> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.restore_files(&args.ids)).await
}

async fn delete_files_forever(svc: Arc<Service>, ctx: Ctx, args: FileIds) -> CommandResult<u64> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.delete_files_forever(&args.ids)).await
}

async fn empty_files_trash(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<u64> {
    svc.on_vault(|vault| vault.empty_files_trash()).await
}

async fn forget_missing_files(svc: Arc<Service>, ctx: Ctx, args: FileIds) -> CommandResult<u64> {
    on_files(&svc, ctx, args.ids.clone(), move |vault| vault.forget_missing_files(&args.ids)).await
}

async fn set_file_encrypted(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SetEncrypted,
) -> CommandResult<FileEntry> {
    on_files(&svc, ctx, vec![args.id], move |vault| {
        vault.set_file_encrypted(args.id, args.encrypted)
    })
    .await
}

/// Keep the poster the interface drew of a film, which the interface can
/// play and this side cannot. Checked and re-encoded
/// ([`files::thumb::accept_uploaded`]): what arrives is a stranger's bytes
/// until it has been decoded and drawn again.
async fn set_file_thumbnail(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SetThumbnail,
) -> CommandResult<FileEntry> {
    let vault = svc.require()?;
    blocking(move || {
        refuse_healths(&ctx, &vault, &[args.id])?;
        let bytes = decode_image(&args.image)?;
        let jpeg = files::thumb::accept_uploaded(&bytes).ok_or_else(|| {
            CommandError::new(
                codes::NOT_AN_IMAGE,
                "that is not a JPEG or PNG picture small enough to be a thumbnail",
            )
        })?;
        let current = vault.file(args.id)?;
        Ok(vault.save_file_derived(args.id, current.media, Some(&jpeg), None)?)
    })
    .await
}

/// The bytes of a base64 picture, with or without a `data:` URL's head,
/// refused before decoding if too large to be a thumbnail.
fn decode_image(image: &str) -> CommandResult<Vec<u8>> {
    use base64::Engine;
    let image = image.trim();
    let image = match image.strip_prefix("data:") {
        Some(url) => url.split_once(',').map_or(url, |(_, data)| data),
        None => image,
    };
    if image.len() > files::thumb::MAX_UPLOADED / 3 * 4 + 8 {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("a thumbnail is at most {} MB", files::thumb::MAX_UPLOADED / 1_048_576),
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(image)
        .map_err(|e| CommandError::new(codes::INVALID, format!("the picture is not base64: {e}")))
}

/// Look through the folder now rather than at the next pass.
async fn rescan_files(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<ScanReport> {
    let vault = svc.require()?;
    blocking(move || Ok(files::task::scan_now(&svc, &vault))).await
}

async fn list_albums(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<AlbumView>> {
    svc.on_vault(|vault| vault.albums()).await
}

async fn save_album(svc: Arc<Service>, _ctx: Ctx, args: SaveAlbum) -> CommandResult<Album> {
    svc.on_vault(move |vault| vault.save_album(&args.album)).await
}

async fn delete_album(svc: Arc<Service>, _ctx: Ctx, args: AlbumRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_album(args.id)).await
}

async fn add_to_album(svc: Arc<Service>, ctx: Ctx, args: AlbumFiles) -> CommandResult<()> {
    on_files(&svc, ctx, args.files.clone(), move |vault| vault.add_to_album(args.id, &args.files))
        .await
}

async fn remove_from_album(svc: Arc<Service>, ctx: Ctx, args: AlbumFiles) -> CommandResult<()> {
    on_files(&svc, ctx, args.files.clone(), move |vault| {
        vault.remove_from_album(args.id, &args.files)
    })
    .await
}

/// Bring pictures and files kept the old way, inside entries, notes and
/// messages, into the files folder. Answers when they are written; what they
/// say about themselves is read, and their thumbnails drawn, afterwards.
async fn bring_in_attachments(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<LegacyReport> {
    let vault = svc.require_unlocked()?;
    let (report, created) = {
        let vault = vault.clone();
        blocking(move || {
            let report = vault.bring_in_attachments();
            forget_legacy_summary(&vault);
            let report = report?;
            // The files it made are the newest the catalog has.
            let created: Vec<FileId> = if report.files == 0 {
                Vec::new()
            } else {
                let query = FileQuery {
                    filter: FileFilter {
                        sort: FileSort::Added,
                        descending: true,
                        limit: Some(report.files),
                        ..FileFilter::default()
                    },
                    ..FileQuery::default()
                };
                vault.files(&query)?.files.into_iter().map(|f| f.id).collect()
            };
            Ok((report, created))
        })
        .await?
    };
    files::probe_later(&svc, &vault, created);
    Ok(report)
}

/// Keep a mail attachment as a file. Reads mail, so wants `Mail` as well as
/// `Files`.
async fn save_mail_part_to_files(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SaveMailPart,
) -> CommandResult<FileEntry> {
    ctx.require(Scope::Mail)?;
    let vault = svc.require_unlocked()?;
    let entry = {
        let vault = vault.clone();
        blocking(move || {
            let part = mailview::part(&vault, args.message_id, &args.part)?;
            let name = attachment_name(part.filename.as_deref());
            let options = import_options(args.dir, Some(FileOrigin::Mail), None);
            Ok(vault.import_bytes(&part.bytes, &name, &options)?)
        })
        .await?
    };
    files::probe_later(&svc, &vault, vec![entry.id]);
    Ok(entry)
}

/// A name for a file saved from a mail attachment. The sender chose the
/// name, so it is a stranger's: only its last segment, no control
/// characters, no leading dots (a name starting with one is left out of the
/// folder's listings), and short enough to be a name.
fn attachment_name(claimed: Option<&str>) -> String {
    let last = claimed.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().map(|c| if c.is_control() { '_' } else { c }).collect();
    let name = cleaned.trim().trim_start_matches('.').trim();
    if name.is_empty() {
        return "attachment".to_string();
    }
    if name.len() <= NAME_BUDGET {
        return name.to_string();
    }
    // Over long: the extension is kept, the rest is cut at a letter.
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if ext.len() <= 16 => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let room = NAME_BUDGET.saturating_sub(ext.len());
    let mut cut = room.min(stem.len());
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{ext}", &stem[..cut])
}

/// How long a saved attachment's name may be, in bytes: a folder's limit is
/// 255, and room is left for the ` (2)` a clash adds.
const NAME_BUDGET: usize = 200;

/// Bring a missing file back from the newest backup that holds it.
///
/// Only a file the catalog has as missing, whose path is free: the backup
/// puts it back where the catalog has it, and a pass of the reconciler then
/// finds it there again, with everything the person added.
async fn restore_file_from_backup(
    svc: Arc<Service>,
    ctx: Ctx,
    args: FileRef,
) -> CommandResult<FileEntry> {
    let vault = svc.require_unlocked()?;
    let id = args.id;
    let path = {
        let vault = vault.clone();
        blocking(move || {
            refuse_healths(&ctx, &vault, &[id])?;
            let entry = vault.file(id)?;
            match (&entry.status, &entry.storage) {
                (FileStatus::Missing { .. }, FileStorage::Plain { path }) => Ok(path.clone()),
                _ => Err(CommandError::new(
                    codes::INVALID,
                    "only a file that has gone missing from the folder can be brought back from \
                     a backup",
                )),
            }
        })
        .await?
    };
    let restored = backup::restore::restore_file(&vault, &path).await.map_err(backup_failure)?;
    if restored.is_none() {
        return Err(CommandError::new(
            codes::NOT_FOUND,
            format!(
                "none of the backups holds {path}: it may have gone before the first one was \
                 made, or been moved since"
            ),
        ));
    }
    // Found again at once, rather than at the next pass.
    blocking(move || {
        files::task::scan_now(&svc, &vault);
        Ok(vault.file(id)?)
    })
    .await
}

/// What a backup's failure is called to a caller.
fn backup_failure(e: BackupError) -> CommandError {
    use crate::backup::target::TargetErrorKind;
    match e {
        BackupError::Target(e) => {
            let code = match e.kind {
                TargetErrorKind::Unreachable => codes::UNREACHABLE,
                TargetErrorKind::Denied | TargetErrorKind::Credentials => codes::FORBIDDEN,
                TargetErrorKind::Missing => codes::NOT_FOUND,
                TargetErrorKind::Other => codes::INVALID,
            };
            CommandError::new(code, e.message)
        }
        BackupError::Vault(e) => CommandError::from(e),
        BackupError::Cancelled | BackupError::Refused(_) => {
            CommandError::new(codes::INVALID, e.to_string())
        }
    }
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "files_status", scope: Files, effect: Read,
        args: Nothing, returns: "FilesStatus", signature: &[],
        run: files_status,
    },
    command! {
        name: "save_files_settings", scope: Files, effect: Write,
        change: Settings / Updated,
        args: SaveFilesSettings, returns: "FilesStatus",
        signature: &[("settings", "FilesSettings", true)],
        run: save_files_settings,
    },
    command! {
        name: "set_up_files_folder", scope: Files, effect: Write,
        change: Settings / Updated,
        args: SetUpFolder, returns: "FilesStatus",
        signature: &[("path", "string", false)],
        run: set_up_files_folder,
    },
    command! {
        name: "list_files", scope: Files, effect: Read,
        args: ListFiles, returns: "FilePage",
        signature: &[("query", "FileQuery", true)],
        run: list_files,
    },
    command! {
        name: "list_folder", scope: Files, effect: Read,
        args: ListFolder, returns: "FolderListing",
        signature: &[("dir", "string", false)],
        run: list_folder,
    },
    command! {
        name: "get_file", scope: Files, effect: Read,
        args: FileRef, returns: "FileDetail",
        signature: &[("id", "string", true)],
        run: get_file,
    },
    command! {
        name: "files_facets", scope: Files, effect: Read,
        args: Nothing, returns: "FilesFacets", signature: &[],
        run: files_facets,
    },
    command! {
        name: "import_files", scope: Files, effect: Write,
        change: File / Created,
        args: ImportFiles, returns: "FileEntry[]",
        signature: &[
            ("paths", "string[]", true),
            ("dir", "string", false),
            ("origin", "FileOrigin", false),
            ("encrypted", "boolean", false),
        ],
        run: import_files,
    },
    command! {
        name: "begin_upload", scope: Files, effect: Write,
        args: BeginUpload, returns: "UploadTicket",
        signature: &[
            ("name", "string", true),
            ("size", "number", true),
            ("dir", "string", false),
            ("origin", "FileOrigin", false),
            ("encrypted", "boolean", false),
        ],
        run: begin_upload,
    },
    command! {
        name: "finish_upload", scope: Files, effect: Write,
        change: File / Created,
        args: UploadRef, returns: "FileEntry",
        signature: &[("upload", "string", true)],
        run: finish_upload,
    },
    command! {
        name: "cancel_upload", scope: Files, effect: Write,
        args: UploadRef, returns: "void",
        signature: &[("upload", "string", true)],
        run: cancel_upload,
    },
    command! {
        name: "create_folder", scope: Files, effect: Write,
        change: File / Updated,
        args: CreateFolder, returns: "string",
        signature: &[("dir", "string", false), ("name", "string", true)],
        run: create_folder,
    },
    command! {
        name: "rename_file", scope: Files, effect: Write,
        change: File / Updated,
        id: |a: &RenameFile| Some(a.id.to_string()),
        args: RenameFile, returns: "FileEntry",
        signature: &[("id", "string", true), ("name", "string", true)],
        run: rename_file,
    },
    command! {
        name: "move_files", scope: Files, effect: Write,
        change: File / Updated,
        ids: |a: &MoveFiles| a.ids.iter().map(ToString::to_string).collect(),
        args: MoveFiles, returns: "FileEntry[]",
        signature: &[("ids", "string[]", true), ("dir", "string", false)],
        run: move_files,
    },
    command! {
        name: "rename_folder", scope: Files, effect: Write,
        change: File / Updated,
        args: RenameFolder, returns: "string",
        signature: &[("path", "string", true), ("name", "string", true)],
        run: rename_folder,
    },
    command! {
        name: "move_folder", scope: Files, effect: Write,
        change: File / Updated,
        args: MoveFolder, returns: "string",
        signature: &[("path", "string", true), ("into", "string", false)],
        run: move_folder,
    },
    command! {
        name: "update_files", scope: Files, effect: Write,
        change: File / Updated,
        ids: |a: &UpdateFiles| a.ids.iter().map(ToString::to_string).collect(),
        args: UpdateFiles, returns: "FileEntry[]",
        signature: &[("ids", "string[]", true), ("update", "FileUpdate", true)],
        run: update_files,
    },
    command! {
        name: "stack_files", scope: Files, effect: Write,
        change: File / Updated,
        args: StackFiles, returns: "void",
        signature: &[("top", "string", true), ("others", "string[]", true)],
        run: stack_files,
    },
    command! {
        name: "unstack_files", scope: Files, effect: Write,
        change: File / Updated,
        args: FileIds, returns: "void",
        signature: &[("ids", "string[]", true)],
        run: unstack_files,
    },
    command! {
        name: "trash_files", scope: Files, effect: Write,
        change: File / Updated,
        ids: |a: &FileIds| a.ids.iter().map(ToString::to_string).collect(),
        args: FileIds, returns: "FileEntry[]",
        signature: &[("ids", "string[]", true)],
        run: trash_files,
    },
    command! {
        name: "trash_folder", scope: Files, effect: Write,
        change: File / Updated,
        args: FolderRef, returns: "number",
        signature: &[("path", "string", true)],
        run: trash_folder,
    },
    command! {
        name: "restore_files", scope: Files, effect: Write,
        change: File / Updated,
        ids: |a: &FileIds| a.ids.iter().map(ToString::to_string).collect(),
        args: FileIds, returns: "FileEntry[]",
        signature: &[("ids", "string[]", true)],
        run: restore_files,
    },
    command! {
        name: "delete_files_forever", scope: Files, effect: Destructive,
        change: File / Deleted,
        ids: |a: &FileIds| a.ids.iter().map(ToString::to_string).collect(),
        args: FileIds, returns: "number",
        signature: &[("ids", "string[]", true)],
        run: delete_files_forever,
    },
    command! {
        name: "empty_files_trash", scope: Files, effect: Destructive,
        change: File / Deleted,
        args: Nothing, returns: "number", signature: &[],
        run: empty_files_trash,
    },
    command! {
        name: "forget_missing_files", scope: Files, effect: Destructive,
        change: File / Deleted,
        ids: |a: &FileIds| a.ids.iter().map(ToString::to_string).collect(),
        args: FileIds, returns: "number",
        signature: &[("ids", "string[]", true)],
        run: forget_missing_files,
    },
    command! {
        name: "set_file_encrypted", scope: Files, effect: Write,
        change: File / Updated,
        id: |a: &SetEncrypted| Some(a.id.to_string()),
        args: SetEncrypted, returns: "FileEntry",
        signature: &[("id", "string", true), ("encrypted", "boolean", true)],
        run: set_file_encrypted,
    },
    command! {
        name: "set_file_thumbnail", scope: Files, effect: Write,
        change: File / Updated,
        id: |a: &SetThumbnail| Some(a.id.to_string()),
        args: SetThumbnail, returns: "FileEntry",
        signature: &[("id", "string", true), ("image", "string", true)],
        run: set_file_thumbnail,
    },
    command! {
        name: "rescan_files", scope: Files, effect: Write,
        change: File / Updated,
        args: Nothing, returns: "ScanReport", signature: &[],
        run: rescan_files,
    },
    command! {
        name: "list_albums", scope: Files, effect: Read,
        args: Nothing, returns: "AlbumView[]", signature: &[],
        run: list_albums,
    },
    command! {
        name: "save_album", scope: Files, effect: Write,
        change: Album / Updated,
        id: |a: &SaveAlbum| Some(a.album.id.to_string()),
        args: SaveAlbum, returns: "Album",
        signature: &[("album", "Album", true)],
        run: save_album,
    },
    command! {
        name: "delete_album", scope: Files, effect: Destructive,
        change: Album / Deleted,
        id: |a: &AlbumRef| Some(a.id.to_string()),
        args: AlbumRef, returns: "void",
        signature: &[("id", "string", true)],
        run: delete_album,
    },
    command! {
        name: "add_to_album", scope: Files, effect: Write,
        change: Album / Updated,
        id: |a: &AlbumFiles| Some(a.id.to_string()),
        args: AlbumFiles, returns: "void",
        signature: &[("id", "string", true), ("files", "string[]", true)],
        run: add_to_album,
    },
    command! {
        name: "remove_from_album", scope: Files, effect: Write,
        change: Album / Updated,
        id: |a: &AlbumFiles| Some(a.id.to_string()),
        args: AlbumFiles, returns: "void",
        signature: &[("id", "string", true), ("files", "string[]", true)],
        run: remove_from_album,
    },
    command! {
        name: "bring_in_attachments", scope: Files, effect: Write,
        change: File / Created,
        args: Nothing, returns: "LegacyReport", signature: &[],
        run: bring_in_attachments,
    },
    command! {
        name: "save_mail_part_to_files", scope: Files, effect: Write,
        change: File / Created,
        args: SaveMailPart, returns: "FileEntry",
        signature: &[
            ("messageId", "string", true),
            ("part", "string", true),
            ("dir", "string", false),
        ],
        run: save_mail_part_to_files,
    },
    command! {
        name: "restore_file_from_backup", scope: Files, effect: Write,
        change: File / Updated,
        id: |a: &FileRef| Some(a.id.to_string()),
        args: FileRef, returns: "FileEntry",
        signature: &[("id", "string", true)],
        run: restore_file_from_backup,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Caller;

    fn caller(caller: Caller) -> Ctx {
        Ctx { caller, scopes: vec![Scope::Files], proved_at: None, request_id: None }
    }

    #[test]
    fn only_the_shell_on_this_machine_is_local() {
        assert!(Ctx::local().is_local());
        assert!(Ctx::window("app-files").is_local());
        for other in
            [Caller::Socket, Caller::Device("phone".into()), Caller::Assistant("run-1".into())]
        {
            assert!(!caller(other.clone()).is_local(), "{other:?}");
        }
    }

    #[test]
    fn a_name_a_sender_chose_is_made_safe_to_keep() {
        assert_eq!(attachment_name(Some("report.pdf")), "report.pdf");
        assert_eq!(attachment_name(Some("../../etc/passwd")), "passwd");
        assert_eq!(attachment_name(Some(r"C:\Users\me\tax.xlsx")), "tax.xlsx");
        // A leading dot would be left out of every listing of the folder.
        assert_eq!(attachment_name(Some(".bashrc")), "bashrc");
        assert_eq!(attachment_name(Some("line\nbreak.txt")), "line_break.txt");
        assert_eq!(attachment_name(Some("   ")), "attachment");
        assert_eq!(attachment_name(None), "attachment");
        // Over long: cut, with the extension kept, on a character boundary.
        let long = format!("{}.pdf", "\u{e9}".repeat(300));
        let name = attachment_name(Some(&long));
        assert!(name.len() <= NAME_BUDGET, "{}", name.len());
        assert!(name.ends_with(".pdf"));
    }

    #[test]
    fn a_thumbnail_arrives_as_base64_with_or_without_its_data_url_head() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"not really a picture");
        assert_eq!(decode_image(&encoded).unwrap(), b"not really a picture");
        let url = format!("data:image/png;base64,{encoded}");
        assert_eq!(decode_image(&url).unwrap(), b"not really a picture");
        assert_eq!(decode_image("not base64 !!").unwrap_err().code, codes::INVALID);
        let huge = "A".repeat(files::thumb::MAX_UPLOADED * 2);
        assert_eq!(decode_image(&huge).unwrap_err().code, codes::TOO_LARGE);
    }
}
