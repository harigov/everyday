//! A picture somebody sends the assistant, made the size a model is shown.
//!
//! What arrives is whatever was to hand: a phone's photograph at four
//! thousand pixels and five megabytes, a screenshot of a wide monitor, a
//! receipt photographed at an angle. None of that size is any use to a
//! model. OpenAI fits a picture inside 2048 pixels square before it looks
//! at it, and most others do much the same, so every pixel past that is
//! uploaded for nothing -- and uploaded again with every turn the picture is
//! replayed in (see `crate::agent`'s `replay`).
//!
//! So every picture is made into one here, on its way into the vault,
//! whichever way it came -- the composer's paperclip, a paste, or a photo
//! sent from a phone over a chat link. The vault keeps that copy and nothing
//! else of it, for as long as the conversation it was sent in.
//!
//! # What is done to it
//!
//! - **Turned the right way up**, from the orientation a camera wrote into
//!   it, as a cover is (see [`crate::covers`]).
//! - **Shrunk** until its longer side is at most [`MAX_SIDE`] pixels, its
//!   proportions kept. Never enlarged.
//! - **Encoded again, always** -- unlike a cover, which keeps a small
//!   picture byte for byte. A photograph carries where it was taken and on
//!   what, and this copy is about to be sent to whoever provides the model.
//!   Encoding it again is how none of that goes with it, however small the
//!   picture was.
//! - **As a JPEG** at [`crate::covers::QUALITY`] when it arrived as one, and
//!   otherwise as whichever of PNG and JPEG comes out smaller. A screenshot's
//!   flat colour and lettering is smaller as a PNG, and sharper -- a JPEG
//!   blurs the edges of text -- while a photograph saved as a PNG is many
//!   times smaller as a JPEG. A PNG keeps any transparency; a JPEG is laid
//!   over white.
//!
//! # Pictures that are files too
//!
//! A picture sent to the assistant may also be one of the person's files --
//! picked from the Files app, or saved there from a chat. The file is the
//! original and stays as it is, in the files folder; what the model is shown
//! is still this copy, made the same way and kept with the message. See
//! [`with_files`] and [`of_file`]. A file that is a picture the model cannot
//! be shown (an iPhone's HEIC, an SVG) is not refused the way a pasted one
//! is: it is the person's file, the assistant is told its name, and only the
//! picture is missing.
//!
//! # What is refused
//!
//! A picture that cannot be read -- an iPhone's HEIC, an SVG, a file that
//! is not a picture at all -- and one over [`MAX_BYTES`]. A cover that cannot
//! be read is kept as it came, because no cover is worse; a picture for the
//! model would only be refused again by the provider, after the person had
//! typed their question and waited for an answer. Said now, while they are
//! still choosing what to send, it is something they can act on.

use std::io::Cursor;
use std::sync::Arc;

use image::{DynamicImage, ImageFormat};
use jatayu_core::Vault;
use jatayu_core::files::{FileEntry, FileKind};
use jatayu_core::id::FileId;
use jatayu_core::model::{Attachment, MediaKind};

use crate::covers;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::blocking;

/// The longest side a picture for the assistant keeps, in pixels: the box
/// OpenAI fits every picture into before looking at it.
pub const MAX_SIDE: u32 = 2048;

/// The largest picture taken, in bytes, before anything is done to it.
/// Roomy: a phone's photograph is a few megabytes, and a large PNG
/// screenshot of a wide display not much more.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;

/// How many pictures one message may carry. Each is a thousand tokens or
/// more on every turn it is replayed in, and a question about more than a
/// handful of pictures at once is better asked about each.
pub const MAX_PER_MESSAGE: usize = 4;

/// How many files one message may carry. The model is told each one's name
/// on every turn the message is replayed in, so the number is kept to what a
/// person could mean by "these".
pub const MAX_FILES_PER_MESSAGE: usize = 20;

/// A picture made ready for the model, not yet stored.
#[derive(Debug)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Why a picture was refused, in words for the person who sent it.
const UNREADABLE: &str = "That picture could not be read. JPEG, PNG, WebP, GIF and BMP \
                          pictures can be sent to the assistant.";

