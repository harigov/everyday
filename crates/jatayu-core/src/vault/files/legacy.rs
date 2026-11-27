//! Pictures and files kept the old way, as blobs inside entries and notes, and carrying them
//! into the folder.

use super::Vault;
use super::helpers::{from_micros, new_entry, set_plain_path, stamp_mtime, validate_file_name};
use super::{HEAD_LEN, MAX_DEPTH};
use crate::error::{Error, Result};
use crate::files::disk::{self, Folder};
use crate::files::{
    FileOrigin, FileStorage, LegacyReport, LegacySummary, MediaInfo, RefKind, RefOwner,
};
use crate::id::{BlobId, ConversationId, EntryId, FileId, NoteId};
use crate::model::{Attachment, MediaKind};
use crate::note::Note;
use crate::record::RecordKind;
use crate::richtext::{MEDIA_NODE, RichDoc};
use crate::store::JournalStore;
use crate::store::agent::ConversationQuery;
use jiff::Timestamp;
use jiff::civil;
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

impl Vault {
    /// How many pictures and files are kept as blobs inside entries, notes
    /// and assistant messages.
    pub fn legacy_attachments(&self) -> Result<LegacySummary> {
        let scan = self.read(|u| scan_legacy(u.store.as_ref()))?;
        Ok(LegacySummary {
            entries: scan.entries.len() as u32,
            notes: scan.notes.len() as u32,
            messages: scan.messages,
            files: scan.blobs.len() as u32,
            bytes: scan.blobs.values().map(|b| b.len).sum(),
        })
    }

