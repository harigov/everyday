//! Thumbnails: a JPEG of at most [`EDGE`] pixels on its longer side, turned
//! the way the photo's EXIF orientation says, for the Files grid and the
//! Photos timeline. Sealed in the blob store by the vault.
//!
//! Drawn here for the pictures `image` decodes (JPEG, PNG, WebP, GIF, BMP).
//! For a HEIC or a RAW file, the small JPEG most cameras embed in the EXIF
//! is used when there is one. A film's poster frame is drawn by the
//! interface, which can play the film, and handed back through the
//! `set_file_thumbnail` command.
//!
//! # What is done to a picture
//!
//! The same as to a cover (see [`crate::covers`]), and for the same reasons:
//! shrunk until its longer side is [`EDGE`] pixels and never enlarged,
//! turned upright from its orientation, laid over white where it is
//! see-through, and encoded as a JPEG -- which also leaves behind everything
//! the file carried, a camera's make and the place it was taken among it. A
//! thumbnail is for looking at, so [`QUALITY`] is lower than a cover's.
//!
//! # Hostile pictures
//!
//! A file in the folder may be anything, and a few kilobytes of PNG can
//! declare itself a hundred thousand pixels square. So the header is read
//! first and a picture is drawn only if what it declares fits within
//! [`MAX_PIXELS`] and [`MAX_DECODED`] bytes; `image` reads a JPEG whole, so a
//! file past [`MAX_SOURCE`] is not given to it at all. One that is refused
//! still gets the thumbnail embedded in its EXIF, when it has one: a camera's
//! panorama past the limit nearly always does.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek};
use std::path::Path;

use exif::{Exif, In, Tag};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage};

use super::meta;
use crate::covers;

/// The longer side of a thumbnail, in pixels.
pub const EDGE: u32 = 512;

/// The JPEG quality a thumbnail is encoded at, out of a hundred. Lower than a
/// cover's: a grid cell is looked at for a moment, and a few thousand of them
/// are kept sealed in the vault, sent in a backup and carried to every
/// device.
const QUALITY: u8 = 80;

/// How much of the start of a file is looked at to tell what it is.
const HEAD: u64 = 4096;

/// The largest file given to `image`, which reads a JPEG into memory whole.
const MAX_SOURCE: u64 = 256 * 1024 * 1024;

/// The most memory a picture may take once decoded. A hundred and twenty
/// megapixels of eight-bit RGB is 360 MB, which passes; the same in sixteen
/// bits a channel and with an alpha does not.
const MAX_DECODED: u64 = 512 * 1024 * 1024;

/// The most pixels a poster handed back by the interface may have. A frame
/// of an 8K film is 33 million; the interface draws these at the size they
/// are shown, so past this it is not the interface.
const MAX_UPLOADED_PIXELS: u64 = 40_000_000;

/// The formats `image` is asked to draw: the ones it was built with.
const DRAWN: [ImageFormat; 5] =
    [ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP, ImageFormat::Gif, ImageFormat::Bmp];

/// A thumbnail's JPEG bytes for the picture at `path`, or `None` when
/// nothing can be drawn of it. Never panics on a hostile file; refuses one
/// whose declared size would need more than [`MAX_PIXELS`] to decode.
pub fn make(path: &Path, mime: &str, orientation: Option<u8>) -> Option<Vec<u8>> {
    meta::guarded(|| {
        let file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        draw(&mut BufReader::new(file), size, mime, orientation)
    })
    .flatten()
}

/// The same, from bytes in memory.
pub fn make_from_bytes(bytes: &[u8], mime: &str, orientation: Option<u8>) -> Option<Vec<u8>> {
    meta::guarded(|| draw(&mut Cursor::new(bytes), bytes.len() as u64, mime, orientation)).flatten()
}

