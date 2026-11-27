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
//!
//! # Smaller still, for a small slot
//!
//! A list row draws a cover at thirty-four pixels and the Feed's sidebar a
//! show's artwork at eighteen, and a webview decodes a picture at the size
//! it is, not the size it is drawn: eight hundred pixels square is two and a
//! half megabytes of memory for an icon. So a blob's address may ask for one
//! of [`WIDTHS`] with `?w=`, and gets the picture made that wide
//! ([`scaled_blob`]) -- made when first asked for and kept in memory only,
//! since what comes out of the vault is plaintext and must not be written
//! anywhere it was not sealed.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, LazyLock, Mutex};

use image::codecs::png::{PngDecoder, PngEncoder};
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader, Limits, RgbImage,
};
use jatayu_core::Vault;
use jatayu_core::id::BlobId;
use jatayu_core::media;

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

// ---- smaller still, for a small slot ----------------------------------------

/// The widths a picture is made for a slot narrower than it: an icon in a
/// list, a cover in a grid, each at twice the density too. A blob address
/// asks for one with `?w=`; any other width gets the original.
pub const WIDTHS: [u32; 3] = [96, 200, 400];

/// `w` as one of [`WIDTHS`], or `None` for anything else.
pub fn width_tier(w: &str) -> Option<u32> {
    let w: u32 = w.parse().ok()?;
    WIDTHS.contains(&w).then_some(w)
}

/// `bytes` made `width` pixels across, its proportions kept: a JPEG, or a PNG
/// where it is see-through, so an icon on a dark page does not grow a white
/// box. `None` -- serve the original -- for a picture already no wider, one
/// that moves, or anything this cannot read.
///
/// Synchronous and not cheap, as [`shrink`] is.
pub fn scaled(bytes: &[u8], width: u32) -> Option<Vec<u8>> {
    let kind = media::sniff_mime(bytes);
    if !scalable(kind) || animated(kind, bytes) {
        return None;
    }
    // From the header alone: a picture with neither side wider than asked is
    // narrow enough whichever way up it is turned.
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(limits());
    let (w, h) = reader.into_dimensions().ok()?;
    if w.max(h) <= width {
        return None;
    }
    let (image, _) = decoded_upright(bytes)?;
    if image.width() <= width {
        return None;
    }
    let small = narrowed(&image, width);
    if image.color().has_alpha() {
        let rgba = small.to_rgba8();
        if rgba.pixels().any(|p| p.0[3] < u8::MAX) {
            let mut out = Vec::new();
            PngEncoder::new(&mut out)
                .write_image(rgba.as_raw(), rgba.width(), rgba.height(), ExtendedColorType::Rgba8)
                .ok()?;
            return Some(out);
        }
    }
    jpeg(&flattened(&small))
}

/// The kinds of picture [`scaled`] makes smaller. Not a GIF, which moves more
/// often than not, nor anything this module cannot read (see [`limits`]).
fn scalable(kind: &str) -> bool {
    matches!(kind, "image/jpeg" | "image/png" | "image/webp")
}

/// An animated PNG or WebP: made smaller it would stand still, so it is left
/// as it came. One that cannot be told is too.
fn animated(kind: &str, bytes: &[u8]) -> bool {
    match kind {
        "image/png" => {
            PngDecoder::new(Cursor::new(bytes)).and_then(|d| d.is_apng()).unwrap_or(true)
        }
        "image/webp" => {
            WebPDecoder::new(Cursor::new(bytes)).map(|d| d.has_animation()).unwrap_or(true)
        }
        _ => false,
    }
}

/// `image` brought to `width` across, the height in proportion -- in two steps
/// when it is far wider, for the reason [`scaled_down`] gives.
fn narrowed(image: &DynamicImage, width: u32) -> DynamicImage {
    let (w, h) = (u64::from(image.width()), u64::from(image.height()));
    let height = ((h * u64::from(width) + w / 2) / w).max(1) as u32;
    let staged;
    let from = if image.width() > width * 2 {
        staged = image.thumbnail_exact(width * 2, height * 2);
        &staged
    } else {
        image
    };
    from.resize_exact(width, height, FilterType::Lanczos3)
}

/// The largest blob [`scaled_blob`] reads to make a smaller copy of. A cover
/// is far under it; past it, the original is served as it always was.
const MAX_SCALED_INPUT: u64 = 32 * 1024 * 1024;

/// Most bytes of smaller copies kept in memory, all of them together.
const MADE_BYTES: usize = 32 * 1024 * 1024;

/// What a remembered "serve the original" is counted as.
const ORIGINAL_COST: usize = 64;

/// Smaller copies already made, by blob and width, the least lately used
/// going first. `None` remembers that the original is the answer, so a blob
/// that is not a picture, or is already narrow, is not read again to find out.
/// One remembered answer: the smaller copy (`None` for "the original"), and
/// when it was last used.
type MadeCopy = (Option<Arc<Vec<u8>>>, u64);

#[derive(Default)]
struct Made {
    copies: HashMap<(BlobId, u32), MadeCopy>,
    bytes: usize,
    clock: u64,
    /// How many times everything has been forgotten: a copy begun before
    /// the last time is not kept when it is finished.
    generation: u64,
}

