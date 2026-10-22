//! A cover, made the size a cover is drawn at.
//!
//! A shelf draws a cover about two hundred pixels across, and the detail
//! panel draws it narrower still. What arrives is whatever the site happened
//! to serve: a shop's jacket at 1430 by 2560, a phone's photograph of a dish
//! at four thousand pixels and four megabytes, a PNG of a bottle on a clear
//! background. Kept as they came, a thousand of them is a few hundred
//! megabytes of vault spent on pixels nobody will ever see -- sealed,
//! backed up, exported and carried to every device the vault goes to.
//!
//! So every cover is made into one on its way in, whichever way it came: a
//! lookup's ([`crate::websearch::apply_and_cover`]), a pasted or picked
//! address (`fetch_image`), the assistant's or a script's `image_url`
//! ([`crate::websearch::prefetch_cover`]) and a file off the disk
//! (`shrink_cover`). One function, so a cover cannot be small one way and
//! enormous another.
//!
//! # What is done to it
//!
//! - **Turned the right way up**, from the orientation a camera wrote into
//!   it -- re-encoding drops that note, so it has to be acted on first.
//! - **Shrunk** until its longer side is at most [`MAX_SIDE`] pixels, its
//!   proportions kept. Never enlarged.
//! - **Flattened onto white**, where it was see-through: a JPEG has no
//!   transparency, and a bottle photographed for a shop looks as it did
//!   there.
//! - **Encoded as a JPEG** at [`QUALITY`], which at this size is a few dozen
//!   kilobytes and not distinguishable from the original on a card.
//!
//! Everything else the file carried is gone with the re-encoding -- a
//! camera's make, a time, and the place a photograph was taken, which is
//! worth not keeping beside a list of what somebody reads.
//!
//! # When it is left as it came
//!
//! A picture already no bigger than a cover, which re-encoding would not
//! make at least a fifth smaller -- a modest JPEG, a PNG logo of a few
//! kilobytes -- is kept byte for byte: a second lossy pass would cost detail
//! for nothing worth having. (It keeps
//! whatever it carried, too; a camera's photograph is never that small.)
//! And a picture this cannot read -- an SVG, an AVIF, anything past
//! [`limits`] -- is kept as it came, exactly as every cover was before this
//! module existed: the alternative is no cover, which is worse in every case
//! (see [`crate::websearch::apply_and_cover`]'s own argument).

use std::io::Cursor;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::id::BlobId;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits, RgbImage};

use crate::error::CommandResult;
use crate::service::blocking;

/// The longest side a stored cover keeps, in pixels.
///
/// A card is at most a little under three hundred pixels wide and a cover is
/// usually taller than it is wide, so this is the height of the widest card
/// drawn twice over for a sharp screen -- with nothing to spare beyond that.
pub const MAX_SIDE: u32 = 800;

/// The JPEG quality a cover is encoded at, out of a hundred. Where the
/// curve flattens: higher costs bytes nobody can see, lower starts to show
/// in a flat sky or a jacket's gradient.
pub const QUALITY: u8 = 85;

/// `bytes` made into a cover, or `None` to keep them as they are -- see the
/// module doc for when that is.
///
/// Synchronous and not cheap: decoding a large photograph is tens of
/// milliseconds. [`store`] runs it on the blocking pool; so must anything
/// else that calls it.
pub fn shrink(bytes: &[u8]) -> Option<Vec<u8>> {
    let (image, upright) = decoded_upright(bytes)?;

    let (width, height) = (image.width(), image.height());
    let fits = width.max(height) <= MAX_SIDE;
    let small = if fits { image } else { scaled_down(&image, MAX_SIDE) };
    let encoded = jpeg(&flattened(&small))?;

    // Already a cover's size and the right way up: kept as it came unless
    // re-encoding saves at least a fifth -- a second, lossier generation of a
    // picture is not worth a few kilobytes. One that had to be turned is
    // re-encoded whatever the saving, since only the new copy is upright.
    if fits && upright && encoded.len() * 5 > bytes.len() * 4 {
        return None;
    }
    Some(encoded)
}

/// `bytes` decoded, within [`limits`], and turned the right way up from the
/// orientation a camera wrote into it -- with whether it already was.
/// `None` for anything that cannot be read. Shared with
/// [`crate::pictures`], which makes pictures for the assistant the same way
/// with a different size in mind.
pub(crate) fn decoded_upright(bytes: &[u8]) -> Option<(DynamicImage, bool)> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(limits());
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some((image, orientation == Orientation::NoTransforms))
}

