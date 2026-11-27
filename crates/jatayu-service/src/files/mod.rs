//! Files: what the service adds to [`jatayu_core::files`] -- whatever
//! needs more than the standard library.
//!
//! - [`meta`]: what a photo, a film or a PDF says about itself (EXIF, a
//!   movie header), read once when a file arrives;
//! - [`thumb`]: a small JPEG of a picture, for grids;
//! - [`task`]: the background task that keeps the catalog in step with the
//!   files folder -- a pass at unlock, one every few minutes, one soon after
//!   [`watch`] notices a change -- and empties the trash of what has been in
//!   it too long;
//! - [`watch`]: noticing changes to the folder as they happen.
//!
//! The commands are in `crate::domains::files`.

pub mod meta;
pub mod task;
pub mod thumb;
pub mod watch;

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jatayu_core::Vault;
use jatayu_core::files::{FileKind, FileStorage, FileUpdate, GeoPoint};
use jatayu_core::id::FileId;

use crate::events::{Change, Kind, Op};
use crate::service::Service;

/// Sealed files bigger than this are not opened for what they say about
/// themselves: reading one means decrypting all of it into memory, for a
/// film's duration and a thumbnail nobody can draw of a film anyway.
const MAX_SEALED_PROBE: u64 = 256 * 1024 * 1024;

/// How much of a plain file's start is read to tell what it really is: a
/// `.jpg` that is a HEIC says so in the first box of the container.
const SNIFF_BYTES: u64 = 512;

/// How many files are probed before the interface is told, so a long import
/// fills the grid as it goes instead of all at once at the end, and a short
/// one is a single reload.
const ANNOUNCE_EVERY: usize = 16;

/// One probe at a time, across every caller: the reconciler's pass, an
/// import, a legacy migration. Reading a picture and drawing its thumbnail
/// is the heaviest thing this crate does on the blocking pool, and several
/// at once would only fight for the disk and for the memory a large picture
/// is decoded into.
static PROBE_TURN: Mutex<()> = Mutex::new(());

/// Background probe jobs that have not finished. See [`settled`].
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Read a file's metadata and draw its thumbnail, and store both
/// ([`Vault::save_file_derived`]). Blocking: call from `spawn_blocking`.
///
/// What every path that adds a file runs afterwards: an import, an upload,
/// a new file the reconciler found. A file nothing can be read from is not
/// an error -- it keeps the metadata it had.
pub fn probe_and_store(vault: &Arc<Vault>, id: FileId) -> jatayu_core::Result<()> {
    let entry = vault.file(id)?;
    // A trashed file is in the trash folder under a name of its own; what it
    // says about itself was read when it arrived, and will be again if it is
    // restored and changes.
    if entry.status.is_trashed() {
        return Ok(());
    }
    let _turn = PROBE_TURN.lock().unwrap_or_else(|e| e.into_inner());

    let (sniffed, media, thumbnail) = match &entry.storage {
        FileStorage::Plain { .. } => {
            let path = match vault.plain_path(id) {
                Ok(path) => path,
                // Not there, or the folder is not: the next pass that finds
                // it will ask again.
                Err(e) => {
                    tracing::debug!(%id, error = %e, "files: not reading a file that cannot be reached");
                    return Ok(());
                }
            };
            let sniffed = better_mime(&entry.mime, meta::sniff(&head_of(&path)));
            let mime = sniffed.unwrap_or(entry.mime.as_str());
            let media = meta::probe(&path, mime);
            // A film's poster is drawn by the interface, and a document has
            // none; only a picture is drawn here, and only when its type says
            // it is one.
            let thumbnail = (FileKind::classify(mime, &entry.name) == FileKind::Image)
                .then(|| thumb::make(&path, mime, media.orientation))
                .flatten();
            (sniffed, media, thumbnail)
        }
        FileStorage::Sealed { .. } => {
            if entry.size > MAX_SEALED_PROBE {
                tracing::debug!(%id, size = entry.size, "files: a sealed file too large to open for its metadata");
                return Ok(());
            }
            let bytes = vault.read_file_range(id, 0, entry.size)?;
            let sniffed = better_mime(
                &entry.mime,
                meta::sniff(&bytes[..bytes.len().min(SNIFF_BYTES as usize)]),
            );
            let mime = sniffed.unwrap_or(entry.mime.as_str());
            let media = meta::probe_bytes(&bytes, mime);
            let thumbnail = (FileKind::classify(mime, &entry.name) == FileKind::Image)
                .then(|| thumb::make_from_bytes(&bytes, mime, media.orientation))
                .flatten();
            (sniffed, media, thumbnail)
        }
    };

    let mime = sniffed;
    // Nothing new learned: no write, so a record nobody changed is not made
    // to look changed to a sync tool, a backup or a window.
    if entry.place.is_none()
        && let Some(at) = media.gps
    {
        name_place_later(vault, id, at);
    }
    if media == entry.media && thumbnail.is_none() && mime.is_none() {
        return Ok(());
    }
    vault.save_file_derived(id, media, thumbnail.as_deref(), mime)?;
    Ok(())
}

