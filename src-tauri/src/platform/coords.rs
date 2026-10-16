//! Coordinate mapping for the computer agent. Everything Soul acts on is in **physical**
//! virtual-desktop pixels (UIA rects, screen capture, cursor): Soul is per-monitor DPI aware.
//! This module maps the model's 0–1000 coordinates (relative to the screenshot region), downscaled
//! screenshot pixels, logical (DPI-scaled) points and SendInput's 0–65535 absolute space onto that
//! frame — with monitors at negative origins (left / above the primary) handled.

use super::ui::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// One monitor: physical rect and its scale factor (1.0 = 100 %, 1.25 = 125 %…).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub rect: Rect,
    pub scale: f64,
}

/// Model coordinates are 0–1000 on both axes of the screenshot.
pub const NORM_MAX: f64 = 1000.0;

/// 0–1000 (screenshot of `region`) → physical screen pixel. Clamped into the region.
pub fn norm_to_screen(nx: f64, ny: f64, region: Rect) -> Point {
    let nx = nx.clamp(0.0, NORM_MAX);
    let ny = ny.clamp(0.0, NORM_MAX);
    let x = region.x as f64 + nx / NORM_MAX * region.w as f64;
    let y = region.y as f64 + ny / NORM_MAX * region.h as f64;
    Point::new(
        (x.round() as i32).clamp(region.x, region.right() - 1),
        (y.round() as i32).clamp(region.y, region.bottom() - 1),
    )
}

/// Physical screen pixel → 0–1000 of `region`.
pub fn screen_to_norm(p: Point, region: Rect) -> (i32, i32) {
    let nx = (p.x - region.x) as f64 * NORM_MAX / region.w.max(1) as f64;
    let ny = (p.y - region.y) as f64 * NORM_MAX / region.h.max(1) as f64;
    (nx.round().clamp(0.0, NORM_MAX) as i32, ny.round().clamp(0.0, NORM_MAX) as i32)
}

/// Pixel in a downscaled screenshot (`img_w` × `img_h` showing `region`) → physical pixel.
#[cfg(test)]
pub fn image_to_screen(px: f64, py: f64, img_w: u32, img_h: u32, region: Rect) -> Point {
    norm_to_screen(px / img_w.max(1) as f64 * NORM_MAX, py / img_h.max(1) as f64 * NORM_MAX, region)
}

/// Size of a screenshot of `w` × `h` downscaled so the long side is at most `max_side`.
pub fn fit_within(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max_side || long == 0 {
        return (w, h);
    }
    let f = max_side as f64 / long as f64;
    (((w as f64 * f).round() as u32).max(1), ((h as f64 * f).round() as u32).max(1))
}

/// Monitor containing `p` (physical).
pub fn monitor_at(p: Point, monitors: &[Monitor]) -> Option<&Monitor> {
    monitors.iter().find(|m| m.rect.contains(p.x, p.y))
}

/// Logical (DIP) point → physical, Tauri's model: each monitor keeps its physical origin and
/// scales distances inside it. `monitor` = the monitor the point is on.
#[cfg_attr(not(test), allow(dead_code))] // for logical-unit callers (DPI-unaware windows)
pub fn logical_to_physical(lx: f64, ly: f64, monitor: &Monitor) -> Point {
    let ox = monitor.rect.x as f64;
    let oy = monitor.rect.y as f64;
    let s = if monitor.scale > 0.0 { monitor.scale } else { 1.0 };
    Point::new((ox + (lx - ox / s) * s).round() as i32, (oy + (ly - oy / s) * s).round() as i32)
}

/// Physical point → logical (inverse of [`logical_to_physical`]).
#[cfg_attr(not(test), allow(dead_code))]
pub fn physical_to_logical(p: Point, monitor: &Monitor) -> (f64, f64) {
    let s = if monitor.scale > 0.0 { monitor.scale } else { 1.0 };
    let ox = monitor.rect.x as f64;
    let oy = monitor.rect.y as f64;
    (ox / s + (p.x as f64 - ox) / s, oy / s + (p.y as f64 - oy) / s)
}

/// Bounding box of all monitors (Windows "virtual screen").
#[cfg_attr(not(windows), allow(dead_code))] // SendInput mapping (Windows backend)
pub fn virtual_bounds(monitors: &[Monitor]) -> Rect {
    let Some(first) = monitors.first() else {
        return Rect::default();
    };
    let (mut l, mut t, mut r, mut b) = (first.rect.x, first.rect.y, first.rect.right(), first.rect.bottom());
    for m in monitors {
        l = l.min(m.rect.x);
        t = t.min(m.rect.y);
        r = r.max(m.rect.right());
        b = b.max(m.rect.bottom());
    }
    Rect::new(l, t, r - l, b - t)
}