/// Make `bytes` a cover and put it in the vault's blob store: [`shrink`],
/// then `put_blob`, both on the blocking pool.
pub async fn store(vault: &Arc<Vault>, bytes: Vec<u8>) -> CommandResult<BlobId> {
    let vault = vault.clone();
    blocking(move || {
        let kept = shrink(&bytes).unwrap_or(bytes);
        Ok(vault.put_blob(&kept)?)
    })
    .await
}

/// How large a picture this will try to decode.
///
/// A cover is fetched from an address somebody else chose, and ten
/// megabytes of a compressed image can claim to be a hundred thousand
/// pixels square -- a decompression bomb, gigabytes of memory for one
/// picture. Past these, the picture is kept as it came instead (see the
/// module doc), which is no worse than before this module existed.
fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// `image` with its longer side brought down to `max_side` -- [`MAX_SIDE`],
/// for a cover.
///
/// In two steps when it is far larger: a fast box filter to twice the size
/// wanted, then Lanczos for the last halving. The box filter alone leaves a
/// cover soft at the end, and Lanczos alone over a phone's photograph is
/// most of a second of work for detail the box filter would have averaged
/// away anyway.
pub(crate) fn scaled_down(image: &DynamicImage, max_side: u32) -> DynamicImage {
    let near = max_side * 2;
    let staged;
    let from = if image.width().max(image.height()) > near {
        staged = image.thumbnail(near, near);
        &staged
    } else {
        image
    };
    from.resize(max_side, max_side, FilterType::Lanczos3)
}

/// `image` as plain RGB, anything see-through laid over white.
pub(crate) fn flattened(image: &DynamicImage) -> RgbImage {
    if !image.has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (out, pixel) in rgb.pixels_mut().zip(rgba.pixels()) {
        let [r, g, b, a] = pixel.0;
        let over_white = |c: u8| -> u8 {
            let (c, a) = (u32::from(c), u32::from(a));
            ((c * a + 255 * (255 - a) + 127) / 255) as u8
        };
        out.0 = [over_white(r), over_white(g), over_white(b)];
    }
    rgb
}

