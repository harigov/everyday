//! What a file says about itself: a photo's EXIF (when, on what, where, how
//! it is turned), a film's movie header (how long, how big, when), a PDF's
//! page count. Read with `kamadak-exif` and `mp4`, both pure Rust, and never
//! written back into the file.
//!
//! Everything here is best effort and total: a file that says nothing, says
//! it wrongly, or is not what its name claims answers an empty
//! [`MediaInfo`], never an error and never a panic.
//!
//! # Bounded, because the file is not ours
//!
//! The files folder is where Syncthing and a phone's camera roll land, so any
//! file in it may be damaged, hostile, or several gigabytes of film. Nothing
//! here reads a file whole: EXIF is looked for through a [`Window`] that ends
//! where a camera could have put it, a film's header is found by hopping from
//! box to box and reading only the `moov` box, and a PDF's page count comes
//! from its first and last megabyte. The parsers are run under
//! `catch_unwind` as a last defence (release builds unwind), so a bug in one
//! of them costs a file its metadata, not the application its process.
//!
//! # Dimensions are the ones the picture is seen at
//!
//! A phone holds a portrait photograph sideways and writes EXIF orientation 6
//! to say so. [`MediaInfo::width`] and `height` are what a viewer shows, so
//! they are swapped for orientations 5 to 8: a grid that lays photographs out
//! by their proportions then agrees with the (already upright) thumbnail.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use exif::{Exif, Field, In, Tag, Value};
use jiff::Timestamp;
use jiff::civil;
use jiff::tz::{Offset, TimeZone};
use karmayoga_core::files::{GeoPoint, MediaInfo};

/// How much of the start of a file is looked at to tell what it is.
const HEAD: u64 = 4096;

/// How far into a JPEG its EXIF is looked for. A camera writes it before the
/// picture, in the first few dozen kilobytes; this is only the point at
/// which a file with none stops being read.
const JPEG_EXIF_WINDOW: u64 = 8 * 1024 * 1024;

/// How far into any other picture its EXIF is looked for. Roomy, because a
/// TIFF-based RAW file is read whole by `kamadak-exif` (its tags point
/// anywhere in the file), and a HEIC keeps its EXIF in the data box, after
/// the pictures.
const EXIF_WINDOW: u64 = 256 * 1024 * 1024;

/// The largest file whose header `image` is asked for the size of: it reads a
/// JPEG whole to find it, so a "JPEG" of several gigabytes is not asked.
const HEADER_READ_MAX: u64 = 512 * 1024 * 1024;

/// The largest `moov` box read. A film's is a few megabytes -- two hours of
/// 4K is about two; this is what it takes to be sure of a long recording.
const MAX_MOOV: u64 = 128 * 1024 * 1024;

/// How many boxes are hopped over in search of `moov`. A camera file has
/// four or five before it; a fragmented one has `moov` first.
const TOP_LEVEL_BOXES: usize = 256;

/// How much of each end of a PDF is searched for its page tree.
const PDF_SCAN: u64 = 1024 * 1024;

/// Seconds from 1 January 1904, where a movie header counts from, to the
/// Unix epoch.
const UNIX_FROM_1904: u64 = 2_082_844_800;

/// Read what `path` says about itself. `mime` is what the catalog believes
/// the file is; the reader also looks at the first bytes.
pub fn probe(path: &Path, mime: &str) -> MediaInfo {
    guarded(|| {
        let file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        Some(read(&mut BufReader::new(file), size, mime))
    })
    .flatten()
    .unwrap_or_default()
}

/// The same, from bytes in memory: a sealed file, opened.
pub fn probe_bytes(bytes: &[u8], mime: &str) -> MediaInfo {
    guarded(|| read(&mut io::Cursor::new(bytes), bytes.len() as u64, mime)).unwrap_or_default()
}

/// A better MIME type than the extension gave, when the first bytes say
/// (a `.jpg` that is really a HEIC). `None` when they do not.
///
/// Only the first bytes are looked at, so this is a hint where a format is a
/// container for others: every Office document, EPUB and JAR starts as a ZIP
/// and answers `application/zip`, and a TIFF-based RAW file answers
/// `image/tiff`. A caller that already has a more particular type from the
/// file's name should keep it.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let at = |off: usize, pattern: &[u8]| head.get(off..off + pattern.len()) == Some(pattern);

    if at(0, &[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if at(0, b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if at(0, b"GIF87a") || at(0, b"GIF89a") {
        return Some("image/gif");
    }
    if at(0, b"RIFF") {
        if at(8, b"WEBP") {
            return Some("image/webp");
        }
        if at(8, b"WAVE") {
            return Some("audio/wav");
        }
        if at(8, b"AVI ") {
            return Some("video/x-msvideo");
        }
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") || at(0, b"II+\0") || at(0, b"MM\0+") {
        return Some("image/tiff");
    }
    if at(4, b"ftyp") {
        return iso_brand(head.get(8..12)?);
    }
    if at(0, b"%PDF-") {
        return Some("application/pdf");
    }
    if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        // The EBML header names its document type in the first few bytes.
        let early = &head[..head.len().min(64)];
        let webm = early.windows(4).any(|w| w == b"webm");
        return Some(if webm { "video/webm" } else { "video/x-matroska" });
    }
    if at(0, b"OggS") {
        return Some("audio/ogg");
    }
    if at(0, b"fLaC") {
        return Some("audio/flac");
    }
    if at(0, b"ID3") {
        return Some("audio/mpeg");
    }
    if let Some(audio) = mpeg_audio(head) {
        return Some(audio);
    }
    if at(0, b"PK\x03\x04") || at(0, b"PK\x05\x06") {
        return Some("application/zip");
    }
    if at(0, b"BM") && head.len() >= 18 {
        // Two letters are not much to go on; the size of the header that
        // follows the file header is one of a handful of values.
        let dib = u32::from_le_bytes([head[14], head[15], head[16], head[17]]);
        if [12, 40, 52, 56, 64, 108, 124].contains(&dib) {
            return Some("image/bmp");
        }
    }
    None
}

/// What an ISO base media file (`ftyp` box) is, from its major brand.
fn iso_brand(brand: &[u8]) -> Option<&'static str> {
    Some(match brand {
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" => {
            "image/heic"
        }
        b"mif1" | b"msf1" => "image/heif",
        b"avif" | b"avis" => "image/avif",
        b"crx " => "image/x-canon-cr3",
        b"qt  " => "video/quicktime",
        b"M4A " | b"M4B " | b"M4P " => "audio/mp4",
        b"M4V " | b"M4VH" | b"M4VP" => "video/x-m4v",
        b if b.starts_with(b"3g2") => "video/3gpp2",
        b if b.starts_with(b"3g") => "video/3gpp",
        b if b.starts_with(b"iso") => "video/mp4",
        b"mp41" | b"mp42" | b"mp71" | b"avc1" | b"dash" | b"f4v " | b"mmp4" | b"XAVC" | b"MSNV"
        | b"NDAS" => "video/mp4",
        _ => return None,
    })
}

/// An MP3 or an AAC stream with no tag in front of it: the frame header's
/// eleven set bits, then the fields around them which are never all ones or
/// reserved.
fn mpeg_audio(head: &[u8]) -> Option<&'static str> {
    let &[0xFF, second, third, ..] = head else {
        return None;
    };
    if second & 0xE0 != 0xE0 {
        return None;
    }
    // ADTS, AAC's frame: the layer field is zero, which MPEG audio reserves.
    if second & 0xF6 == 0xF0 {
        return Some("audio/aac");
    }
    let version_reserved = (second >> 3) & 3 == 1;
    let layer_reserved = (second >> 1) & 3 == 0;
    let bad_bitrate = third >> 4 == 0xF;
    let bad_rate = (third >> 2) & 3 == 3;
    (!version_reserved && !layer_reserved && !bad_bitrate && !bad_rate).then_some("audio/mpeg")
}