/// Physical pixel → SendInput absolute coordinates (MOUSEEVENTF_ABSOLUTE | VIRTUALDESK):
/// 0–65535 across the virtual screen.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn to_absolute(p: Point, virtual_screen: Rect) -> (i32, i32) {
    let w = (virtual_screen.w - 1).max(1) as f64;
    let h = (virtual_screen.h - 1).max(1) as f64;
    let ax = ((p.x - virtual_screen.x) as f64 * 65535.0 / w).round();
    let ay = ((p.y - virtual_screen.y) as f64 * 65535.0 / h).round();
    (ax.clamp(0.0, 65535.0) as i32, ay.clamp(0.0, 65535.0) as i32)
}

/// SendInput absolute → physical pixel (inverse of [`to_absolute`]).
#[cfg(test)]
pub fn from_absolute(ax: i32, ay: i32, virtual_screen: Rect) -> Point {
    let w = (virtual_screen.w - 1).max(1) as f64;
    let h = (virtual_screen.h - 1).max(1) as f64;
    Point::new(
        virtual_screen.x + (ax as f64 * w / 65535.0).round() as i32,
        virtual_screen.y + (ay as f64 * h / 65535.0).round() as i32,
    )
}

/// Kill switch: the cursor is jammed into a "hard" screen corner — a monitor corner the cursor
/// can't move past (no neighbouring monitor there), within `margin` px.
pub fn in_hard_corner(p: Point, monitors: &[Monitor], margin: i32) -> bool {
    let outside = |x: i32, y: i32| !monitors.iter().any(|m| m.rect.contains(x, y));
    monitors.iter().any(|m| {
        let r = m.rect;
        let corners = [
            (r.x, r.y, -1, -1),
            (r.right() - 1, r.y, 1, -1),
            (r.x, r.bottom() - 1, -1, 1),
            (r.right() - 1, r.bottom() - 1, 1, 1),
        ];
        corners.iter().any(|&(cx, cy, dx, dy)| {
            (p.x - cx).abs() <= margin
                && (p.y - cy).abs() <= margin
                && m.rect.contains(p.x, p.y)
                // Hard only if nothing continues past it: diagonally or along either edge
                // (the cursor would slide onto a neighbouring monitor instead of stopping).
                && outside(cx + dx * 4, cy + dy * 4)
                && outside(cx + dx * 4, cy)
                && outside(cx, cy + dy * 4)
        })
    })
}

/// Keep a synthetic click away from monitor corners (would trip the kill switch / hot corners).
pub fn clamp_off_corners(p: Point, monitors: &[Monitor], margin: i32) -> Point {
    if !in_hard_corner(p, monitors, margin) {
        return p;
    }
    let Some(m) = monitor_at(p, monitors) else {
        return p;
    };
    let r = m.rect;
    let inset = margin + 3;
    Point::new(
        p.x.clamp(r.x + inset, r.right() - 1 - inset),
        p.y.clamp(r.y + inset, r.bottom() - 1 - inset),
    )
}

