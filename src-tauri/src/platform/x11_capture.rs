//! X11 screen grab for Circle-to-Ask (x11rb: RandR monitors + core `GetImage` on the root window).
//!
//! Replaces xcap on Linux: xcap 0.9 always links PipeWire (libspa 0.10), whose bindings need
//! PipeWire ≥ 0.3.5x headers — newer than Ubuntu 22.04, the oldest distro Soul builds on. Wayland
//! capture stays a clear error (see `capture::capture_monitor_under_cursor`).
use image::RgbaImage;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder};

/// One monitor in root-window coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl MonitorRect {
    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.width as i32 && py < self.y + self.height as i32
    }
}

/// Monitor under (cx, cy), else the primary one, else the first.
pub fn pick_monitor(monitors: &[MonitorRect], cx: i32, cy: i32) -> Option<MonitorRect> {
    monitors
        .iter()
        .find(|m| m.contains(cx, cy))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
        .copied()
}

/// 32-bpp ZPixmap scanlines → RGBA (opaque). `lsb_first` = server image byte order LSBFirst
/// (B, G, R, X in memory); MSBFirst is X, R, G, B.
pub fn zpixmap32_to_rgba(data: &[u8], width: u32, height: u32, stride: usize, lsb_first: bool) -> Option<RgbaImage> {
    let (w, h) = (width as usize, height as usize);
    if stride < w * 4 || data.len() < stride * h.saturating_sub(1) + w * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let line = &data[row * stride..row * stride + w * 4];
        for px in line.chunks_exact(4) {
            let (r, g, b) = if lsb_first { (px[2], px[1], px[0]) } else { (px[1], px[2], px[3]) };
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    RgbaImage::from_raw(width, height, out)
}

/// Grab the monitor under the cursor: `(monitor, image)`.
pub fn grab_monitor_at(cx: i32, cy: i32) -> Result<(MonitorRect, RgbaImage), String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("X11 connection failed: {e}"))?;
    let setup = conn.setup();
    let screen = setup
        .roots
        .get(screen_num)
        .ok_or_else(|| "X11 screen not found".to_string())?;
    let root = screen.root;

    // RandR 1.5 monitors; without it, the whole root window is one monitor.
    let mut monitors: Vec<MonitorRect> = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| {
            r.monitors
                .iter()
                .filter(|m| m.width > 0 && m.height > 0)
                .map(|m| MonitorRect {
                    x: m.x as i32,
                    y: m.y as i32,
                    width: m.width as u32,
                    height: m.height as u32,
                    primary: m.primary,
                })
                .collect()
        })
        .unwrap_or_default();
    if monitors.is_empty() {
        monitors.push(MonitorRect {
            x: 0,
            y: 0,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
            primary: true,
        });
    }
    let mon = pick_monitor(&monitors, cx, cy).ok_or_else(|| "No monitors found for screen capture.".to_string())?;

    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            mon.x as i16,
            mon.y as i16,
            mon.width as u16,
            mon.height as u16,
            !0,
        )
        .map_err(|e| format!("Screen capture failed: {e}"))?
        .reply()
        .map_err(|e| format!("Screen capture failed: {e}"))?;

    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == reply.depth)
        .ok_or_else(|| format!("Unsupported X11 visual depth {}", reply.depth))?;
    if format.bits_per_pixel != 32 {
        return Err(format!(
            "Unsupported X11 pixel format ({} bpp at depth {}); Circle-to-Ask needs a 24/32-bit display",
            format.bits_per_pixel, reply.depth
        ));
    }
    let pad = (format.scanline_pad as usize / 8).max(1);
    let stride = (mon.width as usize * 4).div_ceil(pad) * pad;
    let lsb_first = setup.image_byte_order == ImageOrder::LSB_FIRST;
    let img = zpixmap32_to_rgba(&reply.data, mon.width, mon.height, stride, lsb_first)
        .ok_or_else(|| "Screen capture returned a truncated image".to_string())?;
    Ok((mon, img))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: u32, h: u32, primary: bool) -> MonitorRect {
        MonitorRect { x, y, width: w, height: h, primary }
    }

    #[test]
    fn picks_monitor_under_cursor_then_primary() {
        let mons = [rect(0, 0, 1920, 1080, false), rect(1920, 0, 2560, 1440, true)];
        assert_eq!(pick_monitor(&mons, 100, 100), Some(mons[0]));
        assert_eq!(pick_monitor(&mons, 2000, 1300), Some(mons[1]));
        assert_eq!(pick_monitor(&mons, -50, 5000), Some(mons[1]));
        assert_eq!(pick_monitor(&[], 0, 0), None);
    }

    #[test]
    fn converts_bgrx_and_xrgb_with_padding() {
        // 2x2, stride 12 (4 bytes of padding per row).
        let lsb = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, 7, 8, 9, 0, 10, 11, 12, 0, 9, 9, 9, 9];
        let img = zpixmap32_to_rgba(&lsb, 2, 2, 12, true).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [3, 2, 1, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [12, 11, 10, 255]);
        let msb = [0, 1, 2, 3];
        assert_eq!(zpixmap32_to_rgba(&msb, 1, 1, 4, false).unwrap().get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert!(zpixmap32_to_rgba(&lsb[..10], 2, 2, 12, true).is_none());
    }
}