/// Run `work`, answering `None` if it panics. Release builds unwind, so this
/// turns a bug in a parser of somebody else's bytes into a file with no
/// metadata. Shared with [`super::thumb`].
pub(super) fn guarded<T>(work: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(work))
        .inspect_err(|_| tracing::warn!("reading a file panicked; carrying on without it"))
        .ok()
}

/// A file's metadata, from a reader over it. The first bytes decide what it
/// is, ahead of `mime`, which is only the name's guess.
fn read<R: BufRead + Seek>(reader: &mut R, size: u64, mime: &str) -> MediaInfo {
    let mut head = Vec::new();
    if reader.by_ref().take(HEAD).read_to_end(&mut head).is_err() || reader.rewind().is_err() {
        return MediaInfo::default();
    }
    match Family::of(sniff(&head).unwrap_or(mime)) {
        Family::Photo => photo(reader, &head, size),
        Family::Film => film(reader, size),
        Family::Pdf => MediaInfo { pages: pdf_pages(reader, size), ..MediaInfo::default() },
        Family::Other => MediaInfo::default(),
    }
}

/// Which reader a file goes to.
enum Family {
    Photo,
    Film,
    Pdf,
    Other,
}

impl Family {
    fn of(mime: &str) -> Self {
        let essence = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        match essence.as_str() {
            "application/pdf" => Family::Pdf,
            // The ISO base media family: what `mp4` and the hand reader below
            // understand. Matroska, AVI and the rest say nothing here.
            "video/mp4" | "video/quicktime" | "video/x-m4v" | "video/3gpp" | "video/3gpp2"
            | "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/x-m4b" | "audio/3gpp"
            | "audio/3gpp2" => Family::Film,
            other if other.starts_with("image/") => Family::Photo,
            _ => Family::Other,
        }
    }
}

// -- Photographs ------------------------------------------------------------

/// A picture's metadata: its EXIF, and its size from its header.
fn photo<R: BufRead + Seek>(reader: &mut R, head: &[u8], size: u64) -> MediaInfo {
    let mut info = MediaInfo::default();
    if let Some(exif) = read_exif(reader, head, size) {
        describe(&mut info, &exif);
    }
    // The picture's own header is the better witness of its size where
    // `image` can read one: EXIF's pixel dimensions are copied by software
    // that edits the picture and forgets to update them.
    if size <= HEADER_READ_MAX
        && reader.rewind().is_ok()
        && let Some((width, height)) = header_size(reader)
    {
        info.width = Some(width);
        info.height = Some(height);
    }
    if matches!(info.orientation, Some(5..=8)) {
        std::mem::swap(&mut info.width, &mut info.height);
    }
    info
}

/// The pixel size in a picture's header, for the formats `image` reads.
fn header_size<R: BufRead + Seek>(reader: &mut R) -> Option<(u32, u32)> {
    let (width, height) =
        image::ImageReader::new(reader).with_guessed_format().ok()?.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// The EXIF of the picture `reader` is over, as far as it can be read: JPEG,
/// TIFF (and the RAW files built on it), HEIF, AVIF, PNG and WebP.
///
/// `head` is the file's first bytes and `size` its length. A file whose EXIF
/// is damaged part way answers what could be read -- a broken maker note
/// costs the maker note, not the date.
pub(super) fn read_exif<R: BufRead + Seek>(reader: &mut R, head: &[u8], size: u64) -> Option<Exif> {
    reader.rewind().ok()?;
    let reach = if head.starts_with(&[0xFF, 0xD8]) { JPEG_EXIF_WINDOW } else { EXIF_WINDOW };
    let mut window = Window::new(&mut *reader, size.min(reach));
    let mut parser = exif::Reader::new();
    parser.continue_on_error(true);
    parser
        .read_from_container(&mut window)
        .or_else(|error| error.distill_partial_result(|_| {}))
        .ok()
}

/// Everything a camera wrote that [`MediaInfo`] has a place for.
fn describe(info: &mut MediaInfo, exif: &Exif) {
    let taken = moment(exif, Tag::DateTimeOriginal, Tag::OffsetTimeOriginal)
        .or_else(|| moment(exif, Tag::DateTimeDigitized, Tag::OffsetTimeDigitized));
    if let Some((local, at)) = taken {
        info.taken_local = Some(local);
        info.taken_at = at;
    }
    info.camera_make = text(exif, Tag::Make);
    info.camera_model = text(exif, Tag::Model);
    info.lens = text(exif, Tag::LensModel);
    info.f_number = number(exif, Tag::FNumber).filter(|v| *v > 0.0).map(|v| v as f32);
    info.focal_length_mm = number(exif, Tag::FocalLength).filter(|v| *v > 0.0).map(|v| v as f32);
    if let Some(Field { value: Value::Rational(times), .. }) =
        exif.get_field(Tag::ExposureTime, In::PRIMARY)
    {
        info.exposure_time = times.first().and_then(exposure);
    }
    // 65535 is what a camera writes for "more than I can say".
    info.iso = uint(exif, Tag::PhotographicSensitivity).filter(|n| (1..65535).contains(n));
    info.orientation = uint(exif, Tag::Orientation)
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| (1..=8).contains(n));
    info.gps = gps(exif);
    info.width = uint(exif, Tag::PixelXDimension).or_else(|| uint(exif, Tag::ImageWidth));
    info.height = uint(exif, Tag::PixelYDimension).or_else(|| uint(exif, Tag::ImageLength));
    info.width = info.width.filter(|n| *n > 0);
    info.height = info.height.filter(|n| *n > 0);
}

/// When a picture was taken, as the camera wrote it (`2024-03-01T14:22:05`)
/// and, where it can be worked out, as an instant: with the offset the
/// camera wrote beside it when it wrote one (most written since 2016 do),
/// else read in this computer's time zone, which is the best guess a photo
/// from the person's own camera allows.
///
/// `None` for a date that is not one -- cameras with an unset clock write
/// `0000:00:00 00:00:00`, and a blank is spaces.
fn moment(exif: &Exif, when: Tag, zone: Tag) -> Option<(String, Option<Timestamp>)> {
    let Value::Ascii(parts) = &exif.get_field(when, In::PRIMARY)?.value else {
        return None;
    };
    let mut written = exif::DateTime::from_ascii(parts.first()?).ok()?;
    if let Some(Field { value: Value::Ascii(parts), .. }) = exif.get_field(zone, In::PRIMARY)
        && let Some(raw) = parts.first()
    {
        // A malformed offset leaves the time without one.
        let _ = written.parse_offset(raw);
    }
    if written.year < 1900 {
        return None;
    }
    let wall = civil::DateTime::new(
        i16::try_from(written.year).ok()?,
        i8::try_from(written.month).ok()?,
        i8::try_from(written.day).ok()?,
        i8::try_from(written.hour).ok()?,
        i8::try_from(written.minute).ok()?,
        i8::try_from(written.second).ok()?,
        0,
    )
    .ok()?;
    let local = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        written.year, written.month, written.day, written.hour, written.minute, written.second
    );
    let offset =
        written.offset.and_then(|minutes| Offset::from_seconds(i32::from(minutes) * 60).ok());
    let at = match offset {
        Some(offset) => offset.to_timestamp(wall).ok(),
        None => TimeZone::system().to_timestamp(wall).ok(),
    };
    Some((local, at))
}