/// Check a thumbnail the interface drew (a film's poster): a JPEG or PNG no
/// larger than [`MAX_UPLOADED`] bytes and [`EDGE`] pixels a side, re-encoded
/// as a JPEG. `None` when it is not one.
///
/// A picture larger than [`EDGE`] is brought down to it rather than
/// refused -- the interface draws a poster at whatever size the film has --
/// and one that is not a JPEG or a PNG is refused, whatever it is: nothing
/// else is drawn by a canvas.
pub fn accept_uploaded(bytes: &[u8]) -> Option<Vec<u8>> {
    meta::guarded(|| {
        if bytes.len() > MAX_UPLOADED {
            return None;
        }
        let formats = [ImageFormat::Jpeg, ImageFormat::Png];
        let (picture, own) = decode(Cursor::new(bytes), &formats, MAX_UPLOADED_PIXELS)?;
        shrunk_and_encoded(picture, own)
    })
    .flatten()
}

/// The most pixels a picture may declare before it is refused rather than
/// decoded: a decompression bomb is a few bytes on disk.
pub const MAX_PIXELS: u64 = 120_000_000;

/// The largest thumbnail the interface may hand back.
pub const MAX_UPLOADED: usize = 2 * 1024 * 1024;

/// A thumbnail of the picture `reader` is over. What is read first decides
/// what it is: a film or a document has no thumbnail here, whatever its name
/// says it is, and a `.jpg` that is really a HEIC is drawn as the HEIC it is.
fn draw<R: BufRead + Seek>(
    reader: &mut R,
    size: u64,
    mime: &str,
    orientation: Option<u8>,
) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    reader.by_ref().take(HEAD).read_to_end(&mut head).ok()?;
    reader.rewind().ok()?;
    if !meta::sniff(&head).unwrap_or(mime).starts_with("image/") {
        return None;
    }
    // What the catalog says, when it says; the file's own word otherwise.
    let turn = orientation.and_then(Orientation::from_exif);
    if size <= MAX_SOURCE
        && let Some(drawn) = render(&mut *reader, turn)
    {
        return Some(drawn);
    }
    // Not a picture `image` reads, or not one it should: HEIC, RAW, a
    // panorama past the limit. The EXIF may carry a small one.
    let exif = meta::read_exif(reader, &head, size)?;
    let preview = embedded_jpeg(&exif)?;
    render(Cursor::new(preview), turn.or_else(|| exif_orientation(&exif)))
}

/// A thumbnail of the picture `reader` is over, turned by `turn` -- or by
/// the orientation in the file itself when that is `None`.
fn render<R: BufRead + Seek>(reader: R, turn: Option<Orientation>) -> Option<Vec<u8>> {
    let (picture, own) = decode(reader, &DRAWN, MAX_PIXELS)?;
    shrunk_and_encoded(picture, turn.unwrap_or(own))
}

/// `reader` decoded, if it is one of `formats` and declares no more than
/// `max_pixels` -- with the orientation its file asks for.
///
/// The header is read, and the size checked, before any pixel is: `image`
/// does not enforce its own allocation limit on a decoder it is handed (only
/// on `ImageReader::decode`), so this is where a decompression bomb is
/// stopped.
fn decode<R: BufRead + Seek>(
    reader: R,
    formats: &[ImageFormat],
    max_pixels: u64,
) -> Option<(DynamicImage, Orientation)> {
    let mut reader = ImageReader::new(reader).with_guessed_format().ok()?;
    if !reader.format().is_some_and(|format| formats.contains(&format)) {
        return None;
    }
    reader.limits(limits());
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > max_pixels || decoder.total_bytes() > MAX_DECODED {
        return None;
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let picture = DynamicImage::from_decoder(decoder).ok()?;
    Some((picture, orientation))
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODED);
    limits
}