/// When the person allowed it ([`FilesSettings::name_places`]), ask
/// OpenStreetMap what the place at `at` is called and give the file that
/// name -- in the background, after the probe, so a hundred photos from a
/// trip are not held one Nominatim second each before their thumbnails.
///
/// Names are remembered for the process by their spot to about a hundred
/// metres, so those hundred photos are one question, not a hundred. A file
/// somebody named in the meantime keeps their name. Nothing is asked
/// outside a runtime (a test calling [`probe_and_store`] directly).
///
/// [`FilesSettings::name_places`]: jatayu_core::files::FilesSettings::name_places
fn name_place_later(vault: &Arc<Vault>, id: FileId, at: GeoPoint) {
    if !vault.files_settings().is_ok_and(|s| s.name_places) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let vault = vault.clone();
    runtime.spawn(async move {
        let spot = ((at.lat * 1000.0).round() as i64, (at.lon * 1000.0).round() as i64);
        let known = place_names().get(&spot).cloned();
        let name = match known {
            Some(name) => name,
            None => {
                let point = jatayu_core::maps::Point { latitude: at.lat, longitude: at.lon };
                match crate::maps::reverse(point).await {
                    Ok(name) => {
                        place_names().insert(spot, name.clone());
                        name
                    }
                    Err(e) => {
                        tracing::debug!(%id, error = %e, "files: could not name a photo's place");
                        return;
                    }
                }
            }
        };
        let Some(name) = name else { return };
        let _ = crate::service::blocking(move || {
            if vault.file(id)?.place.is_none() {
                let update = FileUpdate { place: Some(name), ..FileUpdate::default() };
                vault.update_files(&[id], &update)?;
            }
            Ok(())
        })
        .await;
    });
}

/// Place names already asked for, by spot (thousandths of a degree); `None`
/// where there was nothing to name.
type PlaceNames = HashMap<(i64, i64), Option<String>>;

fn place_names() -> std::sync::MutexGuard<'static, PlaceNames> {
    static NAMES: std::sync::OnceLock<Mutex<PlaceNames>> = std::sync::OnceLock::new();
    NAMES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The first bytes of `path`, or none when it cannot be opened.
fn head_of(path: &Path) -> Vec<u8> {
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(SNIFF_BYTES).read_to_end(&mut head);
    }
    head
}

/// Read and draw `ids` in the background, one at a time, and tell the
/// interface as they land. Returns at once.
///
/// What every command that adds a file calls instead of
/// [`probe_and_store`] itself: answering an import of two hundred pictures
/// has to wait for the copying, never for two hundred thumbnails. The
/// announcement carries no origin, so the window that made the import
/// reloads for it too -- the thumbnails are news to it as much as to any
/// other.
///
/// Stops quietly when the vault locks or another is opened, and holds
/// nothing past that: a job that kept an old vault alive would keep its
/// write lock.
pub(crate) fn probe_later(svc: &Arc<Service>, vault: &Arc<Vault>, ids: Vec<FileId>) {
    if ids.is_empty() {
        return;
    }
    let flight = Flight::take();
    let (svc, vault) = (svc.clone(), vault.clone());
    tokio::spawn(async move {
        let _flight = flight;
        let mut landed: Vec<FileId> = Vec::new();
        for id in ids {
            if !still_open(&svc, &vault) {
                break;
            }
            let probing = vault.clone();
            match tokio::task::spawn_blocking(move || probe_and_store(&probing, id)).await {
                Ok(Ok(())) => landed.push(id),
                Ok(Err(e)) => tracing::debug!(%id, error = %e, "files: could not read a new file"),
                Err(e) => tracing::warn!(%id, error = %e, "files: reading a new file failed"),
            }
            if landed.len() >= ANNOUNCE_EVERY {
                announce(&svc, std::mem::take(&mut landed));
            }
        }
        announce(&svc, landed);
    });
}

