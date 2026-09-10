//! The container: a zip file, written and read here.
//!
//! # Why not a zip crate
//!
//! Because of what it would cost against what it would save. The two hard
//! parts of zip are the compressor and the checksum, and this workspace
//! already builds both -- `flate2` and `crc32fast` are in the dependency
//! graph today, pulled in by the toolchain around us. What is left is the
//! container itself: three fixed-layout records that have not changed since
//! 1993, plus the ZIP64 extensions that have not changed since 2001. That is
//! the file below.
//!
//! The archive is also the one artefact of this application that is read by
//! *other* programs and, on the way back in, written by them. Owning the
//! container means the bytes are ours to make deterministic -- same vault,
//! same choices, same archive -- which is what lets a test compare two
//! exports rather than compare two file listings.
//!
//! # What is supported
//!
//! Written: stored and deflated entries, UTF-8 names, ZIP64 for archives past
//! 4 GiB or 65,535 files. Read: the same, plus whatever a normal archiver
//! produces, because unzipping and rezipping an export in Finder or Explorer
//! is the obvious way somebody will edit one.
//!
//! Not supported, and refused rather than half-handled: encrypted entries,
//! and compression methods other than store and deflate. Both come back as
//! [`Error::Invalid`] naming the file, which is a better answer than a
//! silently missing note.
//!
//! # Path safety
//!
//! Nothing here writes to a filesystem. A reader hands back names as they
//! appeared in the archive and the caller looks them up in a map, so the
//! traversal that archive extractors get wrong -- `../../.ssh/authorized_keys`
//! -- has nowhere to land. [`Reader::open`] rejects such names anyway, on the
//! principle that a defence which costs four lines should not depend on every
//! future caller remembering the rule.

