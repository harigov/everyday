//! Pictures drawn from markup: an SVG or an HTML page -- a diagram, a chart,
//! a card, a timeline -- turned into a PNG somebody can look at.
//!
//! One door, [`render`], for anything that wants a drawing: the assistant's
//! `draw_illustration` tool, and the `render_illustration` command. It knows
//! nothing of who asked or where the picture goes; [`crate::images::keep`]
//! puts a picture in Files.
//!
//! # Two engines, picked for speed
//!
//! - **An SVG** is drawn in this process by resvg ([`svg`]): milliseconds,
//!   on every platform, with no browser at all. It is what to reach for
//!   first, and the cheaper of the two for a model to write.
//! - **An HTML page** is drawn by a headless Chrome ([`html`]), kept warm for
//!   a couple of minutes so a reply with several drawings starts it once. So
//!   is an SVG that holds HTML in a `<foreignObject>`, which resvg leaves
//!   blank. Not on a computer without Chrome or Chromium -- [`can_draw_html`]
//!   says -- nor on Windows yet, where the browser cannot be driven.
//!
//! Neither reaches anything outside the markup: no file on this computer, no
//! address on the web. Each engine's module says how.

pub mod html;
pub mod svg;

use serde::{Deserialize, Serialize};

use crate::error::{CommandError, CommandResult, codes};
use crate::service::blocking;

/// The longest markup taken, in bytes. A drawing a model writes is a few
/// kilobytes; a picture inlined as `data:` can be more, and past this it is
/// the wrong tool.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// The longest side of a drawing, in pixels.
pub const MAX_SIDE: u32 = 4096;

/// The most pixels in one drawing: a 4096 square. Past it, a drawing is
/// shrunk (an SVG) or cut (an HTML page).
pub const MAX_PIXELS: u64 = (MAX_SIDE as u64) * (MAX_SIDE as u64);

/// The width an HTML page is laid out at when none is asked for, in CSS
/// pixels: a card's or a phone's, which reads well in a chat. Drawn at
/// twice that, for sharp text.
pub const PAGE_WIDTH: u32 = 800;

/// The narrowest and widest a page or a drawing may be asked to be.
pub const MIN_WIDTH: u32 = 64;
pub const MAX_WIDTH: u32 = 2048;

/// What the markup is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Markup {
    Svg,
    Html,
}

impl Markup {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "svg" => Some(Self::Svg),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

/// A drawing: the PNG, its size in pixels, and anything worth telling
/// whoever wrote the markup -- content cut off at an edge -- since they
/// cannot see the picture to notice.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub notes: Vec<String>,
}

/// Whether an HTML page can be drawn on this computer.
pub fn can_draw_html() -> bool {
    html::available()
}

/// Draw `source`.
///
/// `width` is in pixels: for an SVG, the picture's own (its proportions
/// kept; its own size when `None`); for a page, the width it is laid out at
/// (drawn at twice that when it fits; [`PAGE_WIDTH`] when `None`). `height`
/// matters only to a page, which is as tall as its content when `None`.
pub async fn render(
    markup: Markup,
    source: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> CommandResult<Rendered> {
    let source = source.trim();
    if source.is_empty() {
        return Err(CommandError::new(codes::INVALID, "there is nothing to draw"));
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("that drawing is over {} KB; draw something simpler", MAX_SOURCE_BYTES / 1024),
        ));
    }
    let width = width.map(|w| w.clamp(MIN_WIDTH, MAX_WIDTH));
    let height = height.map(|h| h.clamp(1, MAX_SIDE));
    match markup {
        // `width` is still the picture's width in pixels, as the caller
        // asked, so the page is laid out at half of it and drawn sharp.
        Markup::Svg if svg::needs_browser(source) && can_draw_html() => {
            let measured = source.to_string();
            let (own, _) = within(SVG_TIME, blocking(move || svg::size(&measured))).await?;
            let pixels = width.map_or(own, |w| w as f32);
            let page = (pixels / html::SHARP as f32).ceil() as u32;
            html::render(&bare_svg_page(source), page.clamp(MIN_WIDTH, MAX_WIDTH), None).await
        }
        Markup::Svg => {
            let blank = svg::needs_browser(source);
            let source = source.to_string();
            let mut drawn = within(SVG_TIME, blocking(move || svg::render(&source, width))).await?;
            if blank {
                drawn.notes.push(svg::FOREIGN_BLANK.to_string());
            }
            Ok(drawn)
        }
        Markup::Html if can_draw_html() => {
            html::render(source, width.unwrap_or(PAGE_WIDTH), height).await
        }
        Markup::Html => Err(CommandError::new(
            codes::UNSUPPORTED,
            "an HTML page cannot be drawn on this computer: it needs Chrome or Chromium \
             installed. Draw it as an SVG instead.",
        )),
    }
}

/// The longest an SVG may take to draw. [`svg`]'s reading first refuses what
/// would cost minutes; this is for whatever that does not foresee. The
/// thread drawing it is left to finish, since it cannot be stopped; the turn
/// is not left waiting on it.
pub const SVG_TIME: std::time::Duration = std::time::Duration::from_secs(20);

/// `work`, or a refusal once `limit` has passed.
async fn within<T>(
    limit: std::time::Duration,
    work: impl std::future::Future<Output = CommandResult<T>>,
) -> CommandResult<T> {
    tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
        Err(CommandError::new(
            codes::TIMED_OUT,
            format!(
                "that drawing took longer than {} seconds; draw something simpler",
                limit.as_secs()
            ),
        ))
    })
}

/// An SVG on a page of its own, filling its width, for the browser to draw.
fn bare_svg_page(source: &str) -> String {
    format!(
        "<style>html,body{{margin:0;background:#fff}}svg{{display:block;width:100%;\
         height:auto}}</style>{source}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_named_as_a_model_would_name_it() {
        assert_eq!(Markup::parse("SVG"), Some(Markup::Svg));
        assert_eq!(Markup::parse(" html "), Some(Markup::Html));
        assert_eq!(Markup::parse("png"), None);
    }

    #[tokio::test]
    async fn an_svg_is_drawn_without_a_browser() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"/>"#;
        let out = render(Markup::Svg, svg, Some(80), None).await.unwrap();
        assert_eq!((out.width, out.height), (80, 40));
        assert!(out.png.starts_with(b"\x89PNG"));
    }

    /// An SVG holding HTML goes to the browser and still comes out at the
    /// width asked in pixels; with no browser it is drawn without that part,
    /// and says so.
    #[tokio::test]
    async fn an_svg_with_html_in_it_comes_out_at_the_width_asked() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="100">
            <foreignObject width="300" height="100">
            <div xmlns="http://www.w3.org/1999/xhtml">Hello</div></foreignObject></svg>"#;
        let out = render(Markup::Svg, svg, Some(600), None).await.unwrap();
        if can_draw_html() {
            assert_eq!((out.width, out.height), (600, 200));
            assert!(out.notes.is_empty(), "{:?}", out.notes);
        } else {
            assert_eq!(out.width, 600);
            assert_eq!(out.notes, vec![svg::FOREIGN_BLANK.to_string()]);
        }
    }

    #[tokio::test]
    async fn nothing_and_too_much_are_refused() {
        let err = render(Markup::Svg, "  ", None, None).await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        let huge = format!("<svg>{}</svg>", "x".repeat(MAX_SOURCE_BYTES));
        let err = render(Markup::Svg, &huge, None, None).await.unwrap_err();
        assert_eq!(err.code, codes::TOO_LARGE);
    }
}