/// `image` encoded as a JPEG at [`QUALITY`]: baseline, with colour at half
/// resolution each way (4:2:0, which `jpeg-encoder` picks below quality 90
/// and which is libjpeg's own default). On real jackets that is a fifth
/// smaller than `image`'s own encoder at the same quality, which keeps
/// colour at half resolution one way only.
///
/// Two things measured and left out. Progressive encoding came out slightly
/// larger. And Huffman tables fitted to the picture, which would save a
/// further two or three per cent, produce files `image`'s own decoder reads
/// back wrong -- flat white decodes as green -- though libjpeg reads them
/// fine. Anything here that decodes a stored cover again would then spoil
/// it, so the standard tables it is; `stored_covers_read_back_as_they_were_drawn`
/// is the test that would catch it coming back.
///
/// `None` only for a picture the encoder cannot take -- one more than 65,535
/// pixels on a side, which [`MAX_SIDE`] has already made impossible.
pub(crate) fn jpeg(image: &RgbImage) -> Option<Vec<u8>> {
    let width = u16::try_from(image.width()).ok()?;
    let height = u16::try_from(image.height()).ok()?;
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, QUALITY);
    encoder.encode(image.as_raw(), width, height, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, Rgba, RgbaImage};

    /// A picture with something in it, so that a JPEG of it is not
    /// implausibly small: a gradient, which is what a jacket mostly is.
    fn gradient(width: u32, height: u32, alpha: impl Fn(u32, u32) -> u8) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x * 255 / width) as u8, (y * 255 / height) as u8, 120, alpha(x, y)])
        })
    }

    fn png(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(image, image.width(), image.height(), image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn jpeg_at(image: &RgbaImage, quality: u8) -> Vec<u8> {
        let rgb = DynamicImage::ImageRgba8(image.clone()).to_rgb8();
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).unwrap();
        out
    }

    fn decoded(bytes: &[u8]) -> DynamicImage {
        image::load_from_memory(bytes).unwrap()
    }

    #[test]
    fn a_shops_jacket_comes_home_a_cover_sized_jpeg_in_its_own_proportions() {
        let jacket = png(&gradient(1430, 2560, |_, _| 255));
        let cover = shrink(&jacket).expect("far larger than a cover");
        assert_eq!(everyday_core::media::sniff_mime(&cover), "image/jpeg");
        let kept = decoded(&cover);
        assert_eq!(kept.height(), MAX_SIDE, "the longer side is brought down to the limit");
        assert_eq!(kept.width(), 447, "and the shorter in proportion: 1430 * 800 / 2560");
        // A smooth gradient is a PNG's best case, so this is a floor; a real
        // jacket comes down by far more.
        assert!(cover.len() * 3 < jacket.len(), "{} bytes from {}", cover.len(), jacket.len());
    }

    #[test]
    fn a_wide_picture_is_limited_by_its_width() {
        let place = jpeg_at(&gradient(3000, 1000, |_, _| 255), 95);
        let kept = decoded(&shrink(&place).unwrap());
        assert_eq!((kept.width(), kept.height()), (MAX_SIDE, 267));
    }

    #[test]
    fn a_jpeg_already_a_covers_size_is_kept_byte_for_byte() {
        // A well-made JPEG of a cover's size: a second pass would cost detail
        // and save nothing worth having.
        let rgb = DynamicImage::ImageRgba8(gradient(400, 600, |_, _| 255)).to_rgb8();
        let small = jpeg(&rgb).unwrap();
        assert_eq!(shrink(&small), None);
    }

    #[test]
    fn a_wasteful_jpeg_of_a_covers_size_is_re_encoded() {
        // Saved at the top quality, it is several times what a cover needs.
        let heavy = jpeg_at(&gradient(500, 750, |x, y| ((x * y) % 255) as u8), 100);
        let cover = shrink(&heavy).expect("worth a fifth at least");
        assert!(cover.len() * 5 <= heavy.len() * 4, "{} from {}", cover.len(), heavy.len());
        let kept = decoded(&cover);
        assert_eq!((kept.width(), kept.height()), (500, 750), "and not resized");
    }

    #[test]
    fn stored_covers_read_back_as_they_were_drawn() {
        // Flat white and a busy picture, through this module's encoder and
        // back through `image`'s own decoder -- which, with Huffman tables
        // fitted to the picture, read flat white back as green.
        let white = RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
        let back = decoded(&jpeg(&white).unwrap()).to_rgb8();
        assert!(back.pixels().all(|p| p.0.iter().all(|c| *c >= 250)), "white stays white");

        let busy = DynamicImage::ImageRgba8(gradient(320, 480, |_, _| 255)).to_rgb8();
        let back = decoded(&jpeg(&busy).unwrap()).to_rgb8();
        let worst = back
            .pixels()
            .zip(busy.pixels())
            .flat_map(|(a, b)| a.0.into_iter().zip(b.0).map(|(x, y)| x.abs_diff(y)))
            .max()
            .unwrap();
        assert!(worst <= 24, "a gradient comes back within JPEG's own error, got {worst}");
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        // A PNG that is bigger than its JPEG is re-encoded, but not scaled.
        let tiny = png(&gradient(120, 180, |_, _| 255));
        let kept = decoded(&shrink(&tiny).unwrap_or(tiny));
        assert_eq!((kept.width(), kept.height()), (120, 180));
    }

    #[test]
    fn a_see_through_background_becomes_white_and_the_rest_is_untouched() {
        // Transparent down the left half, opaque down the right.
        let bottle = png(&gradient(1600, 1600, |x, _| if x < 800 { 0 } else { 255 }));
        let kept = decoded(&shrink(&bottle).unwrap()).to_rgb8();
        assert_eq!(kept.dimensions(), (MAX_SIDE, MAX_SIDE));
        let [r, g, b] = kept.get_pixel(10, 400).0;
        assert!(r > 245 && g > 245 && b > 245, "clear became white, got {:?}", [r, g, b]);
        let [_, _, b] = kept.get_pixel(790, 400).0;
        assert!((110..=130).contains(&b), "opaque kept its colour, got blue {b}");
    }

    #[test]
    fn what_cannot_be_read_is_left_as_it_came() {
        assert_eq!(shrink(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
        assert_eq!(shrink(b"not a picture at all"), None);
        assert_eq!(shrink(&[]), None);
    }

    #[test]
    fn a_picture_wider_than_the_limits_is_not_decoded() {
        // Cheap to write and to keep, and past `limits`: it is stored as it
        // came rather than decoded into memory.
        let ribbon = png(&gradient(20_000, 1, |_, _| 255));
        assert_eq!(shrink(&ribbon), None);
    }
}
