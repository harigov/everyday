//! The files catalog, on both databases: `files`, `file_refs`, `albums`,
//! `album_files` and the `files_settings` singleton (schema v29). See
//! [`karmayoga_core::store::files`] for the rules an implementation keeps
//! and which columns stay readable, and `schema.rs`'s `v29` for the tables.
//!
//! What stays in the clear is what an index is built from, and it splits by
//! where a file's bytes are. For every file: `storage`, `kind`, `size`,
//! `origin`, `status`, the person's `rating` and `favourite` marks, `hidden`,
//! the `stack` it is under, two timestamps, and the blob ids `blob` and
//! `thumb` (a *keyed* address, so it says nothing about the bytes it names).
//! For a *plain* file only: `modified_us` and `when_us`, and three tokens --
//! `path_key`, `dir_key`, `hash_key` -- that are keyed hashes ([`Tokens`]) of
//! its path, its folder and its contents. A sealed file's are `NULL`. A
//! caption, a tag, a person, a place, a property, and everything the file
//! says about itself, are in the sealed `data` beside them, and the path and
//! the hash once more in the sealed `idx`, for the reconciler.
//!
//! # Why tokens and not the path
//!
//! The folder can be on an encrypted volume, locked, while somebody holds a
//! copy of this database -- a backup, a stolen laptop. A readable path there
//! would hand them `Lawyer/settlement.pdf` and the whole folder tree the
//! volume was locked to keep, and a readable content hash would let them
//! confirm that a file they already have is among these. A token under a
//! subkey of the vault key answers the three questions the indexes ask --
//! *this* path, *this* folder, *these* contents -- and nothing else. What a
//! token cannot answer, the store answers after opening the sealed `idx` or
//! `data`: everything under a folder ([`FileFilter::under`]) and a listing by
//! name ([`FileSort::Name`]).
//!
//! # Every row is bound to its own name
//!
//! A file is sealed under [`file_aad`] with its own id, an album under
//! [`album_aad`], and the settings under their own singleton label, so a
//! payload copied into another row by somebody who can write the database
//! but does not hold the key will not open there.
//!
//! # A path is one file's
//!
//! The partial unique index `files_by_path` is the rule that two files not in
//! the trash cannot share a path, but a violation of it is a driver's error
//! and the two drivers do not say it the same way -- and on Postgres it would
//! leave the whole transaction unusable. So a write checks first, on the
//! writer and in the same transaction ([`check_paths`]), and answers
//! [`Error::Conflict`] itself; the index is the backstop that makes the rule
//! true of a writer that did not come through here.
//!
//! # References and memberships have no foreign keys
//!
//! As nowhere else in this schema: deleting a file takes its references and
//! its album memberships in the same transaction, and deleting an album takes
//! its memberships and never a file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::str::FromStr;

use karmayoga_core::crypto::SecretKey;
use karmayoga_core::error::{Error, Result};
use karmayoga_core::files::{
    Album, FileEntry, FileFilter, FileKind, FileSort, FileStorage, FilesSettings, FilesUsage,
    IndexRow, KindUsage, RefKind, RefOwner, StatusFilter, StorageKind,
};
use karmayoga_core::id::{AlbumId, BlobId, FileId};
use karmayoga_core::store::files::{FilesStore, album_aad, file_aad, files_settings_aad};

use crate::conn::{Sql, SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, placeholders, to_us, vals};
use serde::{Deserialize, Serialize};

impl Record for FileEntry {
    const TABLE: &'static str = "files";