    /// Bring them into the files folder: each blob written out once (under
    /// its origin's folder, by the date of the record it was found in), a
    /// file record for it, and every record that held the blob rewritten to
    /// refer to the file instead. The blobs are left for the garbage
    /// collector.
    ///
    /// # How a record is rewritten
    ///
    /// - A **media node** that named a blob names the file instead:
    ///   `attrs.blob` becomes `attrs.file`, and the kind, caption and
    ///   filename stay.
    /// - An **attachment** of an entry or a note is metadata about a blob
    ///   the body shows, and nothing in the interface draws it but the list
    ///   row's count and cover. It is dropped from the list once its blob is
    ///   a file -- left, it would keep the blob alive for ever and the bytes
    ///   would be kept twice -- and its caption goes into the media node.
    ///   One that the body never showed (the command line's `attach` adds
    ///   such) gets a media node at the end of the body, so the picture the
    ///   person attached is still on the page and the file still has
    ///   something that refers to it.
    /// - An **assistant message** keeps its attachments, which are the
    ///   copies a model is shown, and gains the file in `files`. A message
    ///   that has files already is taken to have been brought in, so a
    ///   second run does not write its pictures again.
    ///
    /// A blob that cannot be brought in is reported and left where it is,
    /// with every record that holds it. Run again, only that is retried.
    /// Records are saved as they were, with their times untouched.
    pub fn bring_in_attachments(&self) -> Result<LegacyReport> {
        self.writable()?;
        let scan = self.read(|u| scan_legacy(u.store.as_ref()))?;
        let mut report = LegacyReport::default();
        if scan.absent > 0 {
            report.errors.push(format!(
                "{} pictures or files are named by records but are no longer in the vault",
                scan.absent
            ));
        }
        if scan.blobs.is_empty() {
            return Ok(report);
        }

        // 1. The blobs, one file each.
        let settings = self.files_settings()?;
        let folder = if scan.blobs.values().any(|b| !settings.encrypted.contains(&b.origin)) {
            Some(self.usable_folder()?)
        } else {
            None
        };
        let mut brought: BTreeMap<BlobId, Brought> = BTreeMap::new();
        for (blob, held) in &scan.blobs {
            let sealed = settings.encrypted.contains(&held.origin);
            match self.bring_blob(folder.as_ref().filter(|_| !sealed), *blob, held) {
                Ok((size, b)) => {
                    report.files += 1;
                    report.bytes += size;
                    brought.insert(*blob, b);
                }
                Err(e) => report.errors.push(format!("{}: {e}", short_blob(*blob))),
            }
        }
        if brought.is_empty() {
            return Ok(report);
        }

        // 2. The records, each read again, so a change since the scan is
        //    not overwritten, and written as it was found.
        for id in &scan.entries {
            let rewritten = self.write_in(&self.gates.text, |u| {
                let mut entry = match u.store.get_entry(*id) {
                    Ok(e) => e,
                    Err(Error::NotFound { .. }) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if !rewrite_record(&mut entry.body, &mut entry.attachments, &brought) {
                    return Ok(None);
                }
                u.store.put_entry(&entry)?;
                u.index.put_entry(&entry);
                Ok(Some(entry.body.file_refs()))
            });
            match rewritten {
                Ok(Some(refs)) => {
                    report.entries += 1;
                    self.track_file_refs(RefOwner::new(RefKind::Entry, id), refs);
                    self.wrote(RecordKind::Entry, id);
                }
                Ok(None) => {}
                Err(e) => report.errors.push(format!("entry {id}: {e}")),
            }
        }
        for id in &scan.notes {
            let rewritten = self.write_in(&self.gates.text, |u| {
                let notes = u.notes()?;
                let mut note: Note = match notes.get_note(*id) {
                    Ok(n) => n,
                    Err(Error::NotFound { .. }) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if !rewrite_record(&mut note.body, &mut note.attachments, &brought) {
                    return Ok(None);
                }
                notes.put_note(&note)?;
                super::super::meetings::reindex_note(u, note.id);
                Ok(Some(note.body.file_refs()))
            });
            match rewritten {
                Ok(Some(refs)) => {
                    report.notes += 1;
                    self.track_file_refs(RefOwner::new(RefKind::Note, id), refs);
                    self.wrote(RecordKind::Note, id);
                }
                Ok(None) => {}
                Err(e) => report.errors.push(format!("note {id}: {e}")),
            }
        }
        for id in &scan.conversations {
            let rewritten = self.write(|u| {
                let agent = u.agent()?;
                let mut changed = Vec::new();
                for mut message in agent.list_messages(*id)? {
                    if rewrite_message(&mut message, &brought) {
                        agent.put_message(&message)?;
                        changed.push((message.id, message.files));
                    }
                }
                Ok(changed)
            });
            match rewritten {
                Ok(changed) => {
                    for (message, files) in changed {
                        report.messages += 1;
                        self.track_file_refs(RefOwner::new(RefKind::Message, message), files);
                        self.wrote(RecordKind::Message, message);
                    }
                }
                Err(e) => report.errors.push(format!("conversation {id}: {e}")),
            }
        }
        Ok(report)
    }

    /// One legacy blob into the catalog: written into the folder under its
    /// origin's folder (or kept sealed, when `folder` is `None`), dated by
    /// the record it was found in. Answers its size and how records are to
    /// name it.
    fn bring_blob(
        &self,
        folder: Option<&Folder>,
        blob: BlobId,
        held: &LegacyBlob,
    ) -> Result<(u64, Brought)> {
        let mut mime = held.meta.mime.trim().to_string();
        if mime.is_empty() || mime == "application/octet-stream" {
            let head = self.read(|u| u.store.get_blob_range(blob, 0, HEAD_LEN))?;
            mime = disk::guess_mime(&held.meta.filename, &head);
        }
        let name = legacy_name(&held.meta.filename, blob, &mime);
        let now = Timestamp::now();
        let id = FileId::new();
        let (storage, size, hash, modified_at) = match folder {
            None => (FileStorage::Sealed { blob }, held.len, String::new(), held.at),
            Some(folder) => {
                let dir = held.origin.default_dir(held.date);
                folder.ensure_dir(&dir)?;
                let written = self.blob_to_folder(folder, blob, &dir, &name)?;
                // A film or a picture is dated by the record it was put in,
                // not by the day it was copied out: the timeline files it
                // under its file's time until its own metadata is read.
                let modified_us =
                    stamp_mtime(folder, &written.path, held.at).unwrap_or(written.modified_us);
                (
                    FileStorage::Plain { path: written.path },
                    written.size,
                    written.hash,
                    from_micros(modified_us),
                )
            }
        };
        let mut entry = new_entry(id, held.origin, &name, mime.clone(), now);
        entry.storage = storage;
        entry.size = size;
        entry.hash = hash;
        entry.modified_at = modified_at;
        entry.caption.clone_from(&held.meta.caption);
        entry.media = MediaInfo {
            width: held.meta.width,
            height: held.meta.height,
            duration_ms: held.meta.duration_ms,
            ..Default::default()
        };
        if let Some(path) = entry.path().map(str::to_string) {
            set_plain_path(&mut entry, path);
        }
        if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
            if let (Some(folder), Some(path)) = (folder, entry.path()) {
                let _ = folder.remove(path);
            }
            return Err(e);
        }
        self.wrote(RecordKind::File, id);
        let brought = Brought {
            id,
            name: entry.name.clone(),
            mime,
            caption: held.meta.caption.clone(),
            width: held.meta.width,
            height: held.meta.height,
        };
        Ok((size, brought))
    }
}

/// What is known about a blob from the record that held it.
#[derive(Debug, Clone, Default)]
pub(super) struct Meta {
    mime: String,
    filename: String,
    caption: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
}

/// A blob an old-style record holds.
pub(super) struct Held {
    blob: BlobId,
    meta: Meta,
}

/// One distinct blob found by the scan: where it will go, by the first
/// record that held it.
pub(super) struct LegacyBlob {
    origin: FileOrigin,
    /// The record's time, which the file takes as its own.
    at: Timestamp,
    /// The record's day, in this machine's time zone, for the folder.
    date: civil::Date,
    meta: Meta,
    len: u64,
}

/// Every old-style record, found.
#[derive(Default)]
pub(super) struct LegacyScan {
    blobs: BTreeMap<BlobId, LegacyBlob>,
    entries: Vec<EntryId>,
    notes: Vec<NoteId>,
    conversations: Vec<ConversationId>,
    /// Messages holding a blob that is there.
    messages: u32,
    /// Blobs records name that the vault no longer holds.
    absent: u32,
    gone: BTreeSet<BlobId>,
}

impl LegacyScan {
    /// Note what a record holds. Answers whether it held anything that is
    /// still in the vault.
    fn note(
        &mut self,
        store: &dyn JournalStore,
        held: Vec<Held>,
        origin: FileOrigin,
        at: Timestamp,
        date: civil::Date,
    ) -> bool {
        let mut any = false;
        for h in held {
            if self.gone.contains(&h.blob) {
                continue;
            }
            if let Some(known) = self.blobs.get_mut(&h.blob) {
                any = true;
                // What the first record did not say, a later one may.
                if known.meta.filename.is_empty() {
                    known.meta.filename = h.meta.filename;
                }
                if known.meta.mime.is_empty() {
                    known.meta.mime = h.meta.mime;
                }
                if known.meta.caption.is_empty() {
                    known.meta.caption = h.meta.caption;
                }
                continue;
            }
            let Ok(len) = store.blob_len(h.blob) else {
                self.gone.insert(h.blob);
                self.absent += 1;
                continue;
            };
            any = true;
            self.blobs.insert(h.blob, LegacyBlob { origin, at, date, meta: h.meta, len });
        }
        any
    }
}

/// Walk every entry, note and assistant message for blobs. Under whatever
/// lock the caller holds: this reads, and writes nothing.
pub(super) fn scan_legacy(store: &dyn JournalStore) -> Result<LegacyScan> {
    let zone = TimeZone::system();
    let mut scan = LegacyScan::default();
    for entry in store.all_entries()? {
        let held = held_in(&entry.body, &entry.attachments);
        if !held.is_empty()
            && scan.note(store, held, FileOrigin::Journal, entry.created_at, entry.local_date)
        {
            scan.entries.push(entry.id);
        }
    }
    if let Some(notes) = store.notes() {
        for note in notes.all_notes()? {
            let held = held_in(&note.body, &note.attachments);
            let date = note.created_at.to_zoned(zone.clone()).date();
            if !held.is_empty() && scan.note(store, held, FileOrigin::Notes, note.created_at, date)
            {
                scan.notes.push(note.id);
            }
        }
    }
    if let Some(agent) = store.agent() {
        for conversation in agent.list_conversations(&ConversationQuery::default())? {
            let origin = if conversation.run_id.is_some() || conversation.job_id.is_some() {
                FileOrigin::Jobs
            } else if conversation.via.is_some() {
                FileOrigin::Chat
            } else {
                FileOrigin::Assistant
            };
            let mut any = false;
            for message in agent.list_messages(conversation.id)? {
                // A message that already has files has been brought in (or
                // is one written since files existed): its attachments are
                // the copies a model is shown, not pictures waiting to move.
                if message.attachments.is_empty() || !message.files.is_empty() {
                    continue;
                }
                let held: Vec<Held> = message
                    .attachments
                    .iter()
                    .map(|a| Held { blob: a.blob, meta: meta_of(a) })
                    .collect();
                let date = message.created_at.to_zoned(zone.clone()).date();
                if scan.note(store, held, origin, message.created_at, date) {
                    scan.messages += 1;
                    any = true;
                }
            }
            if any {
                scan.conversations.push(conversation.id);
            }
        }
    }
    Ok(scan)
}

pub(super) fn meta_of(a: &Attachment) -> Meta {
    Meta {
        mime: a.mime.clone(),
        filename: a.filename.clone(),
        caption: a.caption.clone(),
        width: a.width,
        height: a.height,
        duration_ms: a.duration_ms,
    }
}

/// Every blob a body's media nodes and an attachment list name, each once,
/// with what is known of it: the attachment's account first, the node's for
/// a blob the list does not have.
pub(super) fn held_in(body: &RichDoc, attachments: &[Attachment]) -> Vec<Held> {
    let mut out: Vec<Held> = Vec::new();
    for a in attachments {
        if !out.iter().any(|h| h.blob == a.blob) {
            out.push(Held { blob: a.blob, meta: meta_of(a) });
        }
    }
    visit_media(&body.0, 0, &mut |attrs| {
        let Some(blob) = blob_of(attrs) else { return };
        let text =
            |key: &str| attrs.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let number =
            |key: &str| attrs.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        match out.iter_mut().find(|h| h.blob == blob) {
            Some(h) => {
                if h.meta.caption.is_empty() {
                    h.meta.caption = text("caption");
                }
            }
            None => out.push(Held {
                blob,
                meta: Meta {
                    mime: text("mime"),
                    filename: text("filename"),
                    caption: text("caption"),
                    width: number("width"),
                    height: number("height"),
                    duration_ms: None,
                },
            }),
        }
    });
    out
}

/// The blob a media node's attributes name, if they name one.
pub(super) fn blob_of(attrs: &Map<String, Value>) -> Option<BlobId> {
    attrs.get("blob").and_then(Value::as_str).and_then(|s| BlobId::parse(s).ok())
}

pub(super) fn visit_media(node: &Value, depth: usize, f: &mut impl FnMut(&Map<String, Value>)) {
    if depth > MAX_DEPTH {
        return;
    }
    if node.get("type").and_then(Value::as_str) == Some(MEDIA_NODE)
        && let Some(attrs) = node.get("attrs").and_then(Value::as_object)
    {
        f(attrs);
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            visit_media(child, depth + 1, f);
        }
    }
}