use everyday_core::{Error, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

/// Bit 11 of the general-purpose flags: the name is UTF-8 rather than in
/// whatever code page the machine that wrote it happened to use. Every name
/// this application writes is UTF-8, and saying so is what stops an archiver
/// mangling an entry titled "Café" on the way out.
const UTF8_NAMES: u16 = 1 << 11;

/// Bit 0: the entry is encrypted. Refused on read -- see the module docs.
const ENCRYPTED: u16 = 1;

/// The sentinel a 32-bit field carries when the real value is in a ZIP64
/// extra field instead.
const ZIP64_MARK: u32 = 0xFFFF_FFFF;

/// A file on its way into an archive.
struct Entry {
    name: String,
    /// Where its local header starts.
    offset: u64,
    compressed: u64,
    uncompressed: u64,
    crc: u32,
    method: u16,
}

/// Writes a zip stream.
///
/// Sequential: every entry's size and checksum are known before its header is
/// written, because the caller hands over the whole of a file's bytes at once.
/// That is what lets this write to a plain [`Write`] with no seeking, so the
/// same code serves a `Vec<u8>` held in memory for a window to download and a
/// file the command line is streaming to disk.
pub struct Writer<W: Write> {
    out: W,
    offset: u64,
    entries: Vec<Entry>,
    /// MS-DOS date and time, as every entry's modification stamp.
    ///
    /// One stamp for the whole archive rather than each record's own
    /// `updated_at`: the timestamp field has two-second resolution and no
    /// time zone, so it cannot carry the truth anyway -- the truth is in the
    /// front matter of the file it belongs to. What it can do is make two
    /// exports of an unchanged vault identical, which is worth more.
    stamp: (u16, u16),
}

impl<W: Write> Writer<W> {
    /// Start an archive whose entries are all stamped `at`.
    pub fn new(out: W, at: jiff::civil::DateTime) -> Self {
        Self { out, offset: 0, entries: Vec::new(), stamp: dos_stamp(at) }
    }

    /// Add one file. `name` is a forward-slashed path relative to the root.
    ///
    /// Compresses unless deflate would not pay: an already-compressed JPEG
    /// spends CPU to get bigger, and a short file spends a five-byte deflate
    /// header to save nothing.
    pub fn add(&mut self, name: &str, body: &[u8]) -> Result<()> {
        // The same rule the reader enforces, applied here so that a name no
        // reader would accept can never get into an archive in the first
        // place. Everything upstream builds names out of `text::safe_name`,
        // which is where such a name is prevented rather than caught; this is
        // the backstop, and it fails the export loudly instead of producing a
        // file that will not open.
        if !is_safe_path(name) {
            return Err(Error::Invalid(format!("{name} is not a path an archive may contain")));
        }
        let crc = crc32fast::hash(body);
        let (method, payload) = match squeeze(body) {
            Some(deflated) => (DEFLATED, deflated),
            None => (STORED, body.to_vec()),
        };

        let entry = Entry {
            name: name.to_string(),
            offset: self.offset,
            compressed: payload.len() as u64,
            uncompressed: body.len() as u64,
            crc,
            method,
        };

        // A local header states the sizes in 32 bits, and an entry too big
        // for that says so with the sentinel and puts the truth in an extra
        // field. Written up front rather than in a trailing descriptor,
        // because the sizes are already known here.
        // `>=`, not `>`. A value of exactly `0xFFFFFFFF` written into a
        // 32-bit field *is* the sentinel, so an entry that size must escape
        // into ZIP64 too -- otherwise the reader goes looking for an extra
        // field that was never written.
        let big = entry.compressed >= ZIP64_MARK as u64 || entry.uncompressed >= ZIP64_MARK as u64;
        let extra: Vec<u8> = if big {
            let mut e = Vec::with_capacity(20);
            e.extend_from_slice(&0x0001u16.to_le_bytes());
            e.extend_from_slice(&16u16.to_le_bytes());
            e.extend_from_slice(&entry.uncompressed.to_le_bytes());
            e.extend_from_slice(&entry.compressed.to_le_bytes());
            e
        } else {
            Vec::new()
        };

        let name_bytes = name.as_bytes();
        let mut header = Vec::with_capacity(30 + name_bytes.len() + extra.len());
        header.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        header.extend_from_slice(&if big { 45u16 } else { 20u16 }.to_le_bytes());
        header.extend_from_slice(&UTF8_NAMES.to_le_bytes());
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&self.stamp.1.to_le_bytes());
        header.extend_from_slice(&self.stamp.0.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&clamp32(entry.compressed, big).to_le_bytes());
        header.extend_from_slice(&clamp32(entry.uncompressed, big).to_le_bytes());
        header.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        header.extend_from_slice(name_bytes);
        header.extend_from_slice(&extra);

        self.out.write_all(&header)?;
        self.out.write_all(&payload)?;
        self.offset += header.len() as u64 + payload.len() as u64;
        self.entries.push(entry);
        Ok(())
    }

    /// Write the central directory and finish. Consumes the writer, because
    /// an archive whose directory was never written is not an archive.
    pub fn finish(mut self) -> Result<W> {
        let start = self.offset;
        for entry in &self.entries {
            // The same sentinel rule as the local header, over one more
            // field: an entry whose *offset* is past 4 GiB, which is how a
            // large archive's later files end up here even when each of them
            // is small.
            let mut extra = Vec::new();
            let big_sizes =
                entry.compressed > ZIP64_MARK as u64 || entry.uncompressed > ZIP64_MARK as u64;
            let big_offset = entry.offset > ZIP64_MARK as u64;
            if big_sizes || big_offset {
                let mut body = Vec::with_capacity(24);
                if big_sizes {
                    body.extend_from_slice(&entry.uncompressed.to_le_bytes());
                    body.extend_from_slice(&entry.compressed.to_le_bytes());
                }
                if big_offset {
                    body.extend_from_slice(&entry.offset.to_le_bytes());
                }
                extra.extend_from_slice(&0x0001u16.to_le_bytes());
                extra.extend_from_slice(&(body.len() as u16).to_le_bytes());
                extra.extend_from_slice(&body);
            }

            let name = entry.name.as_bytes();
            let mut record = Vec::with_capacity(46 + name.len() + extra.len());
            record.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
            // "Made by" a Unix system, zip 4.5. The high byte matters only
            // for the external attributes below.
            record.extend_from_slice(&0x031Eu16.to_le_bytes());
            record.extend_from_slice(&if extra.is_empty() { 20u16 } else { 45u16 }.to_le_bytes());
            record.extend_from_slice(&UTF8_NAMES.to_le_bytes());
            record.extend_from_slice(&entry.method.to_le_bytes());
            record.extend_from_slice(&self.stamp.1.to_le_bytes());
            record.extend_from_slice(&self.stamp.0.to_le_bytes());
            record.extend_from_slice(&entry.crc.to_le_bytes());
            record.extend_from_slice(&clamp32(entry.compressed, big_sizes).to_le_bytes());
            record.extend_from_slice(&clamp32(entry.uncompressed, big_sizes).to_le_bytes());
            record.extend_from_slice(&(name.len() as u16).to_le_bytes());
            record.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes()); // comment length
            record.extend_from_slice(&0u16.to_le_bytes()); // disk number
            record.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            // 0o644 in the high two bytes, which is what makes an unzipped
            // export readable rather than arriving mode 000 on Unix.
            record.extend_from_slice(&0x81A4_0000u32.to_le_bytes());
            record.extend_from_slice(&clamp32(entry.offset, big_offset).to_le_bytes());
            record.extend_from_slice(name);
            record.extend_from_slice(&extra);

            self.out.write_all(&record)?;
            self.offset += record.len() as u64;
        }

        let size = self.offset - start;
        let count = self.entries.len();
        // `>=` again, and for the count as well: an archive of exactly 65,535
        // files would otherwise write `0xFFFF` -- the sentinel meaning "the
        // real count is in the ZIP64 record" -- with no ZIP64 record after it.
        let zip64 =
            count >= u16::MAX as usize || start >= ZIP64_MARK as u64 || size >= ZIP64_MARK as u64;

        if zip64 {
            let eocd_at = self.offset;
            let mut record = Vec::with_capacity(56);
            record.extend_from_slice(&ZIP64_EOCD_SIG.to_le_bytes());
            record.extend_from_slice(&44u64.to_le_bytes()); // size of what follows
            record.extend_from_slice(&0x031Eu16.to_le_bytes());
            record.extend_from_slice(&45u16.to_le_bytes());
            record.extend_from_slice(&0u32.to_le_bytes()); // this disk
            record.extend_from_slice(&0u32.to_le_bytes()); // disk with the directory
            record.extend_from_slice(&(count as u64).to_le_bytes());
            record.extend_from_slice(&(count as u64).to_le_bytes());
            record.extend_from_slice(&size.to_le_bytes());
            record.extend_from_slice(&start.to_le_bytes());
            record.extend_from_slice(&ZIP64_LOCATOR_SIG.to_le_bytes());
            record.extend_from_slice(&0u32.to_le_bytes());
            record.extend_from_slice(&eocd_at.to_le_bytes());
            record.extend_from_slice(&1u32.to_le_bytes()); // total disks
            self.out.write_all(&record)?;
            self.offset += record.len() as u64;
        }

        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&EOCD_SIG.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        let shown = if zip64 { u16::MAX } else { count as u16 };
        eocd.extend_from_slice(&shown.to_le_bytes());
        eocd.extend_from_slice(&shown.to_le_bytes());
        eocd.extend_from_slice(&clamp32(size, zip64).to_le_bytes());
        eocd.extend_from_slice(&clamp32(start, zip64).to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out.write_all(&eocd)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Deflate `body`, or `None` when storing it is the better answer.
fn squeeze(body: &[u8]) -> Option<Vec<u8>> {
    // Below this, the deflate framing costs more than the compression saves
    // on anything except pathological input, and an export is thousands of
    // small files.
    if body.len() < 64 {
        return None;
    }
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).ok()?;
    let out = encoder.finish().ok()?;
    // Photographs and video are already compressed; deflating them spends
    // time to add a few bytes. Measuring beats guessing from the extension.
    (out.len() < body.len()).then_some(out)
}

fn clamp32(value: u64, escaped: bool) -> u32 {
    if escaped { ZIP64_MARK } else { value as u32 }
}

/// MS-DOS date and time: (date, time), each packed into 16 bits.
///
/// The epoch is 1980 and the resolution is two seconds. Anything before 1980
/// clamps to it rather than wrapping into a date from the future, which is
/// what an unchecked subtraction here would produce.
fn dos_stamp(at: jiff::civil::DateTime) -> (u16, u16) {
    let year = (at.year() as i32 - 1980).clamp(0, 127) as u16;
    let date = (year << 9) | ((at.month() as u16) << 5) | at.day() as u16;
    let time = ((at.hour() as u16) << 11) | ((at.minute() as u16) << 5) | (at.second() as u16 / 2);
    (date, time)
}

// ---- reading ------------------------------------------------------------

/// An archive held in memory, indexed by the paths inside it.
///
/// Reading is eager: the central directory is walked once and every entry is
/// inflated into a map. An archive arrives here having already been held whole
/// in memory to get this far -- a window uploaded it, or the command line read
/// it off disk -- so laziness would save nothing and would leave the caller
/// handling an error from every lookup.
#[derive(Debug)]
pub struct Reader {
    files: BTreeMap<String, Vec<u8>>,
}

/// The most an archive may inflate to.
///
/// A zip bomb is forty kilobytes that decompresses to a terabyte, and the
/// only defence is a ceiling. Ten gigabytes is far past any real export --
/// the vault it came from would not fit on the machine -- and far short of
/// what a bomb is aiming for.
const MAX_INFLATED: u64 = 10 * 1024 * 1024 * 1024;

impl Reader {
    /// Parse an archive, inflating everything in it.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        let (dir_at, count) = locate_directory(bytes)?;
        let mut files = BTreeMap::new();
        let mut cursor = dir_at as usize;
        let mut total: u64 = 0;

        for _ in 0..count {
            let record = bytes.get(cursor..cursor + 46).ok_or_else(truncated)?;
            if u32(record, 0) != CENTRAL_SIG {
                return Err(Error::Invalid("this file's directory is damaged".into()));
            }
            let flags = u16(record, 8);
            let method = u16(record, 10);
            let crc = u32(record, 16);
            let name_len = u16(record, 28) as usize;
            let extra_len = u16(record, 30) as usize;
            let comment_len = u16(record, 32) as usize;
            let mut compressed = u32(record, 20) as u64;
            let mut uncompressed = u32(record, 24) as u64;
            let mut offset = u32(record, 42) as u64;

            let name_at = cursor + 46;
            let name =
                std::str::from_utf8(bytes.get(name_at..name_at + name_len).ok_or_else(truncated)?)
                    .map_err(|_| {
                        Error::Invalid("a file in this archive has a name that is not text".into())
                    })?
                    .to_string();

            let extra = bytes
                .get(name_at + name_len..name_at + name_len + extra_len)
                .ok_or_else(truncated)?;
            read_zip64_extra(extra, &mut uncompressed, &mut compressed, &mut offset);
            cursor = name_at + name_len + extra_len + comment_len;

            // Directories are entries too, and carry no bytes worth keeping.
            if name.ends_with('/') {
                continue;
            }
            if flags & ENCRYPTED != 0 {
                return Err(Error::Invalid(format!(
                    "{name} is password-protected; Every Day cannot read an encrypted archive"
                )));
            }
            if !is_safe_path(&name) {
                return Err(Error::Invalid(format!(
                    "{name} is not a path this archive may contain"
                )));
            }
            total = total.saturating_add(uncompressed);
            if total > MAX_INFLATED {
                return Err(Error::Invalid(
                    "this archive expands to more than Every Day will read at once".into(),
                ));
            }

            let body = read_entry(bytes, offset, method, compressed, uncompressed, &name)?;
            if crc32fast::hash(&body) != crc {
                return Err(Error::Invalid(format!("{name} is damaged")));
            }
            files.insert(name, body);
        }
        Ok(Self { files })
    }

    /// Build one directly. For tests, and for a caller assembling files by
    /// hand rather than from bytes.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Self {
        Self { files }
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.files.get(name).map(Vec::as_slice)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// Every path in the archive, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The paths under `prefix`, which must end in `/`, paired with what is
    /// left of the path after it.
    pub fn under<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (&'a str, &'a [u8])> {
        self.files
            .iter()
            .filter_map(move |(name, body)| Some((name.strip_prefix(prefix)?, body.as_slice())))
    }
}