/// An ASCII field, trimmed of the padding cameras fill it with. `None` when
/// it is empty or not text.
fn text(exif: &Exif, tag: Tag) -> Option<String> {
    let Value::Ascii(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    let raw = String::from_utf8_lossy(parts.first()?);
    let trimmed = raw.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The first value of a rational field, as a number.
fn number(exif: &Exif, tag: Tag) -> Option<f64> {
    match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Rational(parts) => rational(parts.first()?),
        _ => None,
    }
}

/// The first value of an unsigned integer field, whichever width it was
/// written in.
fn uint(exif: &Exif, tag: Tag) -> Option<u32> {
    exif.get_field(tag, In::PRIMARY)?.value.get_uint(0)
}

/// A rational as a number, unless it divides by zero -- which cameras write
/// for "unknown".
fn rational(r: &exif::Rational) -> Option<f64> {
    (r.denom != 0).then(|| r.to_f64()).filter(|v| v.is_finite())
}

/// An exposure time as a photographer says it: `1/250`, and for a long one
/// `2.5` or `30`. A fraction is kept as one where the reciprocal is whole
/// (within a percent: `1/3` is often written `33333/100000`), and is a
/// decimal where it is not (`0.4`, which nobody says as `1/2.5`).
fn exposure(time: &exif::Rational) -> Option<String> {
    if time.num == 0 || time.denom == 0 {
        return None;
    }
    let seconds = time.to_f64();
    if seconds >= 1.0 {
        return Some(trimmed(seconds, 1));
    }
    let divisor = 1.0 / seconds;
    if divisor.round() >= 2.0 && (divisor - divisor.round()).abs() <= divisor * 0.01 {
        return Some(format!("1/{}", divisor.round() as u64));
    }
    Some(trimmed(seconds, 2))
}

/// `value` with at most `places` decimals and no trailing zeros.
fn trimmed(value: f64, places: usize) -> String {
    let fixed = format!("{value:.places$}");
    if fixed.contains('.') {
        fixed.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        fixed
    }
}

/// Where a picture was taken: latitude and longitude from their
/// degrees-minutes-seconds and the letter beside each (south and west are
/// negative), and the altitude with whether it is below sea level.
///
/// `None` when either coordinate is missing or impossible, and for 0, 0: that
/// is what a camera writes when it has no fix yet, and the middle of the
/// Atlantic is not where anybody took the photograph.
fn gps(exif: &Exif) -> Option<GeoPoint> {
    let lat = coordinate(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S', 90.0)?;
    let lon = coordinate(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W', 180.0)?;
    if lat == 0.0 && lon == 0.0 {
        return None;
    }
    let below = uint(exif, Tag::GPSAltitudeRef) == Some(1);
    let alt = number(exif, Tag::GPSAltitude).map(|metres| if below { -metres } else { metres });
    Some(GeoPoint { lat, lon, alt })
}

/// One coordinate in decimal degrees. `negative` is the letter that makes it
/// so (`S`, `W`) and `limit` the most it can be (90, 180).
fn coordinate(exif: &Exif, tag: Tag, hemisphere: Tag, negative: u8, limit: f64) -> Option<f64> {
    let Value::Rational(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    let degrees = rational(parts.first()?)?;
    let minutes = parts.get(1).and_then(rational).unwrap_or(0.0);
    let seconds = parts.get(2).and_then(rational).unwrap_or(0.0);
    let mut value = degrees + minutes / 60.0 + seconds / 3600.0;
    let letter = text(exif, hemisphere).and_then(|t| t.bytes().next());
    if letter.is_some_and(|b| b.eq_ignore_ascii_case(&negative)) {
        value = -value;
    }
    (value.abs() <= limit).then_some(value)
}

/// A view of a reader that ends at `len` bytes, wherever the file does.
///
/// `kamadak-exif` reads as far as its format makes it -- a JPEG with no EXIF
/// is scanned to its last byte, a TIFF is read into memory whole -- and has
/// no limit of its own. Behind a window a damaged or hostile file costs at
/// most `len` bytes of reading and memory.
///
/// Starts where the reader is, which the caller has put at the beginning.
struct Window<R> {
    inner: R,
    len: u64,
    pos: u64,
}

impl<R> Window<R> {
    fn new(inner: R, len: u64) -> Self {
        Window { inner, len, pos: 0 }
    }

    /// How many bytes are left in the window, as far as one read can take.
    fn left(&self) -> usize {
        usize::try_from(self.len.saturating_sub(self.pos)).unwrap_or(usize::MAX)
    }
}

impl<R: Read> Read for Window<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf.len().min(self.left());
        let n = self.inner.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Window<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let left = self.left();
        let buffered = self.inner.fill_buf()?;
        Ok(&buffered[..buffered.len().min(left)])
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt);
        self.pos += amt as u64;
    }
}

impl<R: Seek> Seek for Window<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        // Past the end is allowed, as in a file; reads there find nothing.
        self.pos = self.inner.seek(SeekFrom::Start(target))?;
        Ok(self.pos)
    }
}

// -- Films ------------------------------------------------------------------

/// What a film's movie header said.
#[derive(Debug, Default)]
struct Film {
    duration_ms: Option<u64>,
    created: Option<Timestamp>,
    size: Option<(u32, u32)>,
    codec: Option<String>,
    place: Option<GeoPoint>,
}

/// A film's (or a recording's) metadata.
///
/// Two readers, because neither is enough alone. `mp4` is a real parser and
/// the better witness of the length, but it refuses a file on the first box
/// it dislikes, takes the codec only for the few it knows (not an iPhone's
/// `hvc1`), reports a video's coded size where it is the displayed size that
/// is wanted, and has no idea of Apple's `©xyz`. So [`from_moov`] reads
/// those out of the `moov` box by hand, and answers for the whole file when
/// `mp4` has refused it.
///
/// A file with no `moov` box within reach, or one past [`MAX_MOOV`], is
/// given to neither: `mp4` would read the whole of whatever box it was told
/// to.
fn film<R: BufRead + Seek>(reader: &mut R, size: u64) -> MediaInfo {
    let Some(moov) = read_moov(reader, size) else {
        return MediaInfo::default();
    };
    let by_hand = from_moov(&moov);
    let by_crate = guarded(|| from_crate(reader, size)).flatten().unwrap_or_default();
    MediaInfo {
        duration_ms: by_crate.duration_ms.or(by_hand.duration_ms),
        taken_at: by_crate.created.or(by_hand.created),
        width: by_hand.size.or(by_crate.size).map(|(width, _)| width),
        height: by_hand.size.or(by_crate.size).map(|(_, height)| height),
        codec: by_hand.codec.or(by_crate.codec),
        gps: by_hand.place,
        ..MediaInfo::default()
    }
}

/// What `mp4` makes of the film: its length, when it was made, and the first
/// video track's size and codec (the first audio track's codec, for a
/// recording).
fn from_crate<R: BufRead + Seek>(reader: &mut R, size: u64) -> Option<Film> {
    reader.rewind().ok()?;
    let mp4 = mp4::Mp4Reader::read_header(&mut *reader, size).ok()?;
    // Not `Mp4Reader::duration`, which divides by a timescale a hostile file
    // can set to zero.
    let movie = &mp4.moov.mvhd;
    let mut film = Film {
        duration_ms: movie_millis(movie.duration, movie.timescale),
        created: classic_time(movie.creation_time),
        ..Film::default()
    };
    let mut tracks: Vec<&mp4::Mp4Track> = mp4.tracks().values().collect();
    tracks.sort_by_key(|track| track.track_id());
    let video = tracks.iter().find(|t| matches!(t.track_type(), Ok(mp4::TrackType::Video)));
    let audio = tracks.iter().find(|t| matches!(t.track_type(), Ok(mp4::TrackType::Audio)));
    if let Some(track) = video {
        film.size = pair(u32::from(track.width()), u32::from(track.height()));
        film.codec = track.box_type().ok().map(|code| code.to_string());
    } else if let Some(track) = audio {
        film.codec = track.box_type().ok().map(|code| code.to_string());
    }
    Some(film)
}