    /// Every clear column but the tokens and `idx`, which need the vault's
    /// key and so are added by [`upsert_file`]. Nothing writes a file through
    /// [`Record`]'s own upsert for that reason.
    fn columns(&self) -> Vec<(&'static str, Value)> {
        let plain = !self.is_sealed();
        let sealed_blob = match &self.storage {
            FileStorage::Sealed { blob } => Some(blob.to_hex()),
            FileStorage::Plain { .. } => None,
        };
        vec![
            ("storage", storage_word(self.storage.kind()).to_value()),
            ("blob", sealed_blob.to_value()),
            ("thumb", self.thumbnail.map(BlobId::to_hex).to_value()),
            ("kind", self.kind.as_str().to_value()),
            ("size", (self.size as i64).to_value()),
            ("modified_us", plain.then(|| to_us(self.modified_at)).to_value()),
            ("when_us", plain.then(|| to_us(self.when())).to_value()),
            ("origin", self.origin.as_str().to_value()),
            ("status", self.status.as_str().to_value()),
            ("rating", self.rating.to_value()),
            ("favourite", self.favourite.to_value()),
            ("hidden", self.hidden.to_value()),
            ("stack", self.stack.map(|s| s.to_string()).to_value()),
            ("added_us", to_us(self.added_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Album {
    const TABLE: &'static str = "albums";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the description or the cover: what somebody
        // collects, and which of it they like best.
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

/// How many ids go into one `IN (...)`. SQLite refuses a statement with more
/// than 32,766 bound parameters and Postgres more than 65,535, and a
/// reconciler pass or an album can name more files than either. `money.rs`,
/// `news.rs` and `screentime.rs` keep the same number for the same reason.
const IN_CHUNK: usize = 500;

/// The most ids one [`FileFilter::ids`] may name.
///
/// One query has to carry them all, because its order and its page are
/// applied across the whole set, so they cannot be chunked as
/// [`IN_CHUNK`] does for a plain lookup; and SQLite's limit on bound
/// parameters is what a longer list would run into. A caller with more than
/// this asks for them by [`FilesStore::get_files`], or in pages.
const MAX_FILTER_IDS: usize = 30_000;

/// The label of the subkey a plain file's tokens are made under.
const TOKEN_LABEL: &str = "karmayoga.files.token.v1";

/// Keyed hashes of a plain file's path, folder and contents: what the
/// `path_key`, `dir_key` and `hash_key` columns hold. See the module docs.
///
/// Each is BLAKE3 keyed with a subkey of the vault key, over a word naming
/// what is hashed and the text, so a folder called `a` and a file at the
/// path `a` are different tokens. An unencrypted vault's subkey is derived
/// from nothing secret, as everything else in it is.
pub(crate) struct Tokens(SecretKey);

impl Tokens {
    fn of(&self, what: &str, text: &str) -> String {
        let mut hasher = blake3::Hasher::new_keyed(self.0.expose());
        hasher.update(what.as_bytes());
        hasher.update(&[0]);
        hasher.update(text.as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    fn path(&self, path: &str) -> String {
        self.of("path", path)
    }

    fn dir(&self, dir: &str) -> String {
        self.of("dir", dir.trim_matches('/'))
    }

    fn hash(&self, hash: &str) -> String {
        self.of("hash", hash)
    }
}

impl SqlStore {
    fn tokens(&self) -> Tokens {
        Tokens(self.cipher.derive_subkey(TOKEN_LABEL))
    }
}

/// A plain file's path and contents hash, sealed in `idx`: what
/// [`FilesStore::file_index`] reads instead of opening every record.
#[derive(Serialize, Deserialize)]
struct Index {
    path: String,
    #[serde(default)]
    hash: String,
}

/// `idx`'s associated data: the file's own label and a suffix, so the index
/// of one file opens only in its own row and never as anyone's `data`.
fn index_aad(id: FileId) -> Vec<u8> {
    let mut aad = file_aad(id);
    aad.extend_from_slice(b"#index");
    aad
}

/// Text when there is some, `NULL` when there is none.
fn text_or_null(text: Option<String>) -> Value {
    text.map(Value::Text).unwrap_or(Value::Null)
}

/// The `INSERT ... ON CONFLICT (id) DO UPDATE` for one file: `id`,
/// [`Record::columns`] with the tokens and `idx` after `storage`, then `data`
/// -- the order v29 declares them in.
fn upsert_file(
    file: &FileEntry,
    tokens: &Tokens,
    data: Vec<u8>,
    mut idx: Option<Vec<u8>>,
) -> (String, Vec<Value>) {
    // A plain file whose contents have not been read has no hash to say,
    // which is `NULL` and not a token of the empty string, so that "the same
    // contents" is never every file nothing has read yet.
    let hash = file.path().and(Some(file.hash.as_str())).filter(|h| !h.is_empty());
    let mut cols: Vec<(&'static str, Value)> = vec![("id", Value::Text(file.id.to_string()))];
    for column in file.columns() {
        let after_storage = column.0 == "storage";
        cols.push(column);
        if after_storage {
            cols.push(("path_key", text_or_null(file.path().map(|p| tokens.path(p)))));
            cols.push(("dir_key", text_or_null(file.dir().map(|d| tokens.dir(d)))));
            cols.push(("hash_key", text_or_null(hash.map(|h| tokens.hash(h)))));
            cols.push(("idx", idx.take().map(Value::Bytes).unwrap_or(Value::Null)));
        }
    }
    cols.push(("data", Value::Bytes(data)));

    let names: Vec<&str> = cols.iter().map(|(name, _)| *name).collect();
    let placeholders: Vec<String> = (1..=cols.len()).map(|n| format!("?{n}")).collect();
    let sets: Vec<String> = names[1..]
        .iter()
        .zip(&placeholders[1..])
        .map(|(name, p)| format!("{name} = {p}"))
        .collect();
    let sql = format!(
        "INSERT INTO files ({}) VALUES ({}) ON CONFLICT (id) DO UPDATE SET {}",
        names.join(", "),
        placeholders.join(", "),
        sets.join(", "),
    );
    (sql, cols.into_iter().map(|(_, v)| v).collect())
}

/// The folder a filter asks for everything under, when it is a folder and
/// not the top: what only the sealed paths can answer.
fn under_prefix(filter: &FileFilter) -> Option<&str> {
    filter.under.as_deref().map(|u| u.trim_matches('/')).filter(|u| !u.is_empty())
}

/// Whether `path` is in `folder` or anywhere under it.
fn is_under(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
}

/// Sort a listing by name, as [`FileSort::Name`] promises: plain files by
/// their name without regard to case, sealed files a group of their own in
/// the order they were added -- after the plain ones ascending, before them
/// descending -- and ties by id in the listing's own direction.
fn sort_by_name(files: &mut [FileEntry], descending: bool) {
    files.sort_by(|a, b| {
        let key = |f: &FileEntry| {
            let sealed = f.is_sealed();
            let name = if sealed { String::new() } else { f.name.to_lowercase() };
            let added = if sealed { to_us(f.added_at) } else { 0 };
            (sealed, name, added, f.id)
        };
        let order = key(a).cmp(&key(b));
        if descending { order.reverse() } else { order }
    });
}

/// The word the `storage` column holds.
fn storage_word(kind: StorageKind) -> &'static str {
    match kind {
        StorageKind::Plain => "plain",
        StorageKind::Sealed => "sealed",
    }
}

/// An id read back out of a text column.
fn id_of<T: FromStr>(text: &str) -> Result<T>
where
    T::Err: Display,
{
    text.parse().map_err(|e: T::Err| Error::Invalid(e.to_string()))
}

/// Refuse -- with [`Error::Conflict`] -- a batch that would give two files
/// not in the trash the same path. See the module docs for why this is asked
/// rather than left to the unique index.
///
/// The batch is walked in the order its rows will be written, against what
/// the table holds now: a file already stored frees the path it holds when it
/// is rewritten, so a batch that moves one file off a path and then gives
/// that path to another is accepted, because at no point does the path have
/// two holders. A file wanting a path somebody else holds, whether in the
/// batch or in the table, is refused; so is the batch that swaps two files'
/// paths in the wrong order, which the table would refuse row by row too.
///
/// Two queries a chunk rather than one a row, because each statement is a
/// round trip on a Postgres server, and a reconciler pass can bring
/// thousands of files: who holds each path the batch wants, and which path
/// each of the batch's own ids holds today.
fn check_paths(tx: &mut dyn Sql, tokens: &Tokens, files: &[FileEntry]) -> Result<()> {
    // Paths compared as their tokens, which is all the table holds of them.
    let wanted: BTreeSet<String> = files
        .iter()
        .filter(|f| !f.status.is_trashed())
        .filter_map(|f| f.path())
        .map(|p| tokens.path(p))
        .collect();
    let wanted: Vec<String> = wanted.into_iter().collect();

    // path token -> id, as the table has it now.
    let mut holder: HashMap<String, String> = HashMap::new();
    for chunk in wanted.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let rows = tx.query(
            &format!(
                "SELECT path_key, id FROM files WHERE status <> 'trashed' AND path_key IN ({})",
                placeholders(1, chunk.len())
            ),
            &args,
        )?;
        for row in &rows {
            holder.insert(row.text(0)?, row.text(1)?);
        }
    }

    // id -> the path token it holds now, for the ids this batch rewrites.
    let ids: BTreeSet<String> = files.iter().map(|f| f.id.to_string()).collect();
    let ids: Vec<String> = ids.into_iter().collect();
    let mut held_by: HashMap<String, String> = HashMap::new();
    for chunk in ids.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let rows = tx.query(
            &format!(
                "SELECT id, path_key FROM files
                 WHERE status <> 'trashed' AND path_key IS NOT NULL AND id IN ({})",
                placeholders(1, chunk.len())
            ),
            &args,
        )?;
        for row in &rows {
            held_by.insert(row.text(0)?, row.text(1)?);
        }
    }

    for file in files {
        let id = file.id.to_string();
        // Rewriting a file lets go of the path it held, whatever it holds
        // after.
        if let Some(old) = held_by.remove(&id)
            && holder.get(&old) == Some(&id)
        {
            holder.remove(&old);
        }
        if file.status.is_trashed() {
            continue;
        }
        let Some(path) = file.path() else { continue };
        let path = tokens.path(path);
        if holder.get(&path).is_some_and(|other| *other != id) {
            return Err(Error::Conflict { kind: "file" });
        }
        holder.insert(path.clone(), id.clone());
        held_by.insert(id, path);
    }
    Ok(())
}

/// Which of `files` are in the catalog, as the text of their ids.
fn existing_files(tx: &mut dyn Sql, files: &[FileId]) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for chunk in files.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
        let rows = tx.query(
            &format!("SELECT id FROM files WHERE id IN ({})", placeholders(1, chunk.len())),
            &args,
        )?;
        for row in &rows {
            found.insert(row.text(0)?);
        }
    }
    Ok(found)
}

/// One reference, written once however often it is asked for.
fn insert_ref(tx: &mut dyn Sql, owner: &RefOwner, file: FileId) -> Result<()> {
    tx.execute(
        "INSERT INTO file_refs (file_id, owner_kind, owner_id) VALUES (?1, ?2, ?3)
         ON CONFLICT (file_id, owner_kind, owner_id) DO NOTHING",
        &vals![file.to_string(), owner.kind.as_str(), owner.id.as_str()],
    )?;
    Ok(())
}

/// The `WHERE` clause a [`FileFilter`] asks for and its arguments, or `None`
/// when it can match nothing at all (an empty [`FileFilter::ids`]).
///
/// Clear columns only, as the trait promises. The time of a file is
/// `COALESCE(when_us, added_us)`: a sealed file's own time is sealed, so it
/// is filed -- here as in [`order_by`] -- by when it was added.
///
/// A folder is matched by its token: `dir_key = ?`. Everything *under* a
/// folder is a question no token answers, so the clause narrows to plain
/// files and the caller finishes the job on the sealed paths
/// ([`under_prefix`]). Leading and trailing slashes on a folder are not part
/// of its name.
fn filter_clause(filter: &FileFilter, tokens: &Tokens) -> Result<Option<(String, Vec<Value>)>> {
    let mut w = Where::new();
    if !filter.kinds.is_empty() {
        w = w.in_list("kind", filter.kinds.iter().map(|k| k.as_str()));
    }
    if let Some(ids) = &filter.ids {
        if ids.is_empty() {
            return Ok(None);
        }
        if ids.len() > MAX_FILTER_IDS {
            return Err(Error::Invalid(format!(
                "a listing can be limited to at most {MAX_FILTER_IDS} files by id, not {}",
                ids.len()
            )));
        }
        w = w.in_list("id", ids.iter().map(|id| id.to_string()));
    }
    if let Some(dir) = &filter.dir {
        w = w.eq("dir_key", tokens.dir(dir));
    }
    if let Some(origin) = filter.origin {
        w = w.eq("origin", origin.as_str());
    }
    for origin in &filter.exclude_origins {
        w = w.ne("origin", origin.as_str());
    }
    if let Some(storage) = filter.storage {
        w = w.eq("storage", storage_word(storage));
    }
    if let Some(min) = filter.min_rating {
        w = w.gte("rating", min);
    }
    if let Some(from) = filter.from {
        w = w.gte("COALESCE(when_us, added_us)", to_us(from));
    }
    if let Some(to) = filter.to {
        w = w.lt("COALESCE(when_us, added_us)", to_us(to));
    }
    let (mut sql, mut args) = w.finish();

    if filter.under.is_some() {
        // Every plain file; which of them are under a folder other than the
        // top is decided after opening their paths.
        sql.push_str(" AND path_key IS NOT NULL");
    }
    if let Some(top) = filter.stack {
        args.push(Value::Text(top.to_string()));
        let n = args.len();
        sql.push_str(&format!(" AND (stack = ?{n} OR id = ?{n})"));
    }
    if let Some(album) = filter.album {
        args.push(Value::Text(album.to_string()));
        sql.push_str(&format!(
            " AND id IN (SELECT file_id FROM album_files WHERE album_id = ?{})",
            args.len()
        ));
    }
    if filter.favourite {
        sql.push_str(" AND favourite");
    }
    match filter.status {
        StatusFilter::Live => sql.push_str(" AND status <> 'trashed'"),
        StatusFilter::Present => sql.push_str(" AND status = 'present'"),
        StatusFilter::Missing => sql.push_str(" AND status = 'missing'"),
        StatusFilter::Trashed => sql.push_str(" AND status = 'trashed'"),
        StatusFilter::All => {}
    }
    if !filter.include_hidden {
        sql.push_str(" AND NOT hidden");
    }
    if filter.stack_tops_only {
        sql.push_str(" AND stack IS NULL");
    }
    Ok(Some((sql, args)))
}

/// `ORDER BY`'s terms for a listing: the sort's key, then the id in the same
/// direction, so a page boundary never falls between two files that tie.
///
/// A sealed file's time is sealed, so [`FileSort::When`] files it by
/// `added_us`, which is a time like any other. No column holds a name, so
/// [`FileSort::Name`] orders by id here and the listing is sorted after
/// opening ([`sort_by_name`]).
fn order_by(sort: FileSort, descending: bool) -> String {
    let way = if descending { "DESC" } else { "ASC" };
    let key = match sort {
        FileSort::When => format!("COALESCE(when_us, added_us) {way}, "),
        FileSort::Added => format!("added_us {way}, "),
        FileSort::Size => format!("size {way}, "),
        FileSort::Name => String::new(),
    };
    format!("{key}id {way}")
}

impl FilesStore for SqlStore {
    // ---- files ----------------------------------------------------------

    fn put_file(&self, file: &FileEntry) -> Result<()> {
        self.put_files(std::slice::from_ref(file))
    }

    fn put_files(&self, files: &[FileEntry]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        // Sealed before the writer is taken, as `put_txns` does: a reconciler
        // pass over a large folder brings thousands of files, and nothing
        // else needs to wait behind their encryption.
        let tokens = self.tokens();
        let mut sealed = Vec::with_capacity(files.len());
        for file in files {
            let data = self.seal(&file_aad(file.id), file)?;
            let idx = match file.path() {
                Some(path) => Some(self.seal(
                    &index_aad(file.id),
                    &Index { path: path.to_string(), hash: file.hash.clone() },
                )?),
                None => None,
            };
            sealed.push((data, idx));
        }

        // Checked and written on the writer in one transaction, so a path the
        // check found free cannot be taken by another write before this one
        // lands. An early `?` drops the transaction, which rolls it back: all
        // of the batch or none of it.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        check_paths(tx.as_mut(), &tokens, files)?;
        for (file, (data, idx)) in files.iter().zip(sealed) {
            let (sql, args) = upsert_file(file, &tokens, data, idx);
            tx.execute(&sql, &args)?;
        }
        tx.commit()
    }

    fn get_file(&self, id: FileId) -> Result<FileEntry> {
        self.get::<FileEntry>(id)
    }

    fn get_files(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            let rows = self.read().records(
                &format!(
                    "SELECT id, data FROM files WHERE id IN ({})",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
            let found: Vec<FileEntry> = self.collect(rows, file_aad)?;
            out.extend(found);
        }
        Ok(out)
    }

    fn file_by_path(&self, path: &str) -> Result<Option<FileEntry>> {
        // The file in the folder first: that is the one that holds the path
        // (and what `files_by_path` indexes). Only when there is none is the
        // trash asked, which `status` narrows to the few files in it rather
        // than every file with a path.
        let key = self.tokens().path(path);
        let live = self.read().records(
            "SELECT id, data FROM files WHERE path_key = ?1 AND status <> 'trashed'",
            &vals![key.as_str()],
        )?;
        let live: Vec<FileEntry> = self.collect(live, file_aad)?;
        if let Some(file) = live.into_iter().next() {
            return Ok(Some(file));
        }
        let trashed = self.read().records(
            "SELECT id, data FROM files WHERE path_key = ?1 AND status = 'trashed'
             ORDER BY id LIMIT 1",
            &vals![key.as_str()],
        )?;
        let trashed: Vec<FileEntry> = self.collect(trashed, file_aad)?;
        Ok(trashed.into_iter().next())
    }

    fn files_by_hash(&self, hash: &str) -> Result<Vec<FileEntry>> {
        // No hash is not a hash that every unread file shares.
        if hash.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.tokens().hash(hash);
        let rows = self.read().records(
            "SELECT id, data FROM files WHERE hash_key = ?1 ORDER BY id",
            &vals![key.as_str()],
        )?;
        self.collect(rows, file_aad)
    }

    fn delete_file(&self, id: FileId) -> Result<()> {
        // The references and memberships go in the same transaction as the
        // file, so a crash between them cannot leave an album holding a file
        // that is not there. The records that referred to it are untouched:
        // an entry keeps the picture it said, and shows it missing.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM file_refs WHERE file_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM album_files WHERE file_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM files WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }

    fn query_files(&self, filter: &FileFilter) -> Result<Vec<FileEntry>> {
        let tokens = self.tokens();
        let Some((clause, args)) = filter_clause(filter, &tokens)? else { return Ok(Vec::new()) };
        let order = order_by(filter.sort, filter.descending);
        let mut sql = format!("SELECT id, data FROM files WHERE {clause}");
        let under = under_prefix(filter);
        if under.is_none() && filter.sort != FileSort::Name {
            self.page(&mut sql, &order, filter.limit, filter.offset);
            let rows = self.read().records(&sql, &args)?;
            return self.collect(rows, file_aad);
        }
        // What only the sealed records answer: the whole match, opened, then
        // narrowed, sorted and paged here.
        self.page(&mut sql, &order, None, 0);
        let rows = self.read().records(&sql, &args)?;
        let mut files: Vec<FileEntry> = self.collect(rows, file_aad)?;
        if let Some(folder) = under {
            files.retain(|f| f.path().is_some_and(|p| is_under(p, folder)));
        }
        if filter.sort == FileSort::Name {
            sort_by_name(&mut files, filter.descending);
        }
        let limit = filter.limit.map_or(usize::MAX, |n| n as usize);
        Ok(files.into_iter().skip(filter.offset as usize).take(limit).collect())
    }

    fn count_files(&self, filter: &FileFilter) -> Result<u64> {
        if under_prefix(filter).is_some() {
            let all = FileFilter { limit: None, offset: 0, ..filter.clone() };
            return Ok(self.query_files(&all)?.len() as u64);
        }
        let Some((clause, args)) = filter_clause(filter, &self.tokens())? else { return Ok(0) };
        let n =
            self.read().scalar_i64(&format!("SELECT COUNT(*) FROM files WHERE {clause}"), &args)?;
        Ok(n.max(0) as u64)
    }

    fn stack_counts(&self, tops: &[FileId]) -> Result<BTreeMap<FileId, u64>> {
        let mut out = BTreeMap::new();
        for chunk in tops.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            let rows = self.read().query(
                &format!(
                    "SELECT stack, COUNT(*) FROM files
                     WHERE status <> 'trashed' AND stack IN ({}) GROUP BY stack",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
            for row in &rows {
                out.insert(id_of(&row.text(0)?)?, row.u64(1)?);
            }
        }
        Ok(out)
    }

    fn file_index(&self) -> Result<Vec<IndexRow>> {
        // The sealed `idx` rather than whole records, and in no order: the
        // reconciler makes a map of it to compare against a walk of the
        // folder every few minutes, and the path and the hash are all it
        // needs that the clear columns do not say.
        let rows = self.read().query(
            "SELECT id, idx, size, modified_us, status FROM files
             WHERE path_key IS NOT NULL AND status <> 'trashed'",
            &[],
        )?;
        rows.iter()
            .map(|r| {
                let id: FileId = id_of(&r.text(0)?)?;
                let index: Index = match r.opt_bytes(1)? {
                    Some(sealed) => self.unseal(&index_aad(id), &sealed)?,
                    // Never written without one, but a row that somehow has
                    // none is read the long way rather than left out -- left
                    // out, it would look vanished and be marked missing.
                    None => {
                        let file = self.get_file(id)?;
                        Index { path: file.path().unwrap_or_default().to_string(), hash: file.hash }
                    }
                };
                Ok(IndexRow {
                    id,
                    path: index.path,
                    size: r.u64(2)?,
                    modified_us: r.opt_i64(3)?.unwrap_or(0),
                    hash: index.hash,
                    missing: r.text(4)? == "missing",
                })
            })
            .collect()
    }

    fn files_usage(&self) -> Result<FilesUsage> {
        // Grouped by the three columns a total is split on, and folded in
        // Rust: the sum is cast, because Postgres makes `SUM` over a `BIGINT`
        // a `numeric`, which this crate's driver does not read.
        let rows = self.read().query(
            "SELECT status, storage, kind, COUNT(*), CAST(COALESCE(SUM(size), 0) AS BIGINT)
             FROM files GROUP BY status, storage, kind",
            &[],
        )?;
        let mut usage = FilesUsage::default();
        let mut by_kind: BTreeMap<FileKind, (u64, u64)> = BTreeMap::new();
        for row in &rows {
            let (status, storage) = (row.text(0)?, row.text(1)?);
            let (files, bytes) = (row.u64(3)?, row.u64(4)?);
            if status == "trashed" {
                usage.trashed_files += files;
                usage.trashed_bytes += bytes;
                continue;
            }
            // Live: present and missing, sealed and plain alike. The sealed
            // figures are the part of that held in the vault.
            usage.files += files;
            usage.bytes += bytes;
            if status == "missing" {
                usage.missing_files += files;
            }
            if storage == "sealed" {
                usage.sealed_files += files;
                usage.sealed_bytes += bytes;
            }
            let word = row.text(2)?;
            let kind = FileKind::parse(&word)
                .ok_or_else(|| Error::Invalid(format!("{word:?} is not a kind of file")))?;
            let slot = by_kind.entry(kind).or_default();
            slot.0 += files;
            slot.1 += bytes;
        }
        usage.by_kind = FileKind::ALL
            .into_iter()
            .filter_map(|kind| {
                by_kind.get(&kind).map(|&(files, bytes)| KindUsage { kind, files, bytes })
            })
            .collect();
        Ok(usage)
    }

    fn file_blob_refs(&self) -> Result<BTreeSet<BlobId>> {
        // Two clear columns, no record opened: this is walked by every
        // garbage-collection sweep. `UNION` also removes the repeats.
        let rows = self.read().query(
            "SELECT blob FROM files WHERE blob IS NOT NULL
             UNION
             SELECT thumb FROM files WHERE thumb IS NOT NULL",
            &[],
        )?;
        let mut out = BTreeSet::new();
        for row in &rows {
            out.insert(BlobId::parse(&row.text(0)?)?);
        }
        Ok(out)
    }

    // ---- references -----------------------------------------------------

    fn set_refs(&self, owner: &RefOwner, files: &[FileId]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "DELETE FROM file_refs WHERE owner_kind = ?1 AND owner_id = ?2",
            &vals![owner.kind.as_str(), owner.id.as_str()],
        )?;
        for file in files {
            insert_ref(tx.as_mut(), owner, *file)?;
        }
        tx.commit()
    }

    fn replace_all_refs(&self, refs: &[(RefOwner, FileId)]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM file_refs", &[])?;
        for (owner, file) in refs {
            insert_ref(tx.as_mut(), owner, *file)?;
        }
        tx.commit()
    }

    fn refs_to(&self, file: FileId) -> Result<Vec<RefOwner>> {
        let rows = self.read().query(
            "SELECT owner_kind, owner_id FROM file_refs WHERE file_id = ?1
             ORDER BY owner_kind, owner_id",
            &vals![file.to_string()],
        )?;
        rows.iter()
            .map(|r| {
                let word = r.text(0)?;
                let kind = RefKind::parse(&word).ok_or_else(|| {
                    Error::Invalid(format!("{word:?} is not a kind of record that refers to files"))
                })?;
                Ok(RefOwner { kind, id: r.text(1)? })
            })
            .collect()
    }

    fn referenced_files(&self) -> Result<BTreeSet<FileId>> {
        let rows = self.read().query("SELECT DISTINCT file_id FROM file_refs", &[])?;
        rows.iter().map(|r| id_of(&r.text(0)?)).collect()
    }

    // ---- albums ---------------------------------------------------------

    fn list_albums(&self) -> Result<Vec<Album>> {
        // In the order they were made. Which order a person sees them in is
        // the interface's -- the names are sealed.
        let rows =
            self.read().records("SELECT id, data FROM albums ORDER BY created_us, id", &[])?;
        self.collect(rows, album_aad)
    }

    fn get_album(&self, id: AlbumId) -> Result<Album> {
        self.get::<Album>(id)
    }

    fn put_album(&self, album: &Album) -> Result<()> {
        self.upsert(album)
    }

    fn delete_album(&self, id: AlbumId) -> Result<()> {
        // The memberships, never the files in them: an album is a way of
        // gathering, and the files are somebody's.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM album_files WHERE album_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM albums WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }

    fn album_files(&self, id: AlbumId) -> Result<Vec<FileId>> {
        let rows = self.read().query(
            "SELECT file_id FROM album_files WHERE album_id = ?1 ORDER BY position, file_id",
            &vals![id.to_string()],
        )?;
        rows.iter().map(|r| id_of(&r.text(0)?)).collect()
    }

    fn add_to_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        if tx.query_opt("SELECT 1 FROM albums WHERE id = ?1", &vals![id.to_string()])?.is_none() {
            // A membership of an album that is not there would be one nothing
            // could list or delete.
            return Err(Error::not_found("album", id));
        }
        // Files not in the catalog are left out, as `get_files` leaves them
        // out, rather than kept as memberships of nothing.
        let known = existing_files(tx.as_mut(), files)?;
        let mut next = tx.scalar_i64(
            "SELECT COALESCE(MAX(position), -1) FROM album_files WHERE album_id = ?1",
            &vals![id.to_string()],
        )? + 1;
        for file in files.iter().filter(|f| known.contains(&f.to_string())) {
            // `DO NOTHING` is what keeps a file already in the album where it
            // is, and a file named twice at its first place; only a row that
            // was written uses up a position.
            let added = tx.execute(
                "INSERT INTO album_files (album_id, file_id, position) VALUES (?1, ?2, ?3)
                 ON CONFLICT (album_id, file_id) DO NOTHING",
                &vals![id.to_string(), file.to_string(), next],
            )?;
            next += added as i64;
        }
        tx.commit()
    }

    fn remove_from_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for chunk in files.chunks(IN_CHUNK) {
            let mut args = vec![Value::Text(id.to_string())];
            args.extend(chunk.iter().map(|f| Value::Text(f.to_string())));
            tx.execute(
                &format!(
                    "DELETE FROM album_files WHERE album_id = ?1 AND file_id IN ({})",
                    placeholders(2, chunk.len())
                ),
                &args,
            )?;
        }
        tx.commit()
    }

    fn albums_of(&self, file: FileId) -> Result<Vec<AlbumId>> {
        let rows = self.read().query(
            "SELECT album_id FROM album_files WHERE file_id = ?1 ORDER BY album_id",
            &vals![file.to_string()],
        )?;
        rows.iter().map(|r| id_of(&r.text(0)?)).collect()
    }

    fn album_counts(&self) -> Result<BTreeMap<AlbumId, u64>> {
        // Every album, an empty one as nought: the join to `files` is in the
        // `LEFT JOIN`'s own condition so that a trashed file is left out of
        // the count without taking its album out of the answer.
        let rows = self.read().query(
            "SELECT a.id, COUNT(f.id) FROM albums a
             LEFT JOIN album_files af ON af.album_id = a.id
             LEFT JOIN files f ON f.id = af.file_id AND f.status <> 'trashed'
             GROUP BY a.id",
            &[],
        )?;
        rows.iter().map(|r| Ok((id_of(&r.text(0)?)?, r.u64(1)?))).collect()
    }

    // ---- settings -------------------------------------------------------

    fn files_settings(&self) -> Result<FilesSettings> {
        // Never saved. The defaults -- no folder yet, nothing sealed, a month
        // in the trash -- are a real answer, as screen time's are.
        Ok(self.get_singleton("files_settings", &files_settings_aad())?.unwrap_or_default())
    }

    fn put_files_settings(&self, settings: &FilesSettings) -> Result<()> {
        self.put_singleton("files_settings", &files_settings_aad(), settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::schema::steps;
    use jiff::Timestamp;
    use karmayoga_core::files::{FileOrigin, FileStatus, MediaInfo, PropValue};

    /// Keyed tokens in the catalog's clear columns are derived under this
    /// label; changing it would orphan every one already stored.
    #[test]
    fn token_label_is_pinned() {
        assert_eq!(TOKEN_LABEL, "karmayoga.files.token.v1");
    }

    /// The columns the schema declares for `table`, in order, `id` and
    /// `data` left out -- the list [`Record::columns`] has to match.
    fn declared(table: &str) -> Vec<String> {
        let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let ddl = steps(Dialect::Sqlite)
            .concat()
            .into_iter()
            .find(|sql| sql.contains(&head))
            .unwrap_or_else(|| panic!("no CREATE TABLE for {table}"));
        let open = ddl.find('(').expect("an open paren");
        let close = ddl.rfind(')').expect("a close paren");
        let body = &ddl[open + 1..close];
        body.lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(|word| word.trim_end_matches(',').to_string())
            .filter(|word| word != "id" && word != "data")
            .collect()
    }

    fn names<R: Record>(record: &R) -> Vec<String> {
        record.columns().into_iter().map(|(name, _)| name.to_string()).collect()
    }

    fn tokens() -> Tokens {
        Tokens(SecretKey::from_bytes([7; 32]))
    }

    /// The columns a file is written with, by name -- `id` and `data` left
    /// out, as [`declared`] leaves them out.
    fn written(file: &FileEntry) -> Vec<(&'static str, Value)> {
        let idx = file.path().map(|_| b"sealed index".to_vec());
        let (sql, values) = upsert_file(file, &tokens(), b"sealed".to_vec(), idx);
        let open = sql.find('(').unwrap();
        let close = sql.find(')').unwrap();
        let names: Vec<&str> = sql[open + 1..close].split(", ").collect();
        let names: Vec<&'static str> =
            names.into_iter().map(|n| &*Box::leak(n.to_string().into_boxed_str())).collect();
        names.into_iter().zip(values).filter(|(name, _)| *name != "id" && *name != "data").collect()
    }

    const WORD: &str = "Platypus";

    fn entry(storage: FileStorage) -> FileEntry {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let mut file = FileEntry {
            id: FileId::new(),
            storage,
            name: format!("{WORD} trip.jpg"),
            kind: FileKind::Image,
            mime: "image/jpeg".into(),
            size: 4_096,
            hash: BlobId::of(b"contents").to_hex(),
            modified_at: now,
            added_at: now,
            updated_at: now,
            origin: FileOrigin::Files,
            status: FileStatus::Present,
            media: MediaInfo::default(),
            thumbnail: Some(BlobId::of(b"thumbnail")),
            caption: format!("{WORD} at the lake"),
            tags: vec![format!("{WORD}-tag")],
            rating: Some(4),
            favourite: true,
            hidden: false,
            people: vec![format!("{WORD} Person")],
            place: Some(format!("{WORD} Lake")),
            stack: None,
            properties: Default::default(),
        };
        file.media.taken_at = Some(Timestamp::from_second(1_700_000_000).unwrap());
        file.media.camera_model = Some(format!("{WORD} 3000"));
        let key = format!("user.{}", WORD.to_lowercase());
        file.properties.insert(key, PropValue::Text(WORD.into()));
        file
    }

    fn plain() -> FileEntry {
        entry(FileStorage::Plain { path: format!("Photos/{WORD}/trip.jpg") })
    }

    fn sealed() -> FileEntry {
        entry(FileStorage::Sealed { blob: BlobId::of(b"sealed contents") })
    }

    fn album() -> Album {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        Album {
            id: AlbumId::new(),
            name: format!("{WORD} holiday"),
            description: format!("{WORD} things"),
            cover: Some(FileId::new()),
            created_at: now,
            updated_at: now,
        }
    }

    fn value<'a>(columns: &'a [(&'static str, Value)], name: &str) -> &'a Value {
        &columns.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("no column {name}")).1
    }

    #[test]
    fn the_clear_columns_are_exactly_the_ones_v29_declares() {
        // `upsert_stmt` writes `columns()` into the table by name, so a
        // column here that the table lacks fails every write, and one the
        // table has that is missing here is left at its default forever.
        let names_of = |file: &FileEntry| -> Vec<String> {
            written(file).into_iter().map(|(name, _)| name.to_string()).collect()
        };
        assert_eq!(names_of(&plain()), declared(FileEntry::TABLE));
        assert_eq!(names_of(&sealed()), declared(FileEntry::TABLE));
        assert_eq!(names(&album()), declared(Album::TABLE));
    }

    #[test]
    fn a_sealed_files_path_name_hash_and_times_are_not_in_a_column() {
        let columns = written(&sealed());
        for name in ["path_key", "dir_key", "hash_key", "idx", "modified_us", "when_us"] {
            assert_eq!(value(&columns, name), &Value::Null, "{name} of a sealed file is clear");
        }
        // What stays: where the contents are, and the marks the index reads.
        assert_eq!(value(&columns, "storage"), &Value::Text("sealed".into()));
        assert_eq!(value(&columns, "blob"), &Value::Text(BlobId::of(b"sealed contents").to_hex()));
        assert_eq!(value(&columns, "thumb"), &Value::Text(BlobId::of(b"thumbnail").to_hex()));
        assert_eq!(value(&columns, "rating"), &Value::Int(4));
        assert_eq!(value(&columns, "favourite"), &Value::Bool(true));
    }

    #[test]
    fn a_plain_files_path_folder_and_hash_are_tokens_and_its_times_are_clear() {
        let columns = written(&plain());
        let t = tokens();
        let path = format!("Photos/{WORD}/trip.jpg");
        assert_eq!(value(&columns, "path_key"), &Value::Text(t.path(&path)));
        assert_eq!(value(&columns, "dir_key"), &Value::Text(t.dir(&format!("Photos/{WORD}"))));
        assert_eq!(
            value(&columns, "hash_key"),
            &Value::Text(t.hash(&BlobId::of(b"contents").to_hex()))
        );
        assert!(matches!(value(&columns, "idx"), Value::Bytes(_)), "the path, sealed");
        assert_eq!(value(&columns, "blob"), &Value::Null, "a plain file has no blob of its own");
        // A token is the same for the same text under the same key, and
        // nothing like it under another: what the indexes need, and nothing a
        // reader without the key can use.
        assert_ne!(t.path(&path), Tokens(SecretKey::from_bytes([8; 32])).path(&path));
        assert_ne!(t.path("a"), t.dir("a"), "a folder and a file of one name differ");
        // The time taken, not the time modified, is what a timeline files it
        // under.
        assert_eq!(value(&columns, "when_us"), &Value::Int(1_700_000_000_000_000));
        assert_eq!(value(&columns, "modified_us"), &Value::Int(1_800_000_000_000_000));

        // A file nobody has hashed yet has no hash, not an empty one.
        let mut unread = plain();
        unread.hash.clear();
        assert_eq!(value(&written(&unread), "hash_key"), &Value::Null);
        // And a file at the top of the folder is in `""`, which is a folder
        // like any other, not nothing.
        let mut top = plain();
        top.storage = FileStorage::Plain { path: "top.jpg".into() };
        assert_eq!(value(&written(&top), "dir_key"), &Value::Text(t.dir("")));
    }

    #[test]
    fn no_clear_column_holds_anything_the_person_added() {
        // What a database holder sees of a file is exactly what it is written
        // with. Whatever it is, it must not hold the caption, a tag, a person,
        // the place, a property, the camera -- or, for any file, its path or
        // its name: the folder may be locked away while the database is not.
        let all = [
            ("plain", written(&plain())),
            ("sealed", written(&sealed())),
            ("album", album().columns()),
        ];
        for (record, columns) in all {
            for (name, value) in columns {
                match &value {
                    Value::Text(text) => {
                        let lower = text.to_lowercase();
                        let platypus = WORD.to_lowercase();
                        assert!(!lower.contains(&platypus), "{record}.{name} holds {text:?}");
                    }
                    Value::Int(_) | Value::Null | Value::Bool(_) => {}
                    // Sealed, so whatever it holds is not readable.
                    Value::Bytes(_) if name == "idx" => {}
                    Value::Bytes(_) | Value::Real(_) => {
                        panic!("{record}.{name} is not a clear column of a kind v29 declares")
                    }
                }
            }
        }
    }

    /// The numbers of the `?N` placeholders in `sql`, in the order written.
    fn placeholder_numbers(sql: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut rest = sql;
        while let Some(at) = rest.find('?') {
            let digits: String =
                rest[at + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
            out.push(digits.parse().expect("a placeholder is followed by its number"));
            rest = &rest[at + 1 + digits.len()..];
        }
        out
    }

    #[test]
    fn a_filter_numbers_every_placeholder_once_and_gives_an_argument_to_each() {
        let filter = FileFilter {
            kinds: vec![FileKind::Image, FileKind::Video],
            dir: Some("Photos".into()),
            under: Some("Photos/".into()),
            album: Some(AlbumId::new()),
            ids: Some(vec![FileId::new(), FileId::new()]),
            favourite: true,
            min_rating: Some(3),
            origin: Some(FileOrigin::Journal),
            status: StatusFilter::Present,
            storage: Some(StorageKind::Plain),
            from: Some(Timestamp::from_second(10).unwrap()),
            to: Some(Timestamp::from_second(20).unwrap()),
            include_hidden: false,
            stack_tops_only: true,
            stack: Some(FileId::new()),
            ..FileFilter::default()
        };
        let (sql, args) = filter_clause(&filter, &tokens()).unwrap().unwrap();
        let mut numbers = placeholder_numbers(&sql);
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers, (1..=args.len()).collect::<Vec<_>>(), "{sql}");
    }

    #[test]
    fn the_default_filter_is_what_somebody_not_in_the_trash_and_not_hidden() {
        let (sql, args) = filter_clause(&FileFilter::default(), &tokens()).unwrap().unwrap();
        assert_eq!(sql, "1=1 AND status <> 'trashed' AND NOT hidden");
        assert!(args.is_empty());
    }

    #[test]
    fn the_status_words_in_a_filter_are_the_ones_the_column_holds() {
        let since = Timestamp::UNIX_EPOCH;
        let trashed = FileStatus::Trashed { at: since, from: String::new() };
        let cases = [
            (StatusFilter::Present, FileStatus::Present),
            (StatusFilter::Missing, FileStatus::Missing { since }),
            (StatusFilter::Trashed, trashed.clone()),
        ];
        for (status, word) in cases {
            let filter = FileFilter { status, ..FileFilter::default() };
            let (sql, _) = filter_clause(&filter, &tokens()).unwrap().unwrap();
            assert!(sql.contains(&format!("status = '{}'", word.as_str())), "{sql}");
        }
        let live = FileFilter { status: StatusFilter::Live, ..FileFilter::default() };
        let (sql, _) = filter_clause(&live, &tokens()).unwrap().unwrap();
        assert!(sql.contains(&format!("status <> '{}'", trashed.as_str())), "{sql}");
        let all = FileFilter { status: StatusFilter::All, ..FileFilter::default() };
        assert!(!filter_clause(&all, &tokens()).unwrap().unwrap().0.contains("status"));
    }

    #[test]
    fn a_folder_is_matched_by_its_token_and_never_by_its_name() {
        let t = tokens();
        let filter = FileFilter { dir: Some("/Photos/2024/".into()), ..FileFilter::default() };
        let (sql, args) = filter_clause(&filter, &t).unwrap().unwrap();
        assert!(sql.contains("dir_key"), "{sql}");
        assert!(args.contains(&Value::Text(t.dir("Photos/2024"))));
        assert!(!args.iter().any(|a| matches!(a, Value::Text(s) if s.contains("Photos"))));

        // Under a folder is answered after opening: the clause only narrows
        // to plain files, and names nothing.
        let under = FileFilter { under: Some("/Photos/2024/".into()), ..FileFilter::default() };
        let (sql, args) = filter_clause(&under, &t).unwrap().unwrap();
        assert!(sql.contains("path_key IS NOT NULL"), "{sql}");
        assert!(args.is_empty());
        assert_eq!(under_prefix(&under), Some("Photos/2024"));
        assert_eq!(
            under_prefix(&FileFilter { under: Some("/".into()), ..FileFilter::default() }),
            None
        );
        assert!(is_under("Photos/2024/a.jpg", "Photos/2024"));
        assert!(is_under("Photos/2024/deep/a.jpg", "Photos/2024"));
        assert!(!is_under("Photos/2024b/a.jpg", "Photos/2024"));
        assert!(!is_under("photos/2024/a.jpg", "Photos/2024"), "case matters, as on the disk");
    }

    #[test]
    fn an_empty_list_of_ids_matches_nothing_and_a_huge_one_is_refused() {
        let none = FileFilter { ids: Some(Vec::new()), ..FileFilter::default() };
        assert!(
            filter_clause(&none, &tokens()).unwrap().is_none(),
            "no ids is no files, not every file"
        );

        let many = FileFilter {
            ids: Some((0..=MAX_FILTER_IDS).map(|_| FileId::new()).collect()),
            ..FileFilter::default()
        };
        assert_eq!(filter_clause(&many, &tokens()).unwrap_err().code(), "invalid");
    }

    #[test]
    fn every_listing_ends_by_ordering_on_the_id_in_its_own_direction() {
        for sort in [FileSort::When, FileSort::Added, FileSort::Name, FileSort::Size] {
            let up = order_by(sort, false);
            let down = order_by(sort, true);
            assert!(up.ends_with("id ASC"), "{up}");
            assert!(down.ends_with("id DESC"), "{down}");
            assert!(!up.contains("DESC") && !down.contains("ASC"), "{up} / {down}");
        }
    }

    #[test]
    fn names_sort_without_case_with_sealed_files_a_group_by_when_added() {
        let at = |n| Timestamp::from_second(n).unwrap();
        let mut b = plain();
        b.name = "beta.jpg".into();
        let mut a = plain();
        a.name = "Alpha.jpg".into();
        let mut old = sealed();
        old.added_at = at(1_000);
        let mut new = sealed();
        new.added_at = at(2_000);
        let mut files = vec![new.clone(), b.clone(), old.clone(), a.clone()];
        sort_by_name(&mut files, false);
        let order: Vec<FileId> = files.iter().map(|f| f.id).collect();
        assert_eq!(order, vec![a.id, b.id, old.id, new.id]);
        sort_by_name(&mut files, true);
        let order: Vec<FileId> = files.iter().map(|f| f.id).collect();
        assert_eq!(order, vec![new.id, old.id, b.id, a.id]);
    }
}