/// Whether `vault` is still the service's open vault, and unlocked.
pub(crate) fn still_open(svc: &Service, vault: &Arc<Vault>) -> bool {
    vault.is_unlocked() && svc.get().is_some_and(|open| Arc::ptr_eq(&open, vault))
}

/// Say that these files changed: one id as `id`, several as `ids`, none as
/// nothing -- the shape a command's own `change:` row has.
pub(crate) fn announce(svc: &Service, ids: Vec<FileId>) {
    let (id, ids) = match ids.len() {
        0 => return,
        1 => (Some(ids[0].to_string()), Vec::new()),
        _ => (None, ids.iter().map(ToString::to_string).collect()),
    };
    svc.events().changed(Change { kind: Kind::File, op: Op::Updated, id, ids, origin: None });
}

/// A running background job, counted from the moment it is started to the
/// moment it ends -- by whatever route, a panic included.
struct Flight;

impl Flight {
    fn take() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Flight
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Resolves once no background probe ([`probe_later`]) is running.
///
/// For tests, which must see what a command announces and what its probe
/// announces after it in an order that does not depend on the scheduler.
/// Nothing in the application waits on it.
pub async fn settled() {
    while IN_FLIGHT.load(Ordering::SeqCst) > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The type the first bytes say, when it is worth more than the one the
/// catalog has: when the catalog's says nothing (`application/octet-stream`,
/// or empty), names another kind of thing altogether, or names one picture
/// format for another (a `.jpg` that is really a HEIC). Never a container's generic type over a specific one --
/// a `.docx` is a ZIP and a `.dng` a TIFF to their first bytes, and both
/// are better known by their names.
fn better_mime(catalog: &str, sniffed: Option<&'static str>) -> Option<&'static str> {
    let sniffed = sniffed.filter(|m| *m != catalog)?;
    if catalog.is_empty() || catalog == "application/octet-stream" {
        return Some(sniffed);
    }
    if matches!(sniffed, "application/zip" | "image/tiff" | "application/xml" | "text/plain") {
        return None;
    }
    let top = |m: &str| m.split('/').next().unwrap_or_default().to_string();
    // One picture format named as another is worth correcting: a HEIC saved
    // as `.jpg` is drawn and read differently from a JPEG.
    (top(catalog) != top(sniffed) || top(sniffed) == "image").then_some(sniffed)
}

#[cfg(test)]
mod tests {
    use super::better_mime;

    #[test]
    fn a_sniffed_type_replaces_only_a_vague_or_a_wrong_one() {
        // Nothing known: whatever the bytes say.
        assert_eq!(better_mime("application/octet-stream", Some("image/heic")), Some("image/heic"));
        assert_eq!(better_mime("", Some("application/pdf")), Some("application/pdf"));
        // One picture format for another, or another kind of thing.
        assert_eq!(better_mime("image/jpeg", Some("image/heic")), Some("image/heic"));
        assert_eq!(better_mime("video/quicktime", Some("image/jpeg")), Some("image/jpeg"));
        // A container's own type never wins over a name that says more.
        let docx = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
        assert_eq!(better_mime(docx, Some("application/zip")), None);
        assert_eq!(better_mime("image/x-adobe-dng", Some("image/tiff")), None);
        // The same, or nothing sniffed: nothing to change.
        assert_eq!(better_mime("image/png", Some("image/png")), None);
        assert_eq!(better_mime("image/png", None), None);
    }
}