/// `bytes` made into a picture for the model -- see the module doc -- or the
/// reason it cannot be.
///
/// Synchronous and not cheap: decoding a phone's photograph and encoding it
/// again is a good part of a second. [`store`] runs it on the blocking pool;
/// so must anything else that calls it.
pub fn prepare(bytes: &[u8]) -> CommandResult<Prepared> {
    if bytes.len() > MAX_BYTES {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("That picture is over {} MB.", MAX_BYTES / (1024 * 1024)),
        ));
    }
    let Some((image, _)) = covers::decoded_upright(bytes) else {
        return Err(CommandError::new(codes::NOT_AN_IMAGE, UNREADABLE));
    };
    let fits = image.width().max(image.height()) <= MAX_SIDE;
    let image = if fits { image } else { covers::scaled_down(&image, MAX_SIDE) };
    let (width, height) = (image.width(), image.height());

    let unreadable = || CommandError::new(codes::NOT_AN_IMAGE, UNREADABLE);
    let as_jpeg = covers::jpeg(&covers::flattened(&image)).ok_or_else(unreadable)?;
    let photograph = jatayu_core::media::sniff_mime(bytes) == "image/jpeg";
    let (bytes, mime) = match (photograph, png(&image)) {
        (false, Some(as_png)) if as_png.len() <= as_jpeg.len() => (as_png, "image/png"),
        _ => (as_jpeg, "image/jpeg"),
    };
    Ok(Prepared { bytes, mime, width, height })
}

/// `image` as a PNG, carrying nothing but its pixels. `None` if the encoder
/// will not take it, which leaves the JPEG.
fn png(image: &DynamicImage) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    // Eight bits a channel: a sixteen-bit PNG is twice the bytes for depth
    // no model looks at.
    let image = if image.has_alpha() {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    };
    image.write_to(&mut out, ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Make `bytes` a picture for the model and put it in the vault: [`prepare`],
/// then `put_blob`, both on the blocking pool. `filename` is what it was
/// called where it came from, for the thread to show; a name with a path in
/// it keeps only its last part.
pub async fn store(
    vault: &Arc<Vault>,
    bytes: Vec<u8>,
    filename: String,
) -> CommandResult<Attachment> {
    let vault = vault.clone();
    blocking(move || {
        let prepared = prepare(&bytes)?;
        attach(&vault, &prepared, &filename)
    })
    .await
}

/// A picture made ready by [`prepare`], put in the vault and described as
/// the attachment a message carries. Shared by every way a picture arrives.
fn attach(vault: &Vault, prepared: &Prepared, filename: &str) -> CommandResult<Attachment> {
    let blob = vault.put_blob(&prepared.bytes)?;
    Ok(Attachment {
        blob,
        kind: MediaKind::Image,
        mime: prepared.mime.to_string(),
        filename: display_name(filename, prepared.mime),
        byte_len: prepared.bytes.len() as u64,
        width: Some(prepared.width),
        height: Some(prepared.height),
        duration_ms: None,
        caption: String::new(),
    })
}

/// The picture for the model of a file in the files catalog: the file read
/// back, and made into one the way [`store`] makes any. `None` for a file
/// that is not a picture a model can be shown -- not an image at all, over
/// [`MAX_BYTES`], one `prepare` cannot decode (HEIC, SVG), or one that cannot
/// be read right now, a locked folder, say. Never an error for those: the
/// file is still the person's to hand over, and the model is still told its
/// name.
///
/// Synchronous and not cheap, as [`prepare`] is; run it on the blocking pool.
pub fn of_file(vault: &Vault, file: &FileEntry) -> CommandResult<Option<Attachment>> {
    if file.kind != FileKind::Image || file.size > MAX_BYTES as u64 {
        return Ok(None);
    }
    let bytes = match vault.read_file_range(file.id, 0, file.size) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(file = %file.id, error = %e, "could not read a file to show the model");
            return Ok(None);
        }
    };
    match prepare(&bytes) {
        Ok(prepared) => attach(vault, &prepared, &file.name).map(Some),
        Err(e) if e.code == codes::NOT_AN_IMAGE || e.code == codes::TOO_LARGE => Ok(None),
        Err(e) => Err(e),
    }
}