fn truncated() -> Error {
    Error::Invalid("this archive is incomplete".into())
}

/// Where the central directory starts, and how many records are in it.
fn locate_directory(bytes: &[u8]) -> Result<(u64, u64)> {
    // The end record is last, but may be followed by a comment of up to 64 KiB,
    // so it is found by scanning backwards for its signature rather than by
    // arithmetic.
    let window = bytes.len().saturating_sub(u16::MAX as usize + 22);
    let eocd = (window..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| u32(bytes, i) == EOCD_SIG)
        .ok_or_else(|| Error::Invalid("this file is not a zip archive".into()))?;

    let mut count = u16(bytes, eocd + 10) as u64;
    let mut offset = u32(bytes, eocd + 16) as u64;

    // Either sentinel means the real values are in the ZIP64 record, which
    // sits immediately before the locator, which sits immediately before this.
    if count == u16::MAX as u64 || offset == ZIP64_MARK as u64 {
        let locator = eocd.checked_sub(20).ok_or_else(truncated)?;
        if u32(bytes, locator) != ZIP64_LOCATOR_SIG {
            return Err(Error::Invalid("this archive says it is large and is not".into()));
        }
        let zip64_at = u64_at(bytes, locator + 8) as usize;
        if u32(bytes, zip64_at) != ZIP64_EOCD_SIG {
            return Err(truncated());
        }
        count = u64_at(bytes, zip64_at + 32);
        offset = u64_at(bytes, zip64_at + 48);
    }
    Ok((offset, count))
}

