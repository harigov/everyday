//! An SVG drawn into pixels in this process, by resvg.
//!
//! Milliseconds for a diagram, and nothing to start: no browser, no
//! subprocess, the same on every platform. So an SVG is drawn here unless it
//! needs what only a browser has -- see [`needs_browser`].
//!
//! # What an SVG may cost
//!
//! The source is small, but an SVG can multiply itself: ten levels of `<use>`
//! each repeating the last ten times is ten billion shapes from a few lines.
//! So before resvg sees it, it is read once with roxmltree and every
//! reference expanded on paper ([`preflight`]): past [`MAX_ELEMENTS`], or
//! nested past [`MAX_DEPTH`], or referring to itself, it is refused, and a
//! DTD -- the other way XML multiplies itself -- is refused outright. A
//! picture written into it is drawn only if its own header says it is a size
//! worth drawing ([`MAX_SIDE`] square at most), since a few kilobytes of
//! PNG can declare a picture of gigabytes. The caller bounds the time too.
//!
//! # What an SVG may reach
//!
//! Nothing outside itself. An `<image>` whose address is a file path would
//! be read from this computer's disk by resvg's default resolver; here only
//! pictures written into the SVG as `data:` addresses are drawn, and any
//! other address draws nothing. A web address was never fetched: resvg has
//! no network. Fonts are this computer's own, loaded once.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use resvg::tiny_skia;
use resvg::usvg::{self, fontdb};

use super::{MAX_PIXELS, MAX_SIDE, Rendered};
use crate::error::{CommandError, CommandResult, codes};

/// Draw `source` `width` pixels wide -- its own width when `None` -- keeping
/// its proportions, over white.
///
/// Synchronous and CPU-bound: call it from the blocking pool.
pub fn render(source: &str, width: Option<u32>) -> CommandResult<Rendered> {
    let tree = parse(source)?;
    let size = tree.size();
    let (own_w, own_h) = (size.width(), size.height());
    let wanted = width.map_or(own_w, |w| w as f32);
    let scale = wanted / own_w;
    let (w, h) = fit((own_w * scale).round(), (own_h * scale).round())?;
    let (sx, sy) = (w as f32 / own_w, h as f32 / own_h);

    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| CommandError::new(codes::INVALID, "that drawing has no size"))?;
    // A drawing is looked at on a dark panel as often as on a light one;
    // black lines on a transparent ground disappear on the dark one. Paper
    // is white unless the SVG paints its own.
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(&tree, tiny_skia::Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    let png = pixmap
        .encode_png()
        .map_err(|e| CommandError::new(codes::IO, format!("could not encode the drawing: {e}")))?;
    Ok(Rendered { png, width: w, height: h, notes: Vec::new() })
}

/// Whether `source` uses what resvg does not draw -- HTML inside a
/// `<foreignObject>` -- and so has to go to the browser to come out right.
pub fn needs_browser(source: &str) -> bool {
    source.contains("<foreignObject") || source.contains(":foreignObject")
}

/// What is said of a `<foreignObject>` drawn without a browser, which resvg
/// leaves blank.
pub const FOREIGN_BLANK: &str = "This SVG has HTML in a <foreignObject>, which cannot be drawn on \
                                 this computer, so that part came out blank. Use SVG <text> \
                                 instead.";

/// The size `source` says it is, in its own units: what a drawing asked for
/// at its own width is that wide.
pub fn size(source: &str) -> CommandResult<(f32, f32)> {
    let size = parse(source)?.size();
    Ok((size.width(), size.height()))
}

/// The most elements an SVG may come to once every reference in it is
/// expanded. A detailed diagram is a few thousand.
pub const MAX_ELEMENTS: u64 = 50_000;

/// The deepest elements may nest, references followed.
pub const MAX_DEPTH: usize = 256;

const XLINK: &str = "http://www.w3.org/1999/xlink";

fn invalid(why: impl std::fmt::Display) -> CommandError {
    CommandError::new(codes::INVALID, format!("that is not an SVG that can be drawn: {why}"))
}

/// Refuse what would cost far more to draw than its size suggests. See the
/// module doc.
fn preflight(source: &str) -> CommandResult<()> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 200_000,
        ..roxmltree::ParsingOptions::default()
    };
    let doc = roxmltree::Document::parse_with_options(source, options).map_err(invalid)?;
    let ids: HashMap<&str, roxmltree::Node<'_, '_>> = doc
        .descendants()
        .filter(roxmltree::Node::is_element)
        .filter_map(|n| n.attribute("id").map(|id| (id, n)))
        .collect();
    let mut counted = HashMap::new();
    let total = expanded(doc.root_element(), &ids, &mut counted, &mut Vec::new())?;
    if total > MAX_ELEMENTS {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!(
                "that SVG comes to more than {MAX_ELEMENTS} shapes once its <use> references \
                 are expanded; draw something simpler"
            ),
        ));
    }
    Ok(())
}