/// What a message from the interface carries, checked: `attachments` as
/// [`checked`] checks them, and `files` -- ids in the files catalog -- each
/// looked up, with the pictures among them added to the pictures the model
/// is shown. Answers both, ready for the turn.
///
/// Refused whole, before anything is written, when a file is not in the
/// catalog or is in the trash, when there are more than
/// [`MAX_FILES_PER_MESSAGE`], or when the pictures the model would be shown,
/// theirs and the files', come to more than [`MAX_PER_MESSAGE`]. Repeats are
/// dropped, and so is a picture for a file that `attachments` already has one
/// of, by the same name.
pub async fn with_files(
    vault: &Arc<Vault>,
    attachments: Vec<Attachment>,
    files: Vec<FileId>,
) -> CommandResult<(Vec<Attachment>, Vec<FileId>)> {
    let vault = vault.clone();
    blocking(move || {
        let mut pictures = checked(&vault, attachments)?;
        let mut ids: Vec<FileId> = Vec::with_capacity(files.len());
        for id in files {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.len() > MAX_FILES_PER_MESSAGE {
            return Err(CommandError::new(
                codes::INVALID,
                format!("At most {MAX_FILES_PER_MESSAGE} files can go with one message."),
            ));
        }
        for id in &ids {
            // Not found is the vault's own error, which says so.
            let file = vault.file(*id)?;
            if file.status.is_trashed() {
                return Err(CommandError::new(
                    codes::INVALID,
                    format!("{} is in the trash, so it cannot be sent.", file.name),
                ));
            }
            // A clinical document's page or a meal's photograph is Health's,
            // shown to a model only through the health tools and their gates.
            // The composer never offers one; a caller that names one anyway
            // is refused rather than let past them.
            if file.origin == jatayu_core::files::FileOrigin::Health {
                return Err(CommandError::new(
                    codes::FORBIDDEN,
                    format!("{} is one of Health's files and cannot be sent here.", file.name),
                ));
            }
            // A picture the interface already made ready from this very file
            // -- a composer that keeps the original in Files and prepares the
            // copy for the model, both -- is not made a second time, nor
            // counted against the cap twice. Told apart by name, which is all
            // an attachment keeps of where it came from.
            if pictures.iter().any(|p| p.filename == display_name(&file.name, &p.mime)) {
                continue;
            }
            if let Some(picture) = of_file(&vault, &file)? {
                if pictures.len() >= MAX_PER_MESSAGE {
                    return Err(CommandError::new(
                        codes::INVALID,
                        format!("At most {MAX_PER_MESSAGE} pictures can go with one message."),
                    ));
                }
                pictures.push(picture);
            }
        }
        Ok((pictures, ids))
    })
    .await
}

/// What to call a stored picture: its own name without any directories, or
/// "picture.jpg" when it came without one. The extension is left as it came
/// -- the name is for a person to recognise, and the type travels in `mime`.
fn display_name(given: &str, mime: &str) -> String {
    let base = given.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if base.is_empty() {
        let extension = if mime == "image/png" { "png" } else { "jpg" };
        format!("picture.{extension}")
    } else {
        base.chars().take(200).collect()
    }
}

/// Check that `attachments` are pictures this vault holds, at most
/// [`MAX_PER_MESSAGE`] of them, and give each the type its bytes say it is.
///
/// For a message from the interface, which names pictures it stored through
/// `prepare_picture`: what it says about them is taken for the name and the
/// size, which are only shown, and not for the type, which is what the
/// model is told it is being sent.
pub fn checked(vault: &Vault, attachments: Vec<Attachment>) -> CommandResult<Vec<Attachment>> {
    if attachments.len() > MAX_PER_MESSAGE {
        return Err(CommandError::new(
            codes::INVALID,
            format!("At most {MAX_PER_MESSAGE} pictures can go with one message."),
        ));
    }
    attachments
        .into_iter()
        .map(|mut a| {
            let bytes = vault.blob(a.blob)?;
            let mime = jatayu_core::media::sniff_mime(&bytes);
            if !matches!(mime, "image/jpeg" | "image/png") {
                return Err(CommandError::new(codes::NOT_AN_IMAGE, UNREADABLE));
            }
            a.kind = MediaKind::Image;
            a.mime = mime.to_string();
            a.byte_len = bytes.len() as u64;
            a.caption.clear();
            Ok(a)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};

    /// Something like a photograph: a smooth gradient with grain over it, so
    /// a PNG of it is large and a JPEG of it is not.
    fn photograph(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| {
            let grain = ((x * 7919 + y * 104_729) % 37) as u8;
            Rgb([(x * 200 / width) as u8 + grain, (y * 200 / height) as u8 + grain, 90 + grain])
        })
    }

    /// Something like a screenshot: flat panels and a few sharp lines.
    fn screenshot(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            if y % 40 == 0 || x % 300 == 0 {
                Rgba([20, 20, 20, 255])
            } else if x < 300 {
                Rgba([240, 240, 245, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        })
    }

    fn jpeg_of(image: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 95).encode_image(image).unwrap();
        out
    }

    fn png_of(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(image, image.width(), image.height(), image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn a_phones_photograph_comes_down_to_the_models_size_as_a_jpeg() {
        let taken = jpeg_of(&photograph(2600, 1950));
        let ready = prepare(&taken).unwrap();
        assert_eq!(ready.mime, "image/jpeg");
        assert_eq!((ready.width, ready.height), (MAX_SIDE, 1536), "4:3 fitted into 2048");
        let back = image::load_from_memory(&ready.bytes).unwrap();
        assert_eq!((back.width(), back.height()), (ready.width, ready.height));
        assert!(ready.bytes.len() < taken.len());
    }

    #[test]
    fn a_small_jpeg_is_still_encoded_again_so_nothing_it_carried_goes_with_it() {
        // An APP1 segment where a camera writes its EXIF block -- where a
        // photograph keeps the place it was taken -- spliced in after SOI.
        let plain = jpeg_of(&photograph(640, 480));
        let marker = b"Exif\0\0GPS 51.5007 N 0.1246 W";
        let mut carried = vec![0xFF, 0xD8, 0xFF, 0xE1];
        carried.extend_from_slice(&((marker.len() + 2) as u16).to_be_bytes());
        carried.extend_from_slice(marker);
        carried.extend_from_slice(&plain[2..]);
        assert!(image::load_from_memory(&carried).is_ok(), "still a picture");

        let ready = prepare(&carried).unwrap();
        assert_eq!((ready.width, ready.height), (640, 480), "never enlarged, never shrunk");
        let gps = b"GPS 51.5007";
        assert!(!ready.bytes.windows(gps.len()).any(|w| w == gps), "the location is gone");
    }

    #[test]
    fn a_screenshot_stays_a_png_where_that_is_smaller() {
        let shot = png_of(&screenshot(1600, 900));
        let ready = prepare(&shot).unwrap();
        assert_eq!(ready.mime, "image/png");
        assert_eq!((ready.width, ready.height), (1600, 900));
    }

    #[test]
    fn a_photograph_saved_as_a_png_becomes_a_jpeg() {
        let rgba = DynamicImage::ImageRgb8(photograph(1200, 900)).to_rgba8();
        let ready = prepare(&png_of(&rgba)).unwrap();
        assert_eq!(ready.mime, "image/jpeg");
    }

    #[test]
    fn what_cannot_be_read_is_refused_with_what_can() {
        for junk in [&b"not a picture"[..], b"<svg xmlns='http://www.w3.org/2000/svg'/>", b""] {
            let e = prepare(junk).unwrap_err();
            assert_eq!(e.code, codes::NOT_AN_IMAGE);
            assert!(e.message.contains("JPEG, PNG"), "{}", e.message);
        }
    }

    #[test]
    fn a_picture_past_the_limit_is_refused_before_it_is_decoded() {
        let e = prepare(&vec![0u8; MAX_BYTES + 1]).unwrap_err();
        assert_eq!(e.code, codes::TOO_LARGE);
    }

    #[test]
    fn a_name_keeps_only_its_last_part_and_a_missing_one_is_made_up() {
        assert_eq!(display_name("/home/me/Pictures/receipt.jpg", "image/jpeg"), "receipt.jpg");
        assert_eq!(display_name("C:\\Users\\me\\shot.png", "image/png"), "shot.png");
        assert_eq!(display_name("  ", "image/png"), "picture.png");
        assert_eq!(display_name("", "image/jpeg"), "picture.jpg");
    }
}
