//! Where the panel is: its corner, or wherever the person last dragged it,
//! remembered across runs in `panel-place.json` (per machine).
//!
//! The panel opens in the top right of the primary monitor's work area until
//! the person moves it out of the way. From then on it opens where they let
//! it go -- the window's top-left corner, in physical pixels of the desktop
//! (on a Mac, in points: see [`Screen::in_points`]) -- as long as some of the
//! window is still on a monitor there: one that has been unplugged takes the
//! panel back to its corner, rather than opening it somewhere nobody can see.
//! A place that is kept is kept inside the monitor most of the window is on:
//! moved in from an edge it was dropped over, and moved up, for as long as it
//! is that tall, when the cards grow past the bottom of the work area. Neither
//! of those is saved; the place the person chose is, and a shorter stack goes
//! back to it.
//!
//! The file is written by whichever process hosts the window -- the app, or
//! the panel's child process on Wayland (`super::child`) -- so it is read
//! once and kept, and read again when the other may have written it
//! ([`reload`]).
//!
//! The geometry is plain functions over rectangles, tested without a screen.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The file the place is kept in, in the per-machine config folder.
pub const FILE: &str = "panel-place.json";

/// Where the person put the panel: the window's top-left corner, in physical
/// pixels of the desktop -- on a Mac, in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Place {
    pub x: i32,
    pub y: i32,
    /// Moved by the person; false is "in its corner".
    pub moved: bool,
}

/// The place as last read or saved: read from its file once, then kept here,
/// since every resize of the window asks for it.
static KEPT: Mutex<Option<Place>> = Mutex::new(None);

/// Where the panel goes, as kept.
pub fn get() -> Place {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    *kept.get_or_insert_with(|| crate::machine_config::load(FILE))
}

/// Remember that the person put the panel at `place`.
pub fn save(place: Place) {
    *KEPT.lock().unwrap_or_else(|e| e.into_inner()) = Some(place);
    if let Err(e) = crate::machine_config::save(FILE, &place) {
        tracing::warn!(error = %e, "where the panel was moved to could not be saved");
    }
}

/// Put the panel back in its corner, now and from now on.
pub fn forget() {
    save(Place::default());
}

/// Read the file again the next time the place is asked for: the other
/// process that hosts the window may have written it since.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn reload() {
    KEPT.lock().unwrap_or_else(|e| e.into_inner()).take();
}

// ---- the geometry, without a screen --------------------------------------------

/// A rectangle of the desktop in physical pixels (on a Mac, points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }

    /// How much of `other` lies inside this: the area the two share.
    fn shared(&self, other: &Area) -> f64 {
        let width = self.right().min(other.right()) - self.x.max(other.x);
        let height = self.bottom().min(other.bottom()) - self.y.max(other.y);
        if width > 0.0 && height > 0.0 { width * height } else { 0.0 }
    }
}

/// A rectangle as the platform gives it: left, top, width, height.
pub type Rect = (i32, i32, u32, u32);

/// A monitor, as far as where the panel goes is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// The whole monitor: whether a saved place is still on it.
    pub bounds: Area,
    /// The monitor less its dock, top bar and taskbar: where the window is
    /// kept.
    pub work: Area,
    /// Physical pixels to the logical one; 1 for a monitor described in
    /// points.
    pub scale: f64,
}

impl Screen {
    /// A monitor as the platform describes it: its rectangle, its work area
    /// (empty where the platform cannot say) and its scale.
    pub fn new(monitor: Rect, work: Rect, scale: f64) -> Screen {
        Screen { bounds: area(monitor), work: area_of(work, monitor), scale }
    }

    /// A monitor on a Mac, where the platform gives each monitor's rectangle
    /// in its own physical pixels -- points times that monitor's scale -- so
    /// that a 2x screen beside a 1x one overlap, and no one place can be
    /// worked out across them. Points are the Mac's one space for the whole
    /// desktop, so the monitor is described in those, at a scale of 1, and
    /// the window is placed in them.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn in_points(monitor: Rect, work: Rect, scale: f64) -> Screen {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let points = |(x, y, width, height): Rect| -> Rect {
            let at = |n: f64| (n / scale).round() as i32;
            let size = |n: u32| (f64::from(n) / scale).round() as u32;
            (at(f64::from(x)), at(f64::from(y)), size(width), size(height))
        };
        Screen::new(points(monitor), points(work), 1.0)
    }
}

fn area((x, y, width, height): Rect) -> Area {
    Area { x: f64::from(x), y: f64::from(y), width: f64::from(width), height: f64::from(height) }
}