/// How records are to name a blob that is now a file.
pub(super) struct Brought {
    id: FileId,
    name: String,
    mime: String,
    caption: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// The name a legacy blob's file gets: the attachment's own filename when it
/// is one the folder can keep, else `picture-<short>.<ext>`.
pub(super) fn legacy_name(filename: &str, blob: BlobId, mime: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if let Ok(name) = validate_file_name(base) {
        return name;
    }
    let ext = match mime.split(';').next().unwrap_or_default().trim() {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "image/avif" => ".avif",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "audio/mpeg" => ".mp3",
        "audio/mp4" => ".m4a",
        "audio/wav" => ".wav",
        "audio/ogg" => ".ogg",
        "application/pdf" => ".pdf",
        m if m.starts_with("image/") => ".jpg",
        _ => ".bin",
    };
    format!("picture-{}{ext}", &blob.to_hex()[..8])
}

pub(super) fn short_blob(blob: BlobId) -> String {
    format!("a stored file ({})", &blob.to_hex()[..8])
}

pub(super) fn media_word(mime: &str) -> &'static str {
    match MediaKind::from_mime(mime) {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::File => "file",
    }
}

/// Rewrite a body's media nodes, and the attachment list that goes with it,
/// for blobs that are now files. Answers whether anything changed. See
/// [`Vault::bring_in_attachments`] for what each part becomes.
pub(super) fn rewrite_record(
    body: &mut RichDoc,
    attachments: &mut Vec<Attachment>,
    brought: &BTreeMap<BlobId, Brought>,
) -> bool {
    let captions: BTreeMap<BlobId, String> = attachments
        .iter()
        .filter(|a| !a.caption.is_empty())
        .map(|a| (a.blob, a.caption.clone()))
        .collect();
    let mut shown = BTreeSet::new();
    let mut changed = rewrite_nodes(&mut body.0, 0, brought, &captions, &mut shown);
    let mut kept = Vec::with_capacity(attachments.len());
    for attachment in attachments.drain(..) {
        match brought.get(&attachment.blob) {
            Some(b) => {
                // One the body never showed is shown now, at the end.
                if shown.insert(attachment.blob) {
                    append_media_node(&mut body.0, b, &attachment.caption);
                }
                changed = true;
            }
            None => kept.push(attachment),
        }
    }
    *attachments = kept;
    changed
}