fn cost(copy: &Option<Arc<Vec<u8>>>) -> usize {
    copy.as_ref().map_or(ORIGINAL_COST, |c| c.len())
}

impl Made {
    fn get(&mut self, key: (BlobId, u32)) -> Option<Option<Arc<Vec<u8>>>> {
        self.clock += 1;
        let clock = self.clock;
        let (copy, used) = self.copies.get_mut(&key)?;
        *used = clock;
        Some(copy.clone())
    }

    fn put(&mut self, key: (BlobId, u32), copy: Option<Arc<Vec<u8>>>) {
        if cost(&copy) > MADE_BYTES {
            return;
        }
        self.clock += 1;
        self.bytes += cost(&copy);
        if let Some((gone, _)) = self.copies.insert(key, (copy, self.clock)) {
            self.bytes -= cost(&gone);
        }
        while self.bytes > MADE_BYTES {
            let oldest = self.copies.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| *k);
            let Some((gone, _)) = oldest.and_then(|k| self.copies.remove(&k)) else { break };
            self.bytes -= cost(&gone);
        }
    }

    /// Keep a copy begun at `generation`, unless everything was forgotten
    /// while it was being made -- the vault locked or closed meanwhile.
    fn put_from(&mut self, generation: u64, key: (BlobId, u32), copy: Option<Arc<Vec<u8>>>) {
        if generation == self.generation {
            self.put(key, copy);
        }
    }

    fn clear(&mut self) {
        self.copies.clear();
        self.bytes = 0;
        self.generation += 1;
    }
}

static MADE: LazyLock<Mutex<Made>> = LazyLock::new(Mutex::default);

/// A few at a time -- half the processors, two to four: making a copy is ten or
/// twenty milliseconds of one, a grid scrolled into view asks for forty at
/// once, and the rest of the application wants the others meanwhile.
static MAKING: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| {
    let processors = std::thread::available_parallelism().map_or(4, |n| n.get());
    tokio::sync::Semaphore::new((processors / 2).clamp(2, 4))
});

/// Blob `id` made `width` across ([`scaled`]), from memory when it was made
/// before -- or `None` to serve the original: for a vault that is locked, a
/// blob that is not a picture or is already that narrow, and anything that
/// could not be read (whose original then answers with the error it has).
pub async fn scaled_blob(vault: &Arc<Vault>, id: BlobId, width: u32) -> Option<Arc<Vec<u8>>> {
    if !vault.is_unlocked() {
        return None;
    }
    let key = (id, width);
    if let Some(known) = MADE.lock().unwrap().get(key) {
        return known;
    }
    let _turn = MAKING.acquire().await.ok()?;
    // Made by whoever had the turn before this one, asking for the same.
    let generation = {
        let mut made = MADE.lock().unwrap();
        if let Some(known) = made.get(key) {
            return known;
        }
        made.generation
    };
    let store = vault.clone();
    let made = blocking(move || {
        if store.with_store(|s| s.blob_len(id))? > MAX_SCALED_INPUT {
            return Ok(None);
        }
        let head = store.with_store(|s| s.get_blob_range(id, 0, 16))?;
        if !scalable(media::sniff_mime(&head)) {
            return Ok(None);
        }
        let bytes = store.with_store(|s| s.get_blob(id))?;
        Ok(scaled(&bytes, width).map(Arc::new))
    })
    .await
    .ok()?;
    // A copy finished after the vault locked goes to whoever asked for it,
    // and is not kept: `forget_scaled` has already been and gone.
    if vault.is_unlocked() {
        MADE.lock().unwrap().put_from(generation, key, made.clone());
    }
    made
}