/// The inside of a film's `moov` box, found by hopping over the boxes before
/// it without reading them -- a film's pictures are in the `mdat` beside it
/// and may be gigabytes. `None` when there is no `moov`, it is absurdly
/// large, or the file does not begin with boxes at all.
fn read_moov<R: Read + Seek>(reader: &mut R, size: u64) -> Option<Vec<u8>> {
    let mut at = 0u64;
    for _ in 0..TOP_LEVEL_BOXES {
        if at.checked_add(8)? > size {
            return None;
        }
        reader.seek(SeekFrom::Start(at)).ok()?;
        let mut header = [0u8; 16];
        let readable = usize::try_from((size - at).min(16)).ok()?;
        reader.read_exact(&mut header[..readable]).ok()?;
        let small = u64::from(u32::from_be_bytes(header[0..4].try_into().ok()?));
        let (head, total) = match small {
            // Runs to the end of the file.
            0 => (8, size - at),
            // The real size follows, in 64 bits.
            1 if readable == 16 => (16, u64::from_be_bytes(header[8..16].try_into().ok()?)),
            1 => return None,
            n => (8, n),
        };
        if total < head {
            return None;
        }
        if &header[4..8] == b"moov" {
            let end = at.saturating_add(total).min(size);
            let body = end.saturating_sub(at + head);
            if body > MAX_MOOV {
                return None;
            }
            reader.seek(SeekFrom::Start(at + head)).ok()?;
            let mut moov = Vec::new();
            reader.by_ref().take(body).read_to_end(&mut moov).ok()?;
            return Some(moov);
        }
        at = at.checked_add(total)?;
    }
    None
}

/// The boxes inside `buf`: each one's four-letter name and what is in it.
/// Ends at the first that is cut short or lies about its size.
fn boxes(buf: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = buf;
    std::iter::from_fn(move || {
        let small = u32::from_be_bytes(rest.get(0..4)?.try_into().ok()?);
        let kind: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        let (head, total): (usize, u64) = match small {
            0 => (8, rest.len() as u64),
            1 => (16, u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?)),
            n => (8, u64::from(n)),
        };
        let total = usize::try_from(total).ok().filter(|t| (head..=rest.len()).contains(t))?;
        let inside = &rest[head..total];
        rest = &rest[total..];
        Some((kind, inside))
    })
}

/// What is inside the first box of this kind in `buf`.
fn find<'a>(buf: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(buf).find(|(name, _)| name == kind).map(|(_, inside)| inside)
}