/// The work area of a monitor -- the monitor less its dock, top bar and
/// taskbar -- or, where the platform cannot say and reports an empty one, the
/// whole monitor.
pub fn area_of(work: Rect, monitor: Rect) -> Area {
    area(if work.2 > 0 && work.3 > 0 { work } else { monitor })
}

/// The top left of a window `width` logical pixels wide whose top right
/// corner is `margin` logical pixels in from the top right of `area`.
pub fn corner(area: Area, scale: f64, width: f64, margin: f64) -> (i32, i32) {
    let x = area.right() - (width + margin) * scale;
    let y = area.y + margin * scale;
    (x.round() as i32, y.round() as i32)
}

/// The monitor the panel goes on: the one most of a window `size` logical
/// pixels big put at `saved` is on, if the person moved it and any of it is
/// on one, or else the primary (the first, where the platform names none).
/// None where there is no monitor at all.
pub fn screen_for(
    saved: Place,
    screens: &[Screen],
    primary: usize,
    size: (f64, f64),
) -> Option<&Screen> {
    holding(saved, screens, size).or_else(|| screens.get(primary)).or_else(|| screens.first())
}

/// The monitor most of the window at `saved` is on, if the person moved it
/// and any of it is on one.
///
/// Judged by the whole window rather than one point of it: the window's top
/// edge is the transparent room left for the cards' shadow, above the head
/// the person dragged it by, so a panel pushed up against the top of a screen
/// has that edge just off it -- and is still on that screen, not on the one
/// above it nor on none. As a share of the window, since its size in pixels
/// depends on the scale of the monitor it would be on.
fn holding(saved: Place, screens: &[Screen], size: (f64, f64)) -> Option<&Screen> {
    if !saved.moved {
        return None;
    }
    let mut best: Option<(&Screen, f64)> = None;
    for screen in screens {
        let share = share_on(screen, saved, size);
        if share > 0.0 && best.is_none_or(|(_, most)| share > most) {
            best = Some((screen, share));
        }
    }
    best.map(|(screen, _)| screen)
}

/// How much of a window `size` logical pixels big put at `saved` would be on
/// `screen`, from 0 to 1, at that monitor's scale.
fn share_on(screen: &Screen, saved: Place, (width, height): (f64, f64)) -> f64 {
    let window = Area {
        x: f64::from(saved.x),
        y: f64::from(saved.y),
        width: width.max(1.0) * screen.scale,
        height: height.max(1.0) * screen.scale,
    };
    screen.bounds.shared(&window) / (window.width * window.height)
}

/// Where a window `size` logical pixels big goes, its top left in physical
/// pixels: where the person left it, kept inside the work area of the monitor
/// most of it is on -- moved in from the sides, and up rather than past the
/// bottom, or down from above the top -- or, if they never moved it or none
/// of it is on any monitor now, the corner, `margin` in from the top right of
/// the primary monitor's work area. None where there is no monitor at all.
pub fn position(
    saved: Place,
    screens: &[Screen],
    primary: usize,
    size: (f64, f64),
    margin: f64,
) -> Option<(i32, i32)> {
    if let Some(screen) = holding(saved, screens, size) {
        return Some(keep(saved, screen, size));
    }
    let screen = screens.get(primary).or_else(|| screens.first())?;
    Some(corner(screen.work, screen.scale, size.0, margin))
}