/// Overwrite the fields that carried a sentinel with the ZIP64 extra field's
/// values, in the order the specification puts them.
fn read_zip64_extra(
    mut extra: &[u8],
    uncompressed: &mut u64,
    compressed: &mut u64,
    offset: &mut u64,
) {
    while extra.len() >= 4 {
        let id = u16(extra, 0);
        let len = u16(extra, 2) as usize;
        let Some(body) = extra.get(4..4 + len) else { return };
        if id == 0x0001 {
            // Only the fields that were escaped are present, and they appear
            // in this order. Reading them positionally is why each is guarded
            // by the sentinel it replaces.
            let mut at = 0;
            for field in [&mut *uncompressed, &mut *compressed, &mut *offset] {
                if *field == ZIP64_MARK as u64 && body.len() >= at + 8 {
                    *field = u64_at(body, at);
                    at += 8;
                }
            }
            return;
        }
        extra = &extra[4 + len..];
    }
}

fn read_entry(
    bytes: &[u8],
    offset: u64,
    method: u16,
    compressed: u64,
    uncompressed: u64,
    name: &str,
) -> Result<Vec<u8>> {
    let at = offset as usize;
    let header = bytes.get(at..at + 30).ok_or_else(truncated)?;
    if u32(header, 0) != LOCAL_SIG {
        return Err(Error::Invalid(format!("{name} is not where the archive says it is")));
    }
    // The local header's own name and extra lengths, not the directory's:
    // archivers routinely write different extra fields in the two places.
    let start = at + 30 + u16(header, 26) as usize + u16(header, 28) as usize;
    let raw = bytes.get(start..start + compressed as usize).ok_or_else(truncated)?;

    match method {
        STORED => Ok(raw.to_vec()),
        DEFLATED => {
            // Reserved modestly rather than at the size the header claims. The
            // claim is an attacker's to make: a two-hundred-byte file can say
            // it expands to a hundred gigabytes, and this crate is built with
            // `panic = "abort"`, so a refused allocation is not an error that
            // can be reported -- it is the application gone. Growth is bounded
            // by `take` below and by `MAX_INFLATED` above.
            const RESERVE: u64 = 1 << 20;
            let mut out = Vec::with_capacity(uncompressed.min(RESERVE) as usize);
            flate2::read::DeflateDecoder::new(raw)
                // Bounded by what the directory promised, so a lying header
                // cannot make this allocate without limit.
                .take(uncompressed)
                .read_to_end(&mut out)
                .map_err(|_| Error::Invalid(format!("{name} could not be decompressed")))?;
            Ok(out)
        }
        other => Err(Error::Invalid(format!(
            "{name} uses compression method {other}, which Every Day does not read"
        ))),
    }
}