/// How many elements `node` comes to, its children and whatever it refers to
/// by `href` counted in. Stops counting once past [`MAX_ELEMENTS`].
fn expanded(
    node: roxmltree::Node<'_, '_>,
    ids: &HashMap<&str, roxmltree::Node<'_, '_>>,
    counted: &mut HashMap<roxmltree::NodeId, u64>,
    path: &mut Vec<roxmltree::NodeId>,
) -> CommandResult<u64> {
    if let Some(&n) = counted.get(&node.id()) {
        return Ok(n);
    }
    if path.contains(&node.id()) {
        return Err(invalid("it refers to itself"));
    }
    if path.len() >= MAX_DEPTH {
        return Err(invalid(format!("it nests more than {MAX_DEPTH} deep")));
    }
    path.push(node.id());
    let mut n = 1u64;
    let referred = node
        .attribute("href")
        .or_else(|| node.attribute((XLINK, "href")))
        .and_then(|href| href.strip_prefix('#'))
        .and_then(|id| ids.get(id).copied());
    for next in node.children().filter(roxmltree::Node::is_element).chain(referred) {
        n = n.saturating_add(expanded(next, ids, counted, path)?);
        if n > MAX_ELEMENTS {
            break;
        }
    }
    path.pop();
    counted.insert(node.id(), n);
    Ok(n)
}

/// Whether a picture written into an SVG is one worth drawing: its header
/// read, and its size within [`MAX_SIDE`] and [`MAX_PIXELS`].
fn worth_drawing(data: &[u8]) -> bool {
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(w, h)| {
            w <= MAX_SIDE && h <= MAX_SIDE && u64::from(w) * u64::from(h) <= MAX_PIXELS
        })
}

fn parse(source: &str) -> CommandResult<usvg::Tree> {
    preflight(source)?;
    let mut options = usvg::Options {
        fontdb: fonts().clone(),
        font_family: sans().to_string(),
        ..usvg::Options::default()
    };
    // Pictures written into the SVG are drawn if they are worth drawing, an
    // SVG among them only if it passes the same reading as this one; a path
    // or an address is not read. See the module doc.
    let data = usvg::ImageHrefResolver::default_data_resolver();
    options.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: Box::new(move |mime, bytes, options| {
            let drawable = if mime == "image/svg+xml" {
                std::str::from_utf8(&bytes).is_ok_and(|svg| preflight(svg).is_ok())
            } else {
                worth_drawing(&bytes)
            };
            if drawable { data(mime, bytes, options) } else { None }
        }),
        resolve_string: Box::new(|_, _| None),
    };
    usvg::Tree::from_str(source, &options).map_err(invalid)
}

/// The pixel size to draw at: `w` by `h`, shrunk whole if either side is
/// past [`MAX_SIDE`] or the area past [`MAX_PIXELS`].
fn fit(w: f32, h: f32) -> CommandResult<(u32, u32)> {
    if !(w.is_finite() && h.is_finite()) || w < 1.0 || h < 1.0 {
        return Err(CommandError::new(
            codes::INVALID,
            "that drawing has no size: give the svg a width and height, or a viewBox",
        ));
    }
    let side = (MAX_SIDE as f32 / w.max(h)).min(1.0);
    let area = ((MAX_PIXELS as f32) / (w * h)).sqrt().min(1.0);
    let k = side.min(area);
    Ok((((w * k).round() as u32).max(1), ((h * k).round() as u32).max(1)))
}

/// This computer's fonts, read once. Reading them is a walk over every font
/// file installed -- a few hundred milliseconds -- which a drawing should not
/// pay each time.
fn fonts() -> &'static Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let has = |family: &str| {
            db.faces().any(|face| face.families.iter().any(|(name, _)| name == family))
        };
        // The generic families an SVG names, pointed at what this computer
        // has: fontdb's defaults are Windows' (Arial, Times New Roman, Courier
        // New), which most Linux machines lack, and a missing family falls
        // back to whatever font happens to have the glyph.
        let first = |names: &[&str]| names.iter().find(|n| has(n)).map(|n| (*n).to_string());
        let sans = first(&[
            "Inter",
            "Helvetica Neue",
            "Helvetica",
            "Arial",
            "Segoe UI",
            "Noto Sans",
            "DejaVu Sans",
            "Liberation Sans",
            "Ubuntu",
        ]);
        let serif = first(&[
            "Georgia",
            "Times New Roman",
            "Noto Serif",
            "DejaVu Serif",
            "Liberation Serif",
        ]);
        let mono = first(&[
            "Menlo",
            "Consolas",
            "JetBrains Mono",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Ubuntu Mono",
        ]);
        if let Some(sans) = sans {
            db.set_sans_serif_family(sans);
        }
        if let Some(serif) = serif {
            db.set_serif_family(serif);
        }
        if let Some(mono) = mono {
            db.set_monospace_family(mono);
        }
        Arc::new(db)
    })
}