/// `picture` brought down to [`EDGE`], turned upright by `turn` and encoded.
///
/// Shrunk before it is turned: turning only moves pixels, so the order makes
/// no difference to the result, and it is the smaller picture that is moved.
fn shrunk_and_encoded(picture: DynamicImage, turn: Orientation) -> Option<Vec<u8>> {
    let mut small = if picture.width().max(picture.height()) <= EDGE {
        picture
    } else {
        covers::scaled_down(&picture, EDGE)
    };
    small.apply_orientation(turn);
    encode(&covers::flattened(&small))
}

/// `picture` as a JPEG at [`QUALITY`], by the same encoder and for the same
/// reason as a cover (see [`covers::jpeg`]). `None` only for a picture the
/// encoder cannot take, which [`EDGE`] has already made impossible.
fn encode(picture: &RgbImage) -> Option<Vec<u8>> {
    let width = u16::try_from(picture.width()).ok()?;
    let height = u16::try_from(picture.height()).ok()?;
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, QUALITY);
    encoder.encode(picture.as_raw(), width, height, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

/// The JPEG a camera embeds in its EXIF -- in the second directory, where a
/// thumbnail goes, or the first -- as bytes of the EXIF block. Its offset is
/// from the start of the TIFF structure, which is where [`Exif::buf`] starts.
fn embedded_jpeg(exif: &Exif) -> Option<&[u8]> {
    [In::THUMBNAIL, In::PRIMARY].into_iter().find_map(|ifd| {
        let word = |tag| exif.get_field(tag, ifd)?.value.get_uint(0);
        let start = usize::try_from(word(Tag::JPEGInterchangeFormat)?).ok()?;
        let length = usize::try_from(word(Tag::JPEGInterchangeFormatLength)?).ok()?;
        let jpeg = exif.buf().get(start..start.checked_add(length)?)?;
        jpeg.starts_with(&[0xFF, 0xD8]).then_some(jpeg)
    })
}

/// The orientation in an EXIF block.
fn exif_orientation(exif: &Exif) -> Option<Orientation> {
    let n = exif.get_field(Tag::Orientation, In::PRIMARY)?.value.get_uint(0)?;
    Orientation::from_exif(u8::try_from(n).ok()?)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{Rgb, Rgba, RgbaImage};

    use super::super::meta::fixtures::{ascii, exif_block, jpeg, short, with_exif};
    use super::*;

    const RED: [u8; 3] = [220, 30, 30];
    const BLUE: [u8; 3] = [30, 30, 220];

    /// A JPEG that is red on its left half and blue on its right, so that
    /// turning it shows.
    fn halves(width: u32, height: u32) -> Vec<u8> {
        jpeg(width, height, |x, _| if x < width / 2 { RED } else { BLUE })
    }

    fn png_of(picture: DynamicImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        picture.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn gradient_png(width: u32, height: u32) -> Vec<u8> {
        png_of(DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
            Rgb([x as u8, y as u8, 7])
        })))
    }

    fn decoded(bytes: &[u8]) -> RgbImage {
        assert_eq!(image::guess_format(bytes).unwrap(), ImageFormat::Jpeg, "a thumbnail is a JPEG");
        image::load_from_memory(bytes).unwrap().to_rgb8()
    }

    fn is_red(pixel: &Rgb<u8>) -> bool {
        pixel.0[0] > 170 && pixel.0[2] < 90
    }

    fn is_blue(pixel: &Rgb<u8>) -> bool {
        pixel.0[2] > 170 && pixel.0[0] < 90
    }

    #[test]
    fn a_large_picture_comes_down_to_the_edge_in_its_own_proportions() {
        let thumb = decoded(&make_from_bytes(&halves(1600, 800), "image/jpeg", None).unwrap());
        assert_eq!(thumb.dimensions(), (512, 256));
        assert!(is_red(thumb.get_pixel(40, 128)) && is_blue(thumb.get_pixel(470, 128)));

        let tall = decoded(&make_from_bytes(&gradient_png(300, 900), "image/png", None).unwrap());
        assert_eq!(tall.dimensions(), (171, 512), "300 * 512 / 900, rounded");
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        let thumb = decoded(&make_from_bytes(&gradient_png(100, 60), "image/png", None).unwrap());
        assert_eq!(thumb.dimensions(), (100, 60));
    }

    #[test]
    fn a_picture_is_turned_the_way_its_orientation_says() {
        let wide = halves(800, 400);
        // Rotated a quarter turn clockwise, the left half is the top.
        let six = decoded(&make_from_bytes(&wide, "image/jpeg", Some(6)).unwrap());
        assert_eq!(six.dimensions(), (256, 512));
        assert!(is_red(six.get_pixel(128, 40)) && is_blue(six.get_pixel(128, 470)));
        // Counter-clockwise, it is the bottom.
        let eight = decoded(&make_from_bytes(&wide, "image/jpeg", Some(8)).unwrap());
        assert_eq!(eight.dimensions(), (256, 512));
        assert!(is_blue(eight.get_pixel(128, 40)) && is_red(eight.get_pixel(128, 470)));
        // Upside down, the right.
        let three = decoded(&make_from_bytes(&wide, "image/jpeg", Some(3)).unwrap());
        assert_eq!(three.dimensions(), (512, 256));
        assert!(is_blue(three.get_pixel(40, 128)) && is_red(three.get_pixel(470, 128)));
        // And 1, and what is no orientation at all, leave it be.
        for left_alone in [Some(1), Some(0), Some(9), None] {
            let same = decoded(&make_from_bytes(&wide, "image/jpeg", left_alone).unwrap());
            assert_eq!(same.dimensions(), (512, 256), "{left_alone:?}");
        }
    }

    #[test]
    fn the_orientation_in_the_file_is_used_when_the_catalog_has_none() {
        let turned = with_exif(&halves(800, 400), &exif_block(&[short(Tag::Orientation, 6)], None));
        let by_file = decoded(&make_from_bytes(&turned, "image/jpeg", None).unwrap());
        assert_eq!(by_file.dimensions(), (256, 512));
        // The catalog's word is the last, though: it read the file when it
        // arrived, and may have been told since.
        let by_catalog = decoded(&make_from_bytes(&turned, "image/jpeg", Some(1)).unwrap());
        assert_eq!(by_catalog.dimensions(), (512, 256));
    }

    #[test]
    fn what_was_carried_in_the_file_is_not_carried_into_the_thumbnail() {
        let carried = with_exif(&halves(64, 32), &exif_block(&[ascii(Tag::Make, "Acme")], None));
        assert!(carried.windows(4).any(|w| w == b"Acme"), "it was there");
        let thumb = make_from_bytes(&carried, "image/jpeg", None).unwrap();
        assert!(!thumb.windows(5).any(|w| w == b"Exif\0"), "no EXIF block");
        assert!(!thumb.windows(4).any(|w| w == b"Acme"), "nor what is in it");
    }

    #[test]
    fn a_heic_or_a_raw_file_is_drawn_from_the_jpeg_in_its_exif() {
        // A TIFF structure with a JPEG in its second directory is what a
        // RAW file is at the front; `image` is not built to read TIFF, so the
        // preview is the only way in.
        let raw = exif_block(&[short(Tag::Orientation, 6)], Some(&halves(160, 80)));
        let thumb = decoded(&make_from_bytes(&raw, "image/tiff", None).unwrap());
        assert_eq!(thumb.dimensions(), (80, 160), "turned, and not enlarged");
        assert!(is_red(thumb.get_pixel(40, 20)) && is_blue(thumb.get_pixel(40, 140)));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0042.CR2");
        std::fs::write(&path, raw).unwrap();
        // The name's guess is no use; the first bytes say it is a picture.
        let by_path = make(&path, "application/octet-stream", None).unwrap();
        assert_eq!(decoded(&by_path).dimensions(), (80, 160));
        assert_eq!(make(&dir.path().join("gone.CR2"), "image/x-canon-cr2", None), None);
    }

    #[test]
    fn a_raw_file_with_no_preview_has_no_thumbnail() {
        let bare = exif_block(&[short(Tag::Orientation, 6)], None);
        assert_eq!(make_from_bytes(&bare, "image/tiff", None), None);
    }

    #[test]
    fn films_and_documents_are_the_interfaces_to_draw() {
        let mut film = vec![0, 0, 0, 0x18];
        film.extend_from_slice(b"ftypisom\0\0\0\0isomiso2");
        assert_eq!(make_from_bytes(&film, "video/mp4", None), None);
        assert_eq!(make_from_bytes(b"%PDF-1.4\n%%EOF", "application/pdf", None), None);
        // Whatever the name claims: the bytes are a film.
        assert_eq!(make_from_bytes(&film, "image/jpeg", None), None);
    }

    #[test]
    fn what_cannot_be_drawn_is_none_and_does_not_panic() {
        let good = halves(800, 400);
        let mut noise = Vec::new();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..4000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            noise.push(x as u8);
        }
        for (what, bytes, mime) in [
            ("empty", Vec::new(), "image/jpeg"),
            ("one byte", vec![0xFF], "image/jpeg"),
            ("a header and nothing more", good[..20].to_vec(), "image/jpeg"),
            ("noise", noise.clone(), "image/jpeg"),
            ("text", b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(), "image/svg+xml"),
            ("a PNG signature", b"\x89PNG\r\n\x1a\n".to_vec(), "image/png"),
        ] {
            assert_eq!(make_from_bytes(&bytes, mime, None), None, "{what}");
        }
        // Cut off in the pictures, it may be drawn as far as it goes or not
        // at all; what matters is that nothing panics.
        let _ = make_from_bytes(&good[..good.len() / 2], "image/jpeg", None);
    }

    #[test]
    fn a_picture_declaring_more_pixels_than_the_limit_is_refused_before_it_is_decoded() {
        let png = gradient_png(64, 64);
        assert!(decode(Cursor::new(&png), &DRAWN, 4096).is_some(), "exactly the limit");
        assert!(decode(Cursor::new(&png), &DRAWN, 4095).is_none(), "one past it");
        // And a format that was not asked for is not decoded either.
        assert!(decode(Cursor::new(&png), &[ImageFormat::Jpeg], MAX_PIXELS).is_none());
    }

    #[test]
    fn a_poster_the_interface_drew_is_checked_and_made_a_jpeg_within_the_edge() {
        let big = decoded(&accept_uploaded(&gradient_png(1024, 768)).unwrap());
        assert_eq!(big.dimensions(), (512, 384), "brought down, not refused");

        let small = decoded(&accept_uploaded(&halves(200, 100)).unwrap());
        assert_eq!(small.dimensions(), (200, 100), "a JPEG is encoded again, and not enlarged");

        let nothing = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        let clear = png_of(DynamicImage::ImageRgba8(nothing));
        let flat = decoded(&accept_uploaded(&clear).unwrap());
        assert!(flat.pixels().all(|p| p.0.iter().all(|c| *c > 240)), "laid over white");
    }

    #[test]
    fn a_poster_that_is_not_a_jpeg_or_a_png_or_is_too_large_is_refused() {
        let picture = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, Rgb([9, 9, 9])));
        let mut bmp = Cursor::new(Vec::new());
        picture.write_to(&mut bmp, ImageFormat::Bmp).unwrap();
        assert_eq!(accept_uploaded(&bmp.into_inner()), None, "a BMP");

        let mut huge = halves(64, 64);
        huge.resize(MAX_UPLOADED + 1, 0);
        assert_eq!(accept_uploaded(&huge), None, "over the byte limit");

        assert_eq!(accept_uploaded(b"not a picture"), None);
        assert_eq!(accept_uploaded(&[]), None);
    }
}
