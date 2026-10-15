//! Screen capture for Circle-to-Ask (monitor under cursor).

use image::{ImageBuffer, ImageEncoder, Rgba};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    /// Full-monitor PNG bytes.
    #[serde(skip)]
    pub png: Vec<u8>,
    /// data URL for the frontend overlay.
    pub data_url: String,
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub width: u32,
    pub height: u32,
    pub capture_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Capture the monitor that currently contains the mouse cursor.
pub fn capture_monitor_under_cursor() -> Result<CaptureResult, String> {
    #[cfg(target_os = "linux")]
    {
        if is_wayland() {
            return Err(
                "Screen capture on Wayland is not implemented yet (xdg-desktop-portal stub). Use an X11 session for Circle-to-Ask."
                    .into(),
            );
        }
    }

    let t0 = Instant::now();
    let (cx, cy) = cursor_position()?;
    let (mx, my, rgba) = grab_monitor_at(cx, cy)?;
    let (width, height) = rgba.dimensions();

    let png = rgba_to_png(&rgba)?;
    let capture_ms = t0.elapsed().as_millis();
    eprintln!(
        "[soul][ask] stage=capture ms={capture_ms} size={}x{} png_kb={:.1}",
        width,
        height,
        png.len() as f64 / 1024.0
    );

    let data_url = format!("data:image/png;base64,{}", base64_encode(&png));
    Ok(CaptureResult {
        png,
        data_url,
        monitor_x: mx,
        monitor_y: my,
        width,
        height,
        capture_ms,
    })
}

pub fn crop_png(full_png: &[u8], rect: &CropRect) -> Result<Vec<u8>, String> {
    let t0 = Instant::now();
    let img = image::load_from_memory(full_png)
        .map_err(|e| format!("Failed to decode screenshot: {e}"))?
        .to_rgba8();

    let (iw, ih) = img.dimensions();
    if rect.width == 0 || rect.height == 0 {
        return Err("Selection is empty.".into());
    }
    let x = rect.x.min(iw.saturating_sub(1));
    let y = rect.y.min(ih.saturating_sub(1));
    let w = rect.width.min(iw.saturating_sub(x)).max(1);
    let h = rect.height.min(ih.saturating_sub(y)).max(1);

    let cropped = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    let png = rgba_to_png(&cropped)?;
    eprintln!(
        "[soul][ask] stage=crop ms={} box={}x{}@{},{} png_kb={:.1}",
        t0.elapsed().as_millis(),
        w,
        h,
        x,
        y,
        png.len() as f64 / 1024.0
    );
    Ok(png)
}

fn rgba_to_png(img: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Result<Vec<u8>, String> {
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(buf.into_inner())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(bytes)
}

/// `(monitor x, monitor y, image)` of the monitor containing (cx, cy) — xcap on Windows.
#[cfg(not(target_os = "linux"))]
fn grab_monitor_at(cx: i32, cy: i32) -> Result<(i32, i32, image::RgbaImage), String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("Failed to list monitors: {e}"))?;
    if monitors.is_empty() {
        return Err("No monitors found for screen capture.".into());
    }

    let monitor = monitors
        .into_iter()
        .find(|m| {
            let Ok(x) = m.x() else { return false };
            let Ok(y) = m.y() else { return false };
            let Ok(w) = m.width() else { return false };
            let Ok(h) = m.height() else { return false };
            cx >= x && cy >= y && cx < x + w as i32 && cy < y + h as i32
        })
        .or_else(|| {
            // Fallback: primary / first monitor.
            xcap::Monitor::all().ok().and_then(|mut m| m.drain(..).next())
        })
        .ok_or_else(|| "Could not find a monitor for capture.".to_string())?;

    let mx = monitor.x().map_err(|e| format!("monitor.x: {e}"))?;
    let my = monitor.y().map_err(|e| format!("monitor.y: {e}"))?;
    let rgba = monitor
        .capture_image()
        .map_err(|e| format!("Screen capture failed: {e}"))?;
    Ok((mx, my, rgba))
}

/// X11: RandR monitor + `GetImage` (platform/x11_capture.rs).
#[cfg(target_os = "linux")]
fn grab_monitor_at(cx: i32, cy: i32) -> Result<(i32, i32, image::RgbaImage), String> {
    let (mon, img) = crate::platform::x11_capture::grab_monitor_at(cx, cy)?;
    Ok((mon.x, mon.y, img))
}

fn cursor_position() -> Result<(i32, i32), String> {
    use enigo::{Enigo, Mouse, Settings};
    let enigo =
        Enigo::new(&Settings::default()).map_err(|e| format!("Failed to read cursor: {e}"))?;
    enigo
        .location()
        .map_err(|e| format!("Failed to read cursor location: {e}"))
}

#[cfg(target_os = "linux")]
fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}