/// Monitor overlapping `rect` the most (window → the monitor to capture).
pub fn monitor_for_rect<'a>(rect: &Rect, monitors: &'a [Monitor]) -> Option<&'a Monitor> {
    monitors
        .iter()
        .filter_map(|m| m.rect.intersect(rect).map(|i| (i.area(), m)))
        .max_by_key(|(a, _)| *a)
        .map(|(_, m)| m)
        .or_else(|| {
            let (cx, cy) = rect.center();
            monitor_at(Point::new(cx, cy), monitors)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, y: i32, w: i32, h: i32, scale: f64) -> Monitor {
        Monitor {
            rect: Rect::new(x, y, w, h),
            scale,
        }
    }

    /// Left: 1920×1080 @ 100 % at (-1920, 0); primary: 2560×1440 @ 125 % at (0, 0);
    /// right: 3840×2160 @ 150 % at (2560, -360).
    fn layout() -> Vec<Monitor> {
        vec![mon(-1920, 0, 1920, 1080, 1.0), mon(0, 0, 2560, 1440, 1.25), mon(2560, -360, 3840, 2160, 1.5)]
    }

    #[test]
    fn norm_roundtrip_on_each_monitor() {
        for m in layout() {
            // A window filling the left half of the monitor.
            let region = Rect::new(m.rect.x, m.rect.y, m.rect.w / 2, m.rect.h);
            let p = norm_to_screen(500.0, 500.0, region);
            assert_eq!(p, Point::new(m.rect.x + m.rect.w / 4, m.rect.y + m.rect.h / 2));
            assert_eq!(screen_to_norm(p, region), (500, 500));
            assert_eq!(norm_to_screen(0.0, 0.0, region), Point::new(region.x, region.y));
            // 1000 is the last pixel inside the region, never one past it.
            assert_eq!(norm_to_screen(1000.0, 1000.0, region), Point::new(region.right() - 1, region.bottom() - 1));
            // Out-of-range model output is clamped, not trusted.
            assert_eq!(norm_to_screen(-50.0, 4000.0, region), Point::new(region.x, region.bottom() - 1));
        }
    }

    #[test]
    fn downscaled_screenshot_pixels_map_back() {
        // 2560×1440 window shown as a 1280×720 screenshot.
        let region = Rect::new(0, 0, 2560, 1440);
        assert_eq!(fit_within(2560, 1440, 1280), (1280, 720));
        assert_eq!(fit_within(800, 600, 1280), (800, 600));
        assert_eq!(fit_within(1440, 2560, 1280), (720, 1280));
        assert_eq!(image_to_screen(640.0, 360.0, 1280, 720, region), Point::new(1280, 720));
        // Window on the 150 % monitor at a negative y.
        let region = Rect::new(2600, -300, 3000, 2000);
        let (w, h) = fit_within(3000, 2000, 1280);
        assert_eq!((w, h), (1280, 853));
        let p = image_to_screen(w as f64 / 2.0, h as f64 / 2.0, w, h, region);
        assert_eq!(p, Point::new(4100, 700));
    }

    #[test]
    fn dpi_100_125_150_logical_physical() {
        let m = layout();
        // 100 %: identical.
        assert_eq!(logical_to_physical(-1000.0, 500.0, &m[0]), Point::new(-1000, 500));
        // 125 %: logical 1000,400 on the primary → physical 1250,500.
        assert_eq!(logical_to_physical(1000.0, 400.0, &m[1]), Point::new(1250, 500));
        assert_eq!(physical_to_logical(Point::new(1250, 500), &m[1]), (1000.0, 400.0));
        // 150 %: the origin stays physical (2560,-360); 100 logical px right of it = 150 physical.
        let origin_l = physical_to_logical(Point::new(2560, -360), &m[2]);
        let p = logical_to_physical(origin_l.0 + 100.0, origin_l.1 + 100.0, &m[2]);
        assert_eq!(p, Point::new(2710, -210));
        for mm in &m {
            let probe = Point::new(mm.rect.x + 333, mm.rect.y + 777);
            let (lx, ly) = physical_to_logical(probe, mm);
            assert_eq!(logical_to_physical(lx, ly, mm), probe, "roundtrip at scale {}", mm.scale);
        }
    }

    #[test]
    fn sendinput_absolute_covers_virtual_screen() {
        let m = layout();
        let vs = virtual_bounds(&m);
        assert_eq!(vs, Rect::new(-1920, -360, 1920 + 2560 + 3840, 360 + 1800));
        assert_eq!(to_absolute(Point::new(vs.x, vs.y), vs), (0, 0));
        assert_eq!(to_absolute(Point::new(vs.right() - 1, vs.bottom() - 1), vs), (65535, 65535));
        for p in [Point::new(-1920, 0), Point::new(0, 0), Point::new(1280, 720), Point::new(6399, 1799), Point::new(4000, -100)] {
            let (ax, ay) = to_absolute(p, vs);
            let back = from_absolute(ax, ay, vs);
            assert!((back.x - p.x).abs() <= 1 && (back.y - p.y).abs() <= 1, "{p:?} → {back:?}");
        }
    }

    #[test]
    fn hard_corners_only_where_the_cursor_stops() {
        let m = layout();
        // Top-left of the left monitor: hard.
        assert!(in_hard_corner(Point::new(-1920, 0), &m, 3));
        assert!(in_hard_corner(Point::new(-1918, 2), &m, 3));
        // Bottom-left of the left monitor: hard.
        assert!(in_hard_corner(Point::new(-1920, 1079), &m, 3));
        // Top-left of the primary touches the left monitor's top edge: the cursor can pass → not hard.
        assert!(!in_hard_corner(Point::new(0, 0), &m, 3));
        // Top-right of the primary: right monitor continues there → not hard.
        assert!(!in_hard_corner(Point::new(2559, 0), &m, 3));
        // Bottom-right of the right monitor: hard.
        assert!(in_hard_corner(Point::new(6399, 1799), &m, 3));
        // Middle of the screen: no.
        assert!(!in_hard_corner(Point::new(1280, 720), &m, 3));
        // A click aimed at a hard corner is pulled inside.
        let p = clamp_off_corners(Point::new(-1920, 0), &m, 3);
        assert!(!in_hard_corner(p, &m, 3) && m[0].rect.contains(p.x, p.y));
        // Single monitor: all four corners are hard.
        let single = [mon(0, 0, 1920, 1080, 1.0)];
        for c in [Point::new(0, 0), Point::new(1919, 0), Point::new(0, 1079), Point::new(1919, 1079)] {
            assert!(in_hard_corner(c, &single, 3), "{c:?}");
        }
    }

    #[test]
    fn window_spanning_monitors_picks_largest_overlap() {
        let m = layout();
        let w = Rect::new(-500, 100, 2000, 800); // mostly on the primary
        assert_eq!(monitor_for_rect(&w, &m).unwrap().rect.x, 0);
        let w = Rect::new(-1500, 100, 1600, 800); // mostly on the left one
        assert_eq!(monitor_for_rect(&w, &m).unwrap().rect.x, -1920);
    }
}
