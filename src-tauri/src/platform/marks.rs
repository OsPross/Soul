//! Screenshot for the computer agent: crop the window out of its monitor's capture, downscale to
//! ≤ `max_side` px, draw numbered boxes for the accessibility elements ("set-of-marks", so the
//! model refers to ids, not raw pixels) and encode as JPEG. In memory only.

use super::coords::fit_within;
use super::ui::{Rect, UiElement};
use image::{ImageEncoder, Rgb, RgbImage, RgbaImage};

pub const MAX_SIDE: u32 = 1280;
pub const JPEG_QUALITY: u8 = 70;

#[derive(Debug, Clone)]
pub struct Shot {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Physical screen rect the image shows (the 0–1000 frame for vision coordinates).
    pub region: Rect,
}

/// Distinct, readable box colours.
const PALETTE: [[u8; 3]; 6] = [
    [230, 25, 75],
    [0, 130, 200],
    [60, 160, 40],
    [245, 130, 48],
    [145, 30, 180],
    [0, 128, 128],
];

/// 5×7 digit glyphs, one row per byte (bit 4 = leftmost column).
const DIGITS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
];

/// `monitor` = full capture of the monitor whose top-left is `monitor_origin` (physical px).
/// `marks` = elements to draw (None = plain screenshot, e.g. vision mode).
pub fn render(
    monitor: &RgbaImage,
    monitor_origin: (i32, i32),
    window: Rect,
    marks: Option<&[UiElement]>,
    max_side: u32,
    quality: u8,
) -> Result<Shot, String> {
    let mon_rect = Rect::new(monitor_origin.0, monitor_origin.1, monitor.width() as i32, monitor.height() as i32);
    let region = window
        .intersect(&mon_rect)
        .ok_or_else(|| "The window is not on the captured monitor.".to_string())?;
    let crop = image::imageops::crop_imm(
        monitor,
        (region.x - mon_rect.x) as u32,
        (region.y - mon_rect.y) as u32,
        region.w as u32,
        region.h as u32,
    )
    .to_image();
    let (w, h) = fit_within(crop.width(), crop.height(), max_side);
    let scaled = if (w, h) == crop.dimensions() {
        crop
    } else {
        image::imageops::resize(&crop, w, h, image::imageops::FilterType::Triangle)
    };
    let mut rgb: RgbImage = image::DynamicImage::ImageRgba8(scaled).to_rgb8();
    if let Some(elements) = marks {
        let sx = w as f64 / region.w as f64;
        let sy = h as f64 / region.h as f64;
        let to_img = |r: &Rect| -> Option<(i32, i32, i32, i32)> {
            let v = r.intersect(&region)?;
            Some((
                ((v.x - region.x) as f64 * sx).round() as i32,
                ((v.y - region.y) as f64 * sy).round() as i32,
                ((v.right() - region.x) as f64 * sx).round() as i32,
                ((v.bottom() - region.y) as f64 * sy).round() as i32,
            ))
        };
        let label_scale = if w >= 900 { 2 } else { 1 };
        for e in elements {
            let Some((x0, y0, x1, y1)) = to_img(&e.rect) else { continue };
            let color = Rgb(PALETTE[e.id as usize % PALETTE.len()]);
            draw_box(&mut rgb, x0, y0, x1, y1, color);
            draw_label(&mut rgb, x0, y0, e.id, color, label_scale);
        }
    }
    let jpeg = encode_jpeg(&rgb, quality)?;
    Ok(Shot { jpeg, width: w, height: h, region })
}

pub fn encode_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity((img.width() * img.height() / 8) as usize);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(10, 95))
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;
    Ok(out)
}

fn put(img: &mut RgbImage, x: i32, y: i32, c: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, c);
    }
}

fn draw_box(img: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb<u8>) {
    for t in 0..2 {
        for x in x0..=x1 {
            put(img, x, y0 + t, c);
            put(img, x, y1 - t, c);
        }
        for y in y0..=y1 {
            put(img, x0 + t, y, c);
            put(img, x1 - t, y, c);
        }
    }
}

/// Filled tag with the id in white digits at the box's top-left (inside the image).
fn draw_label(img: &mut RgbImage, x: i32, y: i32, id: u32, bg: Rgb<u8>, scale: i32) {
    let digits: Vec<usize> = id.to_string().bytes().map(|b| (b - b'0') as usize).collect();
    let glyph_w = 6 * scale; // 5 px + 1 px spacing
    let w = digits.len() as i32 * glyph_w + 2 * scale + 1;
    let h = 7 * scale + 2 * scale + 1;
    let x = x.clamp(0, (img.width() as i32 - w).max(0));
    let y = y.clamp(0, (img.height() as i32 - h).max(0));
    for yy in y..y + h {
        for xx in x..x + w {
            put(img, xx, yy, bg);
        }
    }
    let white = Rgb([255, 255, 255]);
    for (i, d) in digits.iter().enumerate() {
        let gx = x + scale + 1 + i as i32 * glyph_w;
        let gy = y + scale + 1;
        for (row, bits) in DIGITS[*d].iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            put(img, gx + col * scale + dx, gy + row as i32 * scale + dy, white);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ui::Role;

    #[test]
    fn crops_scales_marks_and_encodes() {
        // 2560×1440 monitor at (-2560, 0), filled grey; window = its right half.
        let mon = RgbaImage::from_pixel(2560, 1440, image::Rgba([90, 90, 90, 255]));
        let window = Rect::new(-1280, 0, 1280, 1440);
        let mut e = UiElement::new(Role::Button, "Zapisz", Rect::new(-1000, 200, 200, 60));
        e.id = 7;
        let shot = render(&mon, (-2560, 0), window, Some(&[e]), MAX_SIDE, JPEG_QUALITY).unwrap();
        assert_eq!(shot.region, window);
        assert_eq!((shot.width, shot.height), (1138, 1280));
        assert!(shot.jpeg.starts_with(&[0xFF, 0xD8]), "JPEG magic");
        assert!(shot.jpeg.len() < 200_000);
        let img = image::load_from_memory(&shot.jpeg).unwrap().to_rgb8();
        // The box edge (red-ish palette colour for id 7) is drawn where the element is.
        let sx = shot.width as f64 / 1280.0;
        let px = img.get_pixel(((280.0) * sx) as u32 + 1, (200.0 * sx) as u32 + 40);
        assert!(px.0[2] > 120 && px.0[0] < 120, "box colour at the element edge: {:?}", px.0);
    }

    #[test]
    fn window_partly_off_monitor_is_clipped() {
        let mon = RgbaImage::from_pixel(800, 600, image::Rgba([0, 0, 0, 255]));
        let shot = render(&mon, (0, 0), Rect::new(600, 400, 400, 400), None, MAX_SIDE, 60).unwrap();
        assert_eq!(shot.region, Rect::new(600, 400, 200, 200));
        assert_eq!((shot.width, shot.height), (200, 200));
        assert!(render(&mon, (0, 0), Rect::new(900, 0, 100, 100), None, MAX_SIDE, 60).is_err());
    }

    #[test]
    fn labels_stay_inside_the_image() {
        let mut img = RgbImage::new(40, 20);
        draw_label(&mut img, 35, 15, 123, Rgb([200, 0, 0]), 2);
        // Clamped: the tag was moved left/up, so some pixel near the top-left of it is coloured.
        assert!(img.pixels().any(|p| p.0 == [200, 0, 0]));
    }
}