/// Is this a name an archive may contain?
///
///
/// Rejects absolute paths, Windows drive letters, backslashes and any `..`
/// segment. Nothing here extracts to a filesystem, so this is belt to the
/// braces of holding the files in a map -- but it is the check whose absence
/// is the classic archive vulnerability, and it costs four lines.
pub fn is_safe_path(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('\0')
        && name.as_bytes().get(1).is_none_or(|&c| c != b':')
}

fn u16(bytes: &[u8], at: usize) -> u16 {
    bytes.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .map_or(0, |b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> jiff::civil::DateTime {
        jiff::civil::date(2026, 9, 10).at(11, 30, 0, 0)
    }

    fn roundtrip(files: &[(&str, &[u8])]) -> Reader {
        let mut w = Writer::new(Vec::new(), stamp());
        for (name, body) in files {
            w.add(name, body).unwrap();
        }
        Reader::open(&w.finish().unwrap()).unwrap()
    }

    #[test]
    fn a_file_comes_back_as_it_went_in() {
        let long = "the quick brown fox ".repeat(500);
        let r = roundtrip(&[("a/one.md", b"hello"), ("a/two.md", long.as_bytes())]);
        assert_eq!(r.get("a/one.md").unwrap(), b"hello");
        assert_eq!(r.get("a/two.md").unwrap(), long.as_bytes());
    }

    #[test]
    fn both_methods_are_read_back() {
        // Short enough to be stored, and long enough to be deflated, so one
        // archive exercises both paths.
        let r = roundtrip(&[("s", b"tiny"), ("d", "compress me ".repeat(100).as_bytes())]);
        assert_eq!(r.get("s").unwrap(), b"tiny");
        assert_eq!(r.get("d").unwrap(), "compress me ".repeat(100).as_bytes());
    }

    #[test]
    fn the_same_files_make_the_same_archive() {
        let build = || {
            let mut w = Writer::new(Vec::new(), stamp());
            w.add("one.md", b"the same bytes every time").unwrap();
            w.finish().unwrap()
        };
        assert_eq!(build(), build(), "an export is not reproducible");
    }

    #[test]
    fn unicode_names_survive() {
        let r = roundtrip(&[("journal/Caf\u{e9} \u{2014} \u{65e5}\u{8a18}.md", b"x")]);
        assert!(r.contains("journal/Caf\u{e9} \u{2014} \u{65e5}\u{8a18}.md"));
    }

    #[test]
    fn under_gives_one_parts_files_and_no_others() {
        let r = roundtrip(&[("journal/a.md", b"1"), ("notes/b.md", b"2"), ("journal/c.md", b"3")]);
        let mut names: Vec<&str> = r.under("journal/").map(|(n, _)| n).collect();
        names.sort_unstable();
        assert_eq!(names, ["a.md", "c.md"]);
    }

    #[test]
    fn a_file_that_is_not_an_archive_says_so() {
        let e = Reader::open(b"this is a text file").unwrap_err();
        assert_eq!(e.code(), "invalid");
        assert!(e.to_string().contains("not a zip"), "{e}");
    }

    #[test]
    fn a_truncated_archive_is_refused_rather_than_half_read() {
        let mut w = Writer::new(Vec::new(), stamp());
        w.add("one.md", b"hello").unwrap();
        let bytes = w.finish().unwrap();
        assert!(Reader::open(&bytes[..bytes.len() - 8]).is_err());
    }

    #[test]
    fn damage_to_a_file_is_caught_by_its_checksum() {
        let mut w = Writer::new(Vec::new(), stamp());
        w.add("one.md", b"hello there, this is the body").unwrap();
        let mut bytes = w.finish().unwrap();
        // A byte inside the stored payload, past the local header and name.
        bytes[40] ^= 0xFF;
        let e = Reader::open(&bytes).unwrap_err();
        assert!(e.to_string().contains("damaged"), "{e}");
    }

    #[test]
    fn an_archive_of_exactly_the_sentinel_count_still_opens() {
        // 65,535 entries writes `0xFFFF` into the end record, which is the
        // value that means "look in the ZIP64 record" -- so this is the count
        // at which one has to be written, not the count after it.
        let mut w = Writer::new(Vec::new(), stamp());
        for i in 0..u16::MAX as usize {
            w.add(&format!("f/{i}"), b"x").unwrap();
        }
        let bytes = w.finish().unwrap();
        let r = Reader::open(&bytes).expect("an archive at the boundary would not reopen");
        assert_eq!(r.names().count(), u16::MAX as usize);
    }

    #[test]
    fn a_name_no_reader_would_accept_is_refused_on_the_way_in() {
        let mut w = Writer::new(Vec::new(), stamp());
        let e = w.add("journal/Wait...-what.md", b"x").unwrap_err();
        assert_eq!(e.code(), "invalid");
        // And the archive is still whole: the refusal is the whole effect.
        w.add("journal/fine.md", b"x").unwrap();
        let r = Reader::open(&w.finish().unwrap()).unwrap();
        assert_eq!(r.names().collect::<Vec<_>>(), ["journal/fine.md"]);
    }

    #[test]
    fn a_lying_header_does_not_get_to_choose_an_allocation() {
        let mut w = Writer::new(Vec::new(), stamp());
        w.add("big", &"compress me ".repeat(200).into_bytes()).unwrap();
        let mut bytes = w.finish().unwrap();
        // Rewrite the *central directory's* uncompressed size to something
        // enormous. It is below MAX_INFLATED, so the guard above does not
        // catch it, and the old code reserved it up front.
        let dir = bytes.windows(4).rposition(|w| w == CENTRAL_SIG.to_le_bytes()).unwrap();
        bytes[dir + 24..dir + 28].copy_from_slice(&(8u32 << 28).to_le_bytes());
        // Whatever it answers, it must not have tried to allocate 2 GiB.
        let _ = Reader::open(&bytes);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_archive_is_refused() {
        for name in ["../secrets", "/etc/passwd", "a/../../b", "C:/x", "a\\b"] {
            assert!(!is_safe_path(name), "{name} was accepted");
        }
        assert!(is_safe_path("journal/2026/a.md"));
    }

    #[test]
    fn dates_before_the_dos_epoch_do_not_wrap() {
        let (date, _) = dos_stamp(jiff::civil::date(1970, 1, 1).at(0, 0, 0, 0));
        assert_eq!(date >> 9, 0, "a 1970 stamp wrapped into a future year");
    }
}