/// The family text with none named is drawn in.
fn sans() -> &'static str {
    static SANS: OnceLock<String> = OnceLock::new();
    SANS.get_or_init(|| fonts().family_name(&fontdb::Family::SansSerif).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    #[test]
    fn a_drawing_comes_out_at_the_width_asked_with_its_proportions() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100">
            <rect x="0" y="0" width="100" height="100" fill="#ff0000"/>
        </svg>"##;
        let out = render(svg, Some(400)).unwrap();
        assert_eq!((out.width, out.height), (400, 200));
        let img = decoded(&out.png);
        assert_eq!(img.dimensions(), (400, 200));
        // Red where the rectangle is, white paper beside it.
        assert_eq!(img.get_pixel(50, 100).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(350, 100).0, [255, 255, 255, 255]);
    }

    #[test]
    fn its_own_size_is_kept_when_no_width_is_asked() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="240"/>"#;
        let out = render(svg, None).unwrap();
        assert_eq!((out.width, out.height), (320, 240));
    }

    #[test]
    fn an_enormous_drawing_is_shrunk_whole() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="10000"/>"#;
        let out = render(svg, None).unwrap();
        assert_eq!(out.width, MAX_SIDE);
        assert_eq!(out.height, MAX_SIDE / 2);
        assert!(u64::from(out.width) * u64::from(out.height) <= MAX_PIXELS);
    }

    #[test]
    fn a_picture_on_this_computers_disk_is_not_read_into_a_drawing() {
        let dir = tempfile::tempdir().unwrap();
        let red = dir.path().join("red.png");
        image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255])).save(&red).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
                <image href="{}" width="10" height="10"/>
            </svg>"#,
            red.display()
        );
        let img = decoded(&render(&svg, None).unwrap().png);
        assert_eq!(img.get_pixel(5, 5).0, [255, 255, 255, 255]);
    }

    #[test]
    fn what_is_not_an_svg_is_refused_in_words() {
        let err = render("<p>hello</p>", None).unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        let err = render(r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#, None)
            .unwrap_err();
        assert_eq!(err.code, codes::INVALID);
    }

    #[test]
    fn an_svg_that_multiplies_itself_is_refused_before_it_is_drawn() {
        // Ten levels, each using the last ten times: ten billion shapes.
        let mut svg = String::from(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><defs><rect id="l0" width="1" height="1"/>"#,
        );
        for level in 1..=10 {
            svg.push_str(&format!(r#"<g id="l{level}">"#));
            for _ in 0..10 {
                svg.push_str(&format!(r##"<use xlink:href="#l{}"/>"##, level - 1));
            }
            svg.push_str("</g>");
        }
        svg.push_str(r##"</defs><use href="#l10"/></svg>"##);
        let started = std::time::Instant::now();
        let err = render(&svg, None).unwrap_err();
        assert_eq!(err.code, codes::TOO_LARGE, "{}", err.message);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        let looped = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g></svg>"##;
        assert_eq!(render(looped, None).unwrap_err().code, codes::INVALID);

        let entities = r#"<!DOCTYPE svg [<!ENTITY a "aaaaaaaaaa">]>
            <svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&a;</text></svg>"#;
        assert_eq!(render(entities, None).unwrap_err().code, codes::INVALID);
    }

    #[test]
    fn a_picture_inside_declaring_an_enormous_size_is_not_drawn() {
        // A PNG header claiming 100000 x 100000 pixels.
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&100_000u32.to_be_bytes());
        png.extend_from_slice(&100_000u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!worth_drawing(&png));
        let mut small = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut small), image::ImageFormat::Png)
            .unwrap();
        assert!(worth_drawing(&small));
    }

    #[test]
    fn foreign_html_is_sent_to_the_browser() {
        assert!(needs_browser("<svg><foreignObject><div>hi</div></foreignObject></svg>"));
        assert!(!needs_browser("<svg><text>hi</text></svg>"));
    }
}
