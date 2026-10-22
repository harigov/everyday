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

use everyday_core::Vault;
use everyday_core::model::{Attachment, MediaKind};
use image::{DynamicImage, ImageFormat};

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
    let photograph = everyday_core::media::sniff_mime(bytes) == "image/jpeg";
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
        let blob = vault.put_blob(&prepared.bytes)?;
        Ok(Attachment {
            blob,
            kind: MediaKind::Image,
            mime: prepared.mime.to_string(),
            filename: display_name(&filename, prepared.mime),
            byte_len: prepared.bytes.len() as u64,
            width: Some(prepared.width),
            height: Some(prepared.height),
            duration_ms: None,
            caption: String::new(),
        })
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
            let mime = everyday_core::media::sniff_mime(&bytes);
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