fn be16(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(buf.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn be32(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(buf.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn be64(buf: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(buf.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// What the `moov` box says, read by hand: the movie header, the first video
/// track (or the first audio one), and where Apple's `©xyz` says it was shot.
fn from_moov(moov: &[u8]) -> Film {
    let mut film = Film::default();
    if let Some((created, scale, duration)) = find(moov, b"mvhd").and_then(movie_header) {
        film.created = classic_time(created);
        // A fragmented film's header says no length (zero), and its
        // `mvex/mehd` box says what the whole is.
        film.duration_ms = movie_millis(duration, scale).or_else(|| {
            let mehd = find(find(moov, b"mvex")?, b"mehd")?;
            let whole =
                if *mehd.first()? == 1 { be64(mehd, 4)? } else { u64::from(be32(mehd, 4)?) };
            movie_millis(whole, scale)
        });
    }
    let tracks: Vec<Track> = boxes(moov)
        .filter(|(name, _)| name == b"trak")
        .filter_map(|(_, trak)| track(trak))
        .collect();
    if let Some(video) = tracks.iter().find(|t| &t.handler == b"vide") {
        film.size = video.size;
        film.codec = video.codec.clone();
    } else if let Some(sound) = tracks.iter().find(|t| &t.handler == b"soun") {
        film.codec = sound.codec.clone();
    }
    film.place = find(moov, b"udta").and_then(location);
    film
}

/// The movie header: when it was made, the ticks in a second, and how many
/// ticks the film is long. Version 1 widens the times to 64 bits.
fn movie_header(mvhd: &[u8]) -> Option<(u64, u32, u64)> {
    if *mvhd.first()? == 1 {
        Some((be64(mvhd, 4)?, be32(mvhd, 20)?, be64(mvhd, 24)?))
    } else {
        Some((u64::from(be32(mvhd, 4)?), be32(mvhd, 12)?, u64::from(be32(mvhd, 16)?)))
    }
}

/// `duration` ticks of `scale` to the second, in milliseconds. `None` for
/// zero, and for the all-ones a recorder writes when it does not know yet.
fn movie_millis(duration: u64, scale: u32) -> Option<u64> {
    if scale == 0 || duration == 0 || duration == u64::from(u32::MAX) || duration == u64::MAX {
        return None;
    }
    u64::try_from(u128::from(duration) * 1000 / u128::from(scale)).ok()
}

/// A movie header's time -- seconds since 1 January 1904 -- as an instant,
/// unless it is not believable. Cameras whose clock was never set write zero
/// (1904) or the Unix epoch (1970), and a file from the future is a clock
/// that is wrong the other way; none of them is when anything was filmed.
fn classic_time(seconds: u64) -> Option<Timestamp> {
    let unix = i64::try_from(seconds.checked_sub(UNIX_FROM_1904)?).ok()?;
    let latest = Timestamp::now().as_second() + 86_400;
    // From 1 January 1990.
    if !(631_152_000..=latest).contains(&unix) {
        return None;
    }
    Timestamp::from_second(unix).ok()
}

fn pair(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

/// One track of a film, as far as this reads it.
struct Track {
    /// `vide`, `soun`, `text`...
    handler: [u8; 4],
    /// The size it is shown at: the `tkhd` box's, which has any rotation
    /// applied, unlike the coded size in the sample entry.
    size: Option<(u32, u32)>,
    /// The first sample entry's name: `avc1`, `hvc1`, `mp4a`.
    codec: Option<String>,
}

fn track(trak: &[u8]) -> Option<Track> {
    let mdia = find(trak, b"mdia")?;
    // After the version, flags and the QuickTime "component type".
    let handler: [u8; 4] = find(mdia, b"hdlr")?.get(8..12)?.try_into().ok()?;
    // After the version and flags, the entry count, and the entry's own size.
    let entry = find(mdia, b"minf")
        .and_then(|minf| find(minf, b"stbl"))
        .and_then(|stbl| find(stbl, b"stsd"))
        .and_then(|stsd| stsd.get(12..16));
    let codec = entry
        .filter(|name| name.iter().all(|b| b.is_ascii_graphic() || *b == b' '))
        .map(|name| String::from_utf8_lossy(name).trim_end().to_string());
    // Width and height close the box, as 16.16 fixed point, in both versions.
    let size = find(trak, b"tkhd").and_then(|tkhd| {
        let end = tkhd.len();
        pair(be32(tkhd, end.checked_sub(8)?)? >> 16, be32(tkhd, end - 4)? >> 16)
    });
    Some(Track { handler, size, codec })
}

/// Apple's `©xyz`, in `udta`: a 16-bit length, a 16-bit language and then
/// the place as ISO 6709 text. Android writes it too.
fn location(udta: &[u8]) -> Option<GeoPoint> {
    let (_, entry) = boxes(udta).find(|(name, _)| name == b"\xA9xyz")?;
    let length = usize::from(be16(entry, 0)?);
    let text = entry.get(4..)?;
    iso6709(std::str::from_utf8(&text[..text.len().min(length)]).ok()?)
}

/// `+37.7749-122.4194+012.345/`: latitude, longitude and optionally altitude,
/// each signed, ended by a slash. Only the decimal-degrees form is read
/// (two digits of latitude, three of longitude); the degrees-minutes forms
/// are not what phones write.
fn iso6709(text: &str) -> Option<GeoPoint> {
    let body = text.split('/').next()?.trim();
    if !body.starts_with(['+', '-']) {
        return None;
    }
    let mut parts = Vec::new();
    let mut from = 0;
    for (i, c) in body.char_indices().skip(1) {
        if c == '+' || c == '-' {
            parts.push(&body[from..i]);
            from = i;
        }
    }
    parts.push(&body[from..]);

    let whole_digits = |part: &str| part[1..].split('.').next().map_or(0, str::len);
    let (lat, lon) = (parts.first()?, parts.get(1)?);
    if whole_digits(lat) != 2 || whole_digits(lon) != 3 {
        return None;
    }
    let (lat, lon): (f64, f64) = (lat.parse().ok()?, lon.parse().ok()?);
    if lat.abs() > 90.0 || lon.abs() > 180.0 || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    let alt = parts.get(2).and_then(|part| part.parse().ok());
    Some(GeoPoint { lat, lon, alt })
}

// -- PDFs -------------------------------------------------------------------

/// A PDF's page count, from the root of its page tree: the `/Type /Pages`
/// object with no `/Parent`, whose `/Count` is every page below it.
///
/// Looked for in the last megabyte (where an update leaves the newest tree)
/// and then the first (where a linearised file keeps it). Reliable for the
/// files that keep the tree in the open, which most do. A file that packs its
/// objects into compressed streams -- most of what a newer producer writes --
/// leaves nothing to find, and answers `None` rather than a guess.
fn pdf_pages<R: Read + Seek>(reader: &mut R, size: u64) -> Option<u32> {
    let tail_from = size.saturating_sub(PDF_SCAN);
    root_page_count(&read_at(reader, tail_from, PDF_SCAN)?).or_else(|| {
        if tail_from == 0 {
            return None;
        }
        root_page_count(&read_at(reader, 0, PDF_SCAN)?)
    })
}

/// Up to `len` bytes from `from`.
fn read_at<R: Read + Seek>(reader: &mut R, from: u64, len: u64) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(from)).ok()?;
    let mut out = Vec::new();
    reader.by_ref().take(len).read_to_end(&mut out).ok()?;
    Some(out)
}

/// How far either side of a `/Type /Pages` a page tree node is looked for:
/// back to the `obj` that opens it, forward to the `endobj` that closes it.
/// A node listing a couple of thousand kids is some 30 KiB, so this is
/// roughly the biggest tree that is read; a bigger one answers `None`, which
/// is what a page count that cannot be found does anyway.
const PAGES_WINDOW: usize = 32 * 1024;

/// How many `/Type /Pages` nodes in one buffer are looked at. A real file
/// has a handful to a few dozen (a root and the nodes under it); a file
/// that repeats the words by the thousand is somebody making this scan slow.
const MAX_PAGES_NODES: usize = 64;

/// The `/Count` of the last root `/Type /Pages` object in `buf`.
///
/// Bounded in what it does for each node ([`PAGES_WINDOW`]) and in how many
/// nodes it looks at ([`MAX_PAGES_NODES`]), because the file is whatever
/// arrived in the folder. Without them each match searched the whole buffer
/// behind it for an `obj` and the whole buffer ahead of it for an `endobj`:
/// a megabyte of nothing but `/Type/Pages` cost ten billion comparisons.
fn root_page_count(buf: &[u8]) -> Option<u32> {
    const TYPE: &[u8] = b"/Type";
    let mut count = None;
    let mut from = 0;
    let mut nodes = 0;
    while let Some(found) = find_bytes(&buf[from..], TYPE) {
        let after = from + found + TYPE.len();
        from = after;
        let rest = &buf[after..];
        let name = &rest[rest.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
        // `/Pages`, and not `/Page` (a leaf) or a longer name.
        if !name.starts_with(b"/Pages") || name.get(6).is_some_and(u8::is_ascii_alphanumeric) {
            continue;
        }
        nodes += 1;
        if nodes > MAX_PAGES_NODES {
            break;
        }
        // The object it is in: from the last `obj` before to the next `endobj`,
        // neither looked for further away than the window.
        let low = after.saturating_sub(PAGES_WINDOW);
        let start =
            buf[low..after].windows(3).rposition(|w| w == b"obj").map_or(low, |at| low + at + 3);
        let high = (after + PAGES_WINDOW).min(buf.len());
        let end = find_bytes(&buf[after..high], b"endobj").map_or(high, |at| after + at);
        let object = &buf[start..end];
        if find_bytes(object, b"/Parent").is_some() {
            continue;
        }
        if let Some(pages) = number_after(object, b"/Count") {
            count = Some(pages);
        }
    }
    count.filter(|pages| *pages > 0)
}

/// The unsigned integer that follows `key` in `buf`.
fn number_after(buf: &[u8], key: &[u8]) -> Option<u32> {
    let at = find_bytes(buf, key)? + key.len();
    let rest = &buf[at..];
    let rest = &rest[rest.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Pictures and films built in memory, for this module's tests and
/// [`super::thumb`]'s.
#[cfg(test)]
pub(super) mod fixtures {
    use std::io::Cursor;

    use exif::experimental::Writer;
    use exif::{Field, In, Tag, Value};
    use image::codecs::jpeg::JpegEncoder;
    use image::{Rgb, RgbImage};

    /// A JPEG of `width` by `height` pixels, each painted by `paint(x, y)`.
    pub fn jpeg(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let picture = RgbImage::from_fn(width, height, |x, y| Rgb(paint(x, y)));
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 90).encode_image(&picture).unwrap();
        out
    }

    pub fn ascii(tag: Tag, text: &str) -> Field {
        Field { tag, ifd_num: In::PRIMARY, value: Value::Ascii(vec![text.as_bytes().to_vec()]) }
    }

    pub fn short(tag: Tag, n: u16) -> Field {
        Field { tag, ifd_num: In::PRIMARY, value: Value::Short(vec![n]) }
    }

    /// A field of one or more rationals, as `(numerator, denominator)`.
    pub fn rationals(tag: Tag, values: &[(u32, u32)]) -> Field {
        let parts: Vec<exif::Rational> = values.iter().map(|&pair| pair.into()).collect();
        Field { tag, ifd_num: In::PRIMARY, value: Value::Rational(parts) }
    }

    /// An EXIF block -- a TIFF structure, as a camera writes it -- of
    /// `fields`, with `thumbnail` as the JPEG in its second directory when
    /// there is one.
    pub fn exif_block(fields: &[Field], thumbnail: Option<&[u8]>) -> Vec<u8> {
        let mut writer = Writer::new();
        for field in fields {
            writer.push_field(field);
        }
        if let Some(jpeg) = thumbnail {
            writer.set_jpeg(jpeg, In::THUMBNAIL);
        }
        let mut out = Cursor::new(Vec::new());
        writer.write(&mut out, false).unwrap();
        out.into_inner()
    }

    /// `jpeg` with `block` spliced in as the APP1 segment a camera writes
    /// right after the start of image marker.
    pub fn with_exif(jpeg: &[u8], block: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        // The length counts itself and the six bytes of "Exif\0\0".
        out.extend_from_slice(&u16::try_from(block.len() + 8).unwrap().to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(block);
        out.extend_from_slice(&jpeg[2..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::fixtures::{ascii, exif_block, jpeg, rationals, short, with_exif};
    use super::*;

    /// A 64 by 48 picture with the EXIF of a camera, among `more`.
    fn photograph(more: Vec<Field>) -> Vec<u8> {
        let mut fields = vec![
            ascii(Tag::Make, "Acme  "),
            ascii(Tag::Model, "Rocket 9"),
            ascii(Tag::LensModel, "Acme 35mm f/2.8"),
            ascii(Tag::DateTimeOriginal, "2024:03:01 14:22:05"),
            rationals(Tag::FNumber, &[(28, 10)]),
            rationals(Tag::ExposureTime, &[(1, 250)]),
            short(Tag::PhotographicSensitivity, 400),
            rationals(Tag::FocalLength, &[(350, 10)]),
        ];
        fields.extend(more);
        with_exif(&jpeg(64, 48, |x, y| [x as u8, y as u8, 90]), &exif_block(&fields, None))
    }

    fn place(lat: &str, lon: &str, altitude_ref: u8) -> Vec<Field> {
        vec![
            ascii(Tag::GPSLatitudeRef, lat),
            rationals(Tag::GPSLatitude, &[(37, 1), (46, 1), (2964, 100)]),
            ascii(Tag::GPSLongitudeRef, lon),
            rationals(Tag::GPSLongitude, &[(122, 1), (25, 1), (984, 100)]),
            Field {
                tag: Tag::GPSAltitudeRef,
                ifd_num: In::PRIMARY,
                value: Value::Byte(vec![altitude_ref]),
            },
            rationals(Tag::GPSAltitude, &[(12, 1)]),
        ]
    }

    fn instant(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn a_camera_says_what_it_shot_with_and_how() {
        let info = probe_bytes(&photograph(vec![]), "image/jpeg");
        assert_eq!(info.camera_make.as_deref(), Some("Acme"), "padding trimmed");
        assert_eq!(info.camera_model.as_deref(), Some("Rocket 9"));
        assert_eq!(info.lens.as_deref(), Some("Acme 35mm f/2.8"));
        assert_eq!(info.f_number, Some(2.8));
        assert_eq!(info.exposure_time.as_deref(), Some("1/250"));
        assert_eq!(info.iso, Some(400));
        assert_eq!(info.focal_length_mm, Some(35.0));
        assert_eq!(info.taken_local.as_deref(), Some("2024-03-01T14:22:05"));
        assert_eq!((info.width, info.height), (Some(64), Some(48)), "from the header");
        assert_eq!(info.orientation, None);
        assert_eq!(info.gps, None);
    }

    #[test]
    fn a_time_with_an_offset_is_an_instant_in_that_offset() {
        let info =
            probe_bytes(&photograph(vec![ascii(Tag::OffsetTimeOriginal, "+02:00")]), "image/jpeg");
        assert_eq!(info.taken_at, Some(instant("2024-03-01T12:22:05Z")));
        assert_eq!(info.taken_local.as_deref(), Some("2024-03-01T14:22:05"), "as written");
    }

    #[test]
    fn a_time_without_one_is_read_in_this_computers_zone() {
        let info = probe_bytes(&photograph(vec![]), "image/jpeg");
        let wall = jiff::civil::date(2024, 3, 1).at(14, 22, 5, 0);
        assert_eq!(info.taken_at, Some(TimeZone::system().to_timestamp(wall).unwrap()));
    }

    #[test]
    fn a_camera_with_no_clock_set_has_no_time() {
        let info = probe_bytes(
            &photograph(vec![ascii(Tag::DateTimeDigitized, "0000:00:00 00:00:00")]),
            "image/jpeg",
        );
        assert!(info.taken_local.is_some(), "the original is a date");
        let blank = with_exif(
            &jpeg(8, 8, |_, _| [0; 3]),
            &exif_block(&[ascii(Tag::DateTimeOriginal, "0000:00:00 00:00:00")], None),
        );
        let info = probe_bytes(&blank, "image/jpeg");
        assert_eq!((info.taken_local, info.taken_at), (None, None));
    }

    #[test]
    fn a_portrait_taken_sideways_is_the_size_it_is_seen_at() {
        let info = probe_bytes(&photograph(vec![short(Tag::Orientation, 6)]), "image/jpeg");
        assert_eq!(info.orientation, Some(6));
        assert_eq!((info.width, info.height), (Some(48), Some(64)));
    }

    #[test]
    fn a_place_is_decimal_degrees_south_and_west_are_negative() {
        let north_west = probe_bytes(&photograph(place("N", "W", 0)), "image/jpeg").gps.unwrap();
        assert!((north_west.lat - 37.7749).abs() < 1e-6, "{north_west:?}");
        assert!((north_west.lon + 122.4194).abs() < 1e-6, "{north_west:?}");
        assert_eq!(north_west.alt, Some(12.0));

        let south_east = probe_bytes(&photograph(place("S", "E", 1)), "image/jpeg").gps.unwrap();
        assert!(south_east.lat < 0.0 && south_east.lon > 0.0, "{south_east:?}");
        assert_eq!(south_east.alt, Some(-12.0), "below sea level");
    }

    #[test]
    fn no_fix_is_no_place() {
        let nowhere = vec![
            ascii(Tag::GPSLatitudeRef, "N"),
            rationals(Tag::GPSLatitude, &[(0, 1), (0, 1), (0, 1)]),
            ascii(Tag::GPSLongitudeRef, "E"),
            rationals(Tag::GPSLongitude, &[(0, 1), (0, 1), (0, 1)]),
        ];
        assert_eq!(probe_bytes(&photograph(nowhere), "image/jpeg").gps, None);
    }

    #[test]
    fn exposure_times_are_said_the_way_a_photographer_says_them() {
        let say = |num, denom| exposure(&(num, denom).into());
        assert_eq!(say(1, 250).as_deref(), Some("1/250"));
        assert_eq!(say(10, 2500).as_deref(), Some("1/250"), "reduced");
        assert_eq!(say(33_333, 100_000).as_deref(), Some("1/3"), "near enough");
        assert_eq!(say(4, 10).as_deref(), Some("0.4"), "not 1/2.5");
        assert_eq!(say(5, 2).as_deref(), Some("2.5"));
        assert_eq!(say(30, 1).as_deref(), Some("30"));
        assert_eq!(say(1, 8000).as_deref(), Some("1/8000"));
        assert_eq!(say(0, 1), None);
        assert_eq!(say(1, 0), None);
    }

    #[test]
    fn a_picture_declared_with_the_wrong_name_is_read_for_what_it_is() {
        // A JPEG filed as a PDF, and a PDF filed as a JPEG.
        let as_pdf = probe_bytes(&photograph(vec![]), "application/pdf");
        assert_eq!(as_pdf.camera_model.as_deref(), Some("Rocket 9"));
        let pdf = b"%PDF-1.4\n1 0 obj << /Type /Pages /Count 2 /Kids [] >> endobj\n%%EOF";
        assert_eq!(probe_bytes(pdf, "image/jpeg").pages, Some(2));
    }

    #[test]
    fn a_file_on_disk_is_read_like_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0001.jpg");
        let bytes = photograph(vec![short(Tag::Orientation, 3)]);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(probe(&path, "image/jpeg"), probe_bytes(&bytes, "image/jpeg"));
        assert_eq!(probe(&dir.path().join("gone.jpg"), "image/jpeg"), MediaInfo::default());
        assert_eq!(probe(dir.path(), "image/jpeg"), MediaInfo::default(), "a folder");
    }

    #[test]
    fn what_is_not_what_it_says_is_empty_and_does_not_panic() {
        let good = photograph(vec![]);
        let mut noise = Vec::new();
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..5000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            noise.push(x as u8);
        }
        let hostile_film = {
            let mut b = vec![0, 0, 0, 0x14];
            b.extend_from_slice(b"ftypisom\0\0\0\0isom");
            // A moov that claims to be four gigabytes, and a box that claims
            // to be smaller than its own header.
            b.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xF0]);
            b.extend_from_slice(b"moov");
            b.extend_from_slice(&[0, 0, 0, 3]);
            b.extend_from_slice(b"mvhd");
            b
        };
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("empty", vec![], "image/jpeg"),
            ("one byte", vec![0xFF], "image/jpeg"),
            ("a truncated JPEG", good[..30].to_vec(), "image/jpeg"),
            ("noise as a JPEG", noise.clone(), "image/jpeg"),
            ("noise as a film", noise.clone(), "video/mp4"),
            ("noise as a PDF", noise.clone(), "application/pdf"),
            ("text as a picture", b"hello there, I am not a picture".to_vec(), "image/png"),
            ("a hostile film", hostile_film, "video/mp4"),
            ("nothing", b"%PDF-".to_vec(), "application/pdf"),
        ];
        for (what, bytes, mime) in cases {
            let info = probe_bytes(&bytes, mime);
            assert_eq!(info.camera_model, None, "{what}");
            assert_eq!(info.duration_ms, None, "{what}");
            assert_eq!(info.pages, None, "{what}");
        }
        // And the truncated JPEG with a header still knows what it can.
        let cut = probe_bytes(&good[..good.len() / 2], "image/jpeg");
        assert_eq!(cut.camera_model.as_deref(), Some("Rocket 9"), "the EXIF is at the front");
    }

    /// A film from `mp4`'s own writer: 640 by 360 H.264, two seconds.
    fn written_film() -> Vec<u8> {
        use mp4::{
            AvcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType,
        };
        let config = Mp4Config {
            major_brand: "isom".parse().unwrap(),
            minor_version: 512,
            compatible_brands: vec!["isom".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1000,
        };
        let mut writer = Mp4Writer::write_start(Cursor::new(Vec::new()), &config).unwrap();
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Video,
                timescale: 1000,
                language: "und".to_string(),
                media_conf: MediaConfig::AvcConfig(AvcConfig {
                    width: 640,
                    height: 360,
                    seq_param_set: vec![0x67, 0x64, 0x00, 0x0D, 0xAC, 0xD9, 0x41, 0x41],
                    pic_param_set: vec![0x68, 0xEB, 0xE3, 0xCB],
                }),
            })
            .unwrap();
        let sample = Mp4Sample {
            start_time: 0,
            duration: 2000,
            rendering_offset: 0,
            is_sync: true,
            bytes: bytes::Bytes::from(vec![0u8; 32]),
        };
        writer.write_sample(1, &sample).unwrap();
        writer.write_end().unwrap();
        writer.into_writer().into_inner()
    }

    /// `film` with the movie header's creation time set to `unix`: the writer
    /// leaves it zero.
    fn made_at(mut film: Vec<u8>, unix: u64) -> Vec<u8> {
        // After the name and the version and flags.
        let at = film.windows(4).position(|w| w == b"mvhd").unwrap() + 8;
        let classic = u32::try_from(unix + UNIX_FROM_1904).unwrap();
        film[at..at + 4].copy_from_slice(&classic.to_be_bytes());
        film
    }

    #[test]
    fn a_films_header_says_how_long_how_big_and_in_what() {
        let film = probe_bytes(&written_film(), "video/mp4");
        assert_eq!(film.duration_ms, Some(2000));
        assert_eq!((film.width, film.height), (Some(640), Some(360)));
        assert_eq!(film.codec.as_deref(), Some("avc1"));
        assert_eq!(film.taken_at, None, "a creation time of zero is no time");
        assert_eq!((film.orientation, film.camera_make), (None, None));

        let dated = probe_bytes(&made_at(written_film(), 1_709_302_925), "video/mp4");
        assert_eq!(dated.taken_at, Some(instant("2024-03-01T14:22:05Z")));
        // Sniffed, whatever the name says.
        let unnamed = probe_bytes(&written_film(), "application/octet-stream");
        assert_eq!(unnamed.duration_ms, Some(2000));
    }

    #[test]
    fn a_film_made_by_a_clock_never_set_has_no_date() {
        let epoch = probe_bytes(&made_at(written_film(), 0), "video/mp4");
        assert_eq!(epoch.taken_at, None);
        // The latest a 32-bit movie header can say is early 2040.
        let future = probe_bytes(&made_at(written_film(), 2_200_000_000), "video/mp4");
        assert_eq!(future.taken_at, None);
    }

    fn boxed(kind: &[u8; 4], inside: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(inside.len() + 8).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(inside);
        out
    }

    /// A QuickTime movie as an iPhone writes one, as far as the hand reader
    /// goes and no further -- `mp4` refuses it for being short of boxes it
    /// insists on: 1920 by 1080 `hvc1`, five seconds, and `©xyz`.
    fn quicktime_movie() -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        mvhd[4..8].copy_from_slice(&3_792_147_725u32.to_be_bytes());
        mvhd[12..16].copy_from_slice(&600u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&3000u32.to_be_bytes());
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(1920u32 << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(1080u32 << 16).to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let mut stsd = vec![0u8; 8];
        stsd.extend(boxed(b"hvc1", &[0u8; 78]));
        let minf = boxed(b"minf", &boxed(b"stbl", &boxed(b"stsd", &stsd)));
        let mdia = boxed(b"mdia", &[boxed(b"hdlr", &hdlr), minf].concat());
        let trak = boxed(b"trak", &[boxed(b"tkhd", &tkhd), mdia].concat());

        let text = "+37.7749-122.4194+012.345/";
        let mut xyz = u16::try_from(text.len()).unwrap().to_be_bytes().to_vec();
        xyz.extend_from_slice(&[0x15, 0xC7]);
        xyz.extend_from_slice(text.as_bytes());
        let udta = boxed(b"udta", &boxed(b"\xA9xyz", &xyz));

        let moov = boxed(b"moov", &[boxed(b"mvhd", &mvhd), trak, udta].concat());
        [boxed(b"ftyp", b"qt  \0\0\0\0qt  "), boxed(b"mdat", &[0u8; 64]), moov].concat()
    }

    #[test]
    fn a_phones_movie_gives_its_codec_its_shown_size_and_where_it_was_shot() {
        let movie = quicktime_movie();
        assert_eq!(sniff(&movie), Some("video/quicktime"));
        let info = probe_bytes(&movie, "video/quicktime");
        assert_eq!(info.duration_ms, Some(5000));
        assert_eq!((info.width, info.height), (Some(1920), Some(1080)));
        assert_eq!(info.codec.as_deref(), Some("hvc1"));
        assert_eq!(info.taken_at, Some(instant("2024-03-01T14:22:05Z")));
        let gps = info.gps.unwrap();
        assert!((gps.lat - 37.7749).abs() < 1e-9 && (gps.lon + 122.4194).abs() < 1e-9);
        assert_eq!(gps.alt, Some(12.345));
    }

    #[test]
    fn iso_6709_is_read_in_decimal_degrees_only() {
        let at = |text| iso6709(text).map(|g| (g.lat, g.lon, g.alt));
        assert_eq!(at("+37.7749-122.4194/"), Some((37.7749, -122.4194, None)));
        assert_eq!(at("-33.8688+151.2093+058.000CRSWGS_84/"), Some((-33.8688, 151.2093, None)));
        assert_eq!(at("+3746.494-12225.164/"), None, "degrees and minutes");
        assert_eq!(at("+00.0000+000.0000/"), None, "no fix");
        assert_eq!(at("+97.0000+000.0000/"), None, "no such latitude");
        assert_eq!(at("not a place"), None);
        assert_eq!(at(""), None);
    }

    #[test]
    fn a_page_count_is_the_root_of_the_page_tree() {
        let pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 3 >>\nendobj\n\
            3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>\nendobj\n\
            4 0 obj\n<</Type/Pages/Parent 2 0 R/Kids[6 0 R 7 0 R]/Count 2>>\nendobj\n\
            5 0 obj\n<< /Type /Page /Parent 3 0 R >>\nendobj\n\
            9 0 obj\n<< /Type /Outlines /Count 40 >>\nendobj\n\
            trailer << /Root 1 0 R >>\n%%EOF\n";
        assert_eq!(probe_bytes(pdf, "application/pdf").pages, Some(3));

        // A tree packed into a compressed object stream cannot be seen.
        let packed = b"%PDF-1.5\n4 0 obj\n<< /Type /ObjStm /N 3 /First 20 >>\nstream\n\
            \x78\x9c\x00\nendstream\nendobj\n";
        assert_eq!(probe_bytes(packed, "application/pdf").pages, None);
    }

    #[test]
    fn the_last_page_tree_wins_where_a_file_was_updated() {
        let pdf = b"%PDF-1.4\n\
            2 0 obj\n<< /Type /Pages /Kids [] /Count 3 >>\nendobj\n\
            %%EOF\n\
            2 0 obj\n<< /Type /Pages /Kids [] /Count 4 >>\nendobj\n\
            %%EOF\n";
        assert_eq!(probe_bytes(pdf, "application/pdf").pages, Some(4));
    }

    #[test]
    fn a_file_of_nothing_but_page_trees_is_read_in_bounded_time() {
        // A megabyte of `/Type/Pages`, no `obj` or `endobj` anywhere: each
        // match used to search everything behind and ahead of it.
        let mut pdf = b"%PDF-1.4\n".to_vec();
        while pdf.len() < 1 << 20 {
            pdf.extend_from_slice(b"/Type/Pages ");
        }
        assert_eq!(root_page_count(&pdf), None);
        assert_eq!(probe_bytes(&pdf, "application/pdf").pages, None);

        // The same with an `obj` at the very start and a `/Count` at the very
        // end: both further away than the window, so neither is found.
        let mut far = b"1 0 obj ".to_vec();
        far.extend(std::iter::repeat_n(b' ', 1 << 20));
        far.extend_from_slice(b"/Type /Pages ");
        far.extend(std::iter::repeat_n(b' ', 1 << 20));
        far.extend_from_slice(b"/Count 9 endobj");
        assert_eq!(root_page_count(&far), None);
    }

    #[test]
    fn a_page_tree_with_a_long_kids_list_is_still_counted() {
        let kids = "3 0 R ".repeat(1500);
        let pdf =
            format!("%PDF-1.4\n2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count 1500 >>\nendobj\n");
        assert_eq!(root_page_count(pdf.as_bytes()), Some(1500));
    }

    #[test]
    fn the_first_bytes_name_the_format() {
        let ftyp = |brand: &[u8; 4]| {
            let mut head = vec![0, 0, 0, 0x18];
            head.extend_from_slice(b"ftyp");
            head.extend_from_slice(brand);
            head.extend_from_slice(&[0; 12]);
            head
        };
        let ebml: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
        let riff = |kind: &[u8; 4]| [&b"RIFF\x24\0\0\0"[..], kind, &[0u8; 8]].concat();
        let sniffed: Vec<(Vec<u8>, Option<&str>)> = vec![
            (jpeg(2, 2, |_, _| [0; 3]), Some("image/jpeg")),
            (b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(), Some("image/png")),
            (b"GIF89a\x01\0\x01\0".to_vec(), Some("image/gif")),
            (riff(b"WEBP"), Some("image/webp")),
            (riff(b"WAVE"), Some("audio/wav")),
            (riff(b"AVI "), Some("video/x-msvideo")),
            ([&b"BM"[..], &[0u8; 12], &40u32.to_le_bytes()].concat(), Some("image/bmp")),
            (b"BMW is a car".to_vec(), None),
            (b"II*\0\x08\0\0\0".to_vec(), Some("image/tiff")),
            (b"MM\0*\0\0\0\x08".to_vec(), Some("image/tiff")),
            (ftyp(b"heic"), Some("image/heic")),
            (ftyp(b"mif1"), Some("image/heif")),
            (ftyp(b"avif"), Some("image/avif")),
            (ftyp(b"isom"), Some("video/mp4")),
            (ftyp(b"mp42"), Some("video/mp4")),
            (ftyp(b"M4V "), Some("video/x-m4v")),
            (ftyp(b"M4A "), Some("audio/mp4")),
            (ftyp(b"qt  "), Some("video/quicktime")),
            (ftyp(b"3gp4"), Some("video/3gpp")),
            (ftyp(b"zzzz"), None),
            (b"%PDF-1.7\n".to_vec(), Some("application/pdf")),
            ([&ebml[..], &[0x42, 0x82, 0x84], b"webm"].concat(), Some("video/webm")),
            ([&ebml[..], &[0x42, 0x82, 0x88], b"matroska"].concat(), Some("video/x-matroska")),
            (b"OggS\0\x02".to_vec(), Some("audio/ogg")),
            (b"fLaC\0\0\0\"".to_vec(), Some("audio/flac")),
            (b"ID3\x04\0\0\0\0\0\0".to_vec(), Some("audio/mpeg")),
            (vec![0xFF, 0xFB, 0x90, 0x00], Some("audio/mpeg")),
            (vec![0xFF, 0xF1, 0x50, 0x80], Some("audio/aac")),
            (vec![0xFF, 0xE0, 0xFF, 0xFF], None),
            (b"PK\x03\x04\x14\0".to_vec(), Some("application/zip")),
            (b"plain words".to_vec(), None),
            (vec![], None),
            (vec![0xFF], None),
        ];
        for (head, expected) in sniffed {
            let shown = String::from_utf8_lossy(&head[..head.len().min(20)]).into_owned();
            assert_eq!(sniff(&head), expected, "{shown:?}");
        }
    }

    #[test]
    fn a_window_ends_where_it_says_though_the_file_goes_on() {
        let mut window = Window::new(Cursor::new(vec![7u8; 100]), 10);
        let mut all = Vec::new();
        window.read_to_end(&mut all).unwrap();
        assert_eq!(all.len(), 10);
        assert_eq!(window.seek(SeekFrom::End(-4)).unwrap(), 6);
        all.clear();
        window.read_to_end(&mut all).unwrap();
        assert_eq!(all.len(), 4);
        window.seek(SeekFrom::Start(50)).unwrap();
        assert_eq!(window.read(&mut [0u8; 8]).unwrap(), 0, "nothing past the end");
        assert!(window.seek(SeekFrom::Current(-100)).is_err());
    }
}