/// Forget every smaller copy made: pictures out of the vault, in the clear,
/// which should not outlive its being unlocked.
pub fn forget_scaled() {
    MADE.lock().unwrap().clear();
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
        assert_eq!(jatayu_core::media::sniff_mime(&cover), "image/jpeg");
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

    // ---- smaller still ------------------------------------------------------

    fn size(bytes: &[u8]) -> (u32, u32) {
        let image = decoded(bytes);
        (image.width(), image.height())
    }

    #[test]
    fn only_the_widths_on_offer_are_asked_for() {
        assert_eq!(width_tier("96"), Some(96));
        assert_eq!(width_tier("200"), Some(200));
        assert_eq!(width_tier("400"), Some(400));
        for other in ["800", "0", "97", "-96", "abc", ""] {
            assert_eq!(width_tier(other), None, "{other}");
        }
    }

    #[test]
    fn a_cover_is_made_as_wide_as_asked_in_its_own_proportions() {
        let cover = jpeg_at(&gradient(533, 800, |_, _| 255), 90);
        let small = scaled(&cover, 400).expect("wider than asked");
        assert_eq!(jatayu_core::media::sniff_mime(&small), "image/jpeg");
        assert_eq!(size(&small), (400, 600));

        let icon = scaled(&cover, 96).unwrap();
        assert_eq!(size(&icon), (96, 144));
    }

    #[test]
    fn a_picture_no_wider_than_asked_and_what_cannot_be_read_are_the_original() {
        let narrow = jpeg_at(&gradient(300, 450, |_, _| 255), 90);
        assert_eq!(scaled(&narrow, 400), None);
        // Tall and narrow: only the width counts.
        let tall = jpeg_at(&gradient(380, 1200, |_, _| 255), 90);
        assert_eq!(scaled(&tall, 400), None);
        assert_eq!(scaled(b"<svg xmlns='http://www.w3.org/2000/svg'/>", 96), None);
        assert_eq!(scaled(b"not a picture at all", 96), None);
        assert_eq!(scaled(&[], 96), None);
    }

    #[test]
    fn a_gif_is_left_moving() {
        let mut gif = Vec::new();
        let frame = gradient(500, 500, |_, _| 255);
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder.encode(frame.as_raw(), 500, 500, image::ExtendedColorType::Rgba8).unwrap();
        }
        assert_eq!(jatayu_core::media::sniff_mime(&gif), "image/gif");
        assert_eq!(scaled(&gif, 96), None);
    }

    #[test]
    fn a_see_through_icon_stays_see_through_and_an_opaque_one_is_a_jpeg() {
        let logo = png(&gradient(800, 800, |x, _| if x < 400 { 0 } else { 255 }));
        let small = scaled(&logo, 96).unwrap();
        assert_eq!(jatayu_core::media::sniff_mime(&small), "image/png");
        let back = decoded(&small).to_rgba8();
        assert_eq!(back.dimensions(), (96, 96));
        assert_eq!(back.get_pixel(5, 50).0[3], 0, "the clear half is still clear");
        assert_eq!(back.get_pixel(90, 50).0[3], 255);

        // An alpha channel with nothing see-through in it is a JPEG.
        let opaque = png(&gradient(800, 800, |_, _| 255));
        let small = scaled(&opaque, 96).unwrap();
        assert_eq!(jatayu_core::media::sniff_mime(&small), "image/jpeg");
    }

    #[test]
    fn copies_made_stay_within_their_budget_and_the_least_used_goes_first() {
        let mut made = Made::default();
        let copy = |n: usize| Some(Arc::new(vec![0u8; n]));
        let (a, b, c) = (BlobId::of(b"a"), BlobId::of(b"b"), BlobId::of(b"c"));
        made.put((a, 96), copy(MADE_BYTES / 2));
        made.put((b, 96), copy(MADE_BYTES / 3));
        // `a` is asked for again, so `b` is now the least lately used.
        assert!(made.get((a, 96)).is_some());
        made.put((c, 96), copy(MADE_BYTES / 3));
        assert!(made.bytes <= MADE_BYTES, "{} held", made.bytes);
        assert!(made.get((b, 96)).is_none(), "the least lately used went");
        assert!(made.get((a, 96)).is_some() && made.get((c, 96)).is_some());

        // "Serve the original" is remembered too, and costs next to nothing.
        made.put((b, 200), None);
        assert_eq!(made.get((b, 200)), Some(None));
        // Something larger than the whole budget is not kept at all.
        made.put((b, 400), copy(MADE_BYTES + 1));
        assert!(made.get((b, 400)).is_none());

        made.clear();
        assert!(made.get((a, 96)).is_none());
        assert_eq!(made.bytes, 0);
    }

    /// A copy that was still being made when the vault locked is handed to
    /// whoever asked, and not kept after the lock forgot the rest.
    #[test]
    fn a_copy_finished_after_everything_was_forgotten_is_not_kept() {
        let mut made = Made::default();
        let copy = Some(Arc::new(vec![0u8; 16]));
        let (a, b) = (BlobId::of(b"a"), BlobId::of(b"b"));
        let begun = made.generation;
        made.put_from(begun, (a, 96), copy.clone());
        assert!(made.get((a, 96)).is_some(), "nothing forgotten meanwhile");

        let begun = made.generation;
        made.clear();
        made.put_from(begun, (b, 96), copy.clone());
        assert!(made.get((b, 96)).is_none(), "begun before the lock");
        assert_eq!(made.bytes, 0);
        made.put_from(made.generation, (b, 96), copy);
        assert!(made.get((b, 96)).is_some(), "begun after it");
    }

    #[tokio::test]
    async fn a_blob_is_made_smaller_when_it_is_a_picture_and_never_while_locked() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = jatayu_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = Arc::new(jatayu_vault::create(dir.path(), cfg).unwrap());
        let cover =
            vault.put_blob(&jpeg_at(&gradient(600, 900, |x, y| (x ^ y) as u8), 90)).unwrap();
        let text = vault.put_blob(b"a note, not a picture").unwrap();

        let small = scaled_blob(&vault, cover, 200).await.expect("a picture wider than asked");
        assert_eq!(size(&small), (200, 300));
        // The second time is the same copy, from memory.
        let again = scaled_blob(&vault, cover, 200).await.unwrap();
        assert!(Arc::ptr_eq(&small, &again));
        assert!(scaled_blob(&vault, text, 200).await.is_none());

        vault.lock();
        assert!(scaled_blob(&vault, cover, 200).await.is_none(), "a locked vault serves nothing");
    }
}