pub(super) fn rewrite_nodes(
    node: &mut Value,
    depth: usize,
    brought: &BTreeMap<BlobId, Brought>,
    captions: &BTreeMap<BlobId, String>,
    shown: &mut BTreeSet<BlobId>,
) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let mut changed = false;
    if node.get("type").and_then(Value::as_str) == Some(MEDIA_NODE)
        && let Some(attrs) = node.get_mut("attrs").and_then(Value::as_object_mut)
        && let Some(blob) = blob_of(attrs)
        && let Some(b) = brought.get(&blob)
    {
        attrs.remove("blob");
        attrs.insert("file".into(), json!(b.id.to_string()));
        let has_caption =
            attrs.get("caption").and_then(Value::as_str).is_some_and(|c| !c.is_empty());
        if !has_caption && let Some(caption) = captions.get(&blob) {
            attrs.insert("caption".into(), json!(caption));
        }
        shown.insert(blob);
        changed = true;
    }
    if let Some(children) = node.get_mut("content").and_then(Value::as_array_mut) {
        for child in children {
            changed |= rewrite_nodes(child, depth + 1, brought, captions, shown);
        }
    }
    changed
}

pub(super) fn append_media_node(doc: &mut Value, b: &Brought, caption: &str) {
    let mut attrs = Map::new();
    attrs.insert("file".into(), json!(b.id.to_string()));
    attrs.insert("kind".into(), json!(media_word(&b.mime)));
    attrs.insert("mime".into(), json!(b.mime));
    attrs.insert("filename".into(), json!(b.name));
    let caption = if caption.is_empty() { b.caption.as_str() } else { caption };
    attrs.insert("caption".into(), json!(caption));
    if let Some(width) = b.width {
        attrs.insert("width".into(), json!(width));
    }
    if let Some(height) = b.height {
        attrs.insert("height".into(), json!(height));
    }
    if let Some(content) = doc.get_mut("content").and_then(Value::as_array_mut) {
        content.push(json!({ "type": MEDIA_NODE, "attrs": Value::Object(attrs) }));
    }
}

/// A message gains the files its attachments became. Its attachments stay:
/// they are the copies a model is shown.
pub(super) fn rewrite_message(
    message: &mut crate::agent::Message,
    brought: &BTreeMap<BlobId, Brought>,
) -> bool {
    let mut changed = false;
    for attachment in &message.attachments {
        if let Some(b) = brought.get(&attachment.blob)
            && !message.files.contains(&b.id)
        {
            message.files.push(b.id);
            changed = true;
        }
    }
    changed
}