/// `saved`, moved as little as keeps a window `size` logical pixels big
/// inside `screen`'s work area. Where the window is bigger than the work
/// area, its top left stays in it.
fn keep(saved: Place, screen: &Screen, (width, height): (f64, f64)) -> (i32, i32) {
    let work = screen.work;
    let (width, height) = (width * screen.scale, height * screen.scale);
    let x = f64::from(saved.x).min(work.right() - width).max(work.x);
    let y = f64::from(saved.y).min(work.bottom() - height).max(work.y);
    (x.round() as i32, y.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f64 = 400.0;
    const MARGIN: f64 = 16.0;

    fn hd() -> Screen {
        Screen::new((0, 0, 1920, 1080), (0, 32, 1920, 1048), 1.0)
    }

    /// A second monitor to the right of [`hd`], at twice the scale.
    fn retina() -> Screen {
        Screen::new((1920, 0, 3840, 2160), (1920, 0, 3840, 2160), 2.0)
    }

    fn moved(x: i32, y: i32) -> Place {
        Place { x, y, moved: true }
    }

    #[test]
    fn the_place_reads_back_under_the_names_it_was_written_with() {
        let json = serde_json::to_value(moved(-40, 300)).unwrap();
        assert_eq!(json, serde_json::json!({ "x": -40, "y": 300, "moved": true }));
        let back: Place = serde_json::from_value(json).unwrap();
        assert_eq!(back, moved(-40, 300));
        let partial: Place = serde_json::from_value(serde_json::json!({ "x": 5 })).unwrap();
        assert_eq!(partial, Place { x: 5, y: 0, moved: false }, "what is missing is the corner");
    }

    #[test]
    fn the_work_area_is_used_when_the_platform_has_one() {
        let area = area_of((0, 32, 1920, 1048), (0, 0, 1920, 1080));
        assert_eq!(area, Area { x: 0.0, y: 32.0, width: 1920.0, height: 1048.0 });
    }

    #[test]
    fn a_monitor_with_no_work_area_is_used_whole() {
        let area = area_of((0, 0, 0, 0), (1920, 0, 2560, 1440));
        assert_eq!(area, Area { x: 1920.0, y: 0.0, width: 2560.0, height: 1440.0 });
        let one_side_empty = area_of((10, 10, 0, 500), (0, 0, 800, 600));
        assert_eq!(one_side_empty.width, 800.0);
    }

    #[test]
    fn the_corner_is_a_margin_in_from_the_top_right() {
        let area = area_of((0, 0, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(corner(area, 1.0, WIDTH, MARGIN), (1920 - 400 - 16, 16));
    }

    #[test]
    fn the_corner_keeps_below_a_top_bar() {
        assert_eq!(corner(hd().work, 1.0, WIDTH, MARGIN), (1504, 32 + 16));
    }

    #[test]
    fn the_corner_is_in_physical_pixels_of_the_monitor_it_is_on() {
        // 400 logical pixels wide is 800 physical ones at twice the scale.
        assert_eq!(corner(retina().work, 2.0, WIDTH, MARGIN), (1920 + 3840 - 2 * (400 + 16), 32));
        let fractional =
            corner(area_of((0, 0, 1920, 1080), (0, 0, 1920, 1080)), 1.25, WIDTH, MARGIN);
        assert_eq!(fractional, (1920 - 520, 20));
    }

    #[test]
    fn a_panel_never_moved_opens_in_the_corner_of_the_primary_monitor() {
        let screens = [retina(), hd()];
        let at = position(Place::default(), &screens, 1, (WIDTH, 300.0), MARGIN);
        assert_eq!(at, Some((1504, 48)));
        // A place written down but not moved by the person is the corner too.
        let at =
            position(Place { x: 10, y: 10, moved: false }, &screens, 1, (WIDTH, 300.0), MARGIN);
        assert_eq!(at, Some((1504, 48)));
    }

    #[test]
    fn no_primary_named_is_the_first_monitor() {
        let screens = [hd(), retina()];
        assert_eq!(
            position(Place::default(), &screens, 7, (WIDTH, 300.0), MARGIN),
            Some((1504, 48))
        );
    }

    #[test]
    fn no_monitor_at_all_is_no_place() {
        assert_eq!(position(moved(10, 10), &[], 0, (WIDTH, 300.0), MARGIN), None);
        assert!(screen_for(moved(10, 10), &[], 0, (WIDTH, 300.0)).is_none());
    }

    #[test]
    fn a_place_on_a_monitor_is_kept() {
        let screens = [hd(), retina()];
        assert_eq!(
            position(moved(200, 400), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((200, 400))
        );
        // On the second monitor, where the panel is twice as many pixels.
        assert_eq!(
            position(moved(3000, 500), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((3000, 500))
        );
    }

    #[test]
    fn a_place_on_a_monitor_that_has_gone_is_the_corner() {
        // Left on the second monitor, which has been unplugged since.
        let screens = [hd()];
        assert_eq!(
            position(moved(3000, 500), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((1504, 48))
        );
        // Above every monitor.
        assert_eq!(
            position(moved(200, -900), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((1504, 48))
        );
    }

    #[test]
    fn a_panel_dropped_over_an_edge_is_kept_inside_the_work_area() {
        let screens = [hd()];
        // Half off the right: its middle is still on the monitor.
        assert_eq!(
            position(moved(1700, 400), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((1920 - 400, 400))
        );
        // Over the left edge.
        assert_eq!(position(moved(-150, 400), &screens, 0, (WIDTH, 300.0), MARGIN), Some((0, 400)));
        // Under the top bar.
        assert_eq!(position(moved(200, 5), &screens, 0, (WIDTH, 300.0), MARGIN), Some((200, 32)));
    }

    #[test]
    fn a_panel_that_grows_past_the_bottom_moves_up() {
        let screens = [hd()];
        // 1080 is the bottom of the work area: 900 + 300 would pass it.
        assert_eq!(
            position(moved(200, 900), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((200, 1080 - 300))
        );
        // Shorter again, and it is back where it was put.
        assert_eq!(
            position(moved(200, 900), &screens, 0, (WIDTH, 120.0), MARGIN),
            Some((200, 900))
        );
    }

    #[test]
    fn moving_up_is_measured_in_the_monitors_own_pixels() {
        let screens = [hd(), retina()];
        // 300 logical pixels tall is 600 physical ones on the second monitor.
        assert_eq!(
            position(moved(3000, 1800), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((3000, 2160 - 600))
        );
    }

    #[test]
    fn a_window_taller_than_the_work_area_keeps_its_top_in_it() {
        let screens = [hd()];
        assert_eq!(
            position(moved(200, 600), &screens, 0, (WIDTH, 5000.0), MARGIN),
            Some((200, 32))
        );
    }

    #[test]
    fn the_monitor_is_the_one_most_of_the_window_is_on() {
        let screens = [hd(), retina()];
        let size = (WIDTH, 300.0);
        // The top-left is on the first monitor, but most of the window is on
        // the second: 680 of its 800 pixels there, 120 of 400 here.
        let screen = screen_for(moved(1800, 300), &screens, 0, size).unwrap();
        assert_eq!(screen.scale, 2.0);
        let screen = screen_for(moved(1500, 300), &screens, 1, size).unwrap();
        assert_eq!(screen.scale, 1.0);
        // Not moved: the primary, wherever the numbers are.
        let screen =
            screen_for(Place { x: 1500, y: 300, moved: false }, &screens, 1, size).unwrap();
        assert_eq!(screen.scale, 2.0);
    }

    #[test]
    fn a_panel_pushed_against_the_top_of_a_screen_stays_on_it() {
        // Its top edge -- the room for the shadow, above the head -- is just
        // off the screen: it is kept, moved down under the top bar.
        let screens = [hd()];
        assert_eq!(position(moved(300, -20), &screens, 0, (WIDTH, 300.0), MARGIN), Some((300, 32)));
        // With a monitor above, it is the one most of the window is on.
        let above = Screen::new((0, -1080, 1920, 1080), (0, -1080, 1920, 1080), 1.0);
        let screens = [hd(), above];
        assert_eq!(position(moved(300, -20), &screens, 0, (WIDTH, 300.0), MARGIN), Some((300, 32)));
        // And one dropped mostly on the monitor above goes there.
        assert_eq!(
            position(moved(300, -250), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((300, -300))
        );
    }

    /// A MacBook's screen, 1512 by 982 points at twice the scale, with the
    /// menu bar, and a 1x display to its right: as the platform gives them,
    /// and in points.
    fn macbook_and_display() -> [Screen; 2] {
        [
            Screen::in_points((0, 0, 3024, 1964), (0, 50, 3024, 1914), 2.0),
            Screen::in_points((1512, 0, 1920, 1080), (1512, 0, 1920, 1080), 1.0),
        ]
    }

    #[test]
    fn a_mac_monitor_is_described_in_points() {
        let [macbook, display] = macbook_and_display();
        assert_eq!(macbook.bounds, Area { x: 0.0, y: 0.0, width: 1512.0, height: 982.0 });
        assert_eq!(macbook.work, Area { x: 0.0, y: 25.0, width: 1512.0, height: 957.0 });
        assert_eq!(macbook.scale, 1.0);
        assert_eq!(display.bounds, Area { x: 1512.0, y: 0.0, width: 1920.0, height: 1080.0 });
        // In the platform's own pixels the two overlap; in points they do not.
        assert_eq!(macbook.bounds.shared(&display.bounds), 0.0);
        let odd = Screen::in_points((0, 0, 100, 100), (0, 0, 100, 100), f64::NAN);
        assert_eq!(odd.bounds.width, 100.0, "a scale that is no number is taken as 1");
    }

    #[test]
    fn on_a_mac_a_panel_left_on_the_1x_display_beside_a_2x_screen_stays_there() {
        let screens = macbook_and_display();
        assert_eq!(
            position(moved(2400, 200), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((2400, 200))
        );
        // Its corner is the MacBook's, in points, under the menu bar.
        assert_eq!(
            position(Place::default(), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((1512 - 416, 25 + 16))
        );
        // Dropped over the display's right edge, it is kept on the display.
        assert_eq!(
            position(moved(3300, 200), &screens, 0, (WIDTH, 300.0), MARGIN),
            Some((1512 + 1920 - 400, 200))
        );
    }
}
