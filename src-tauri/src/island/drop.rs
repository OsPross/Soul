//! Files dropped on the Island: copied into `<app data>/inbox` (kept 7 days) and asked about.
//!
//! Windows: the Island window has `dragDropEnabled: false`, so WebView2 accepts drops from
//! Explorer like Edge does (wry's own drop target is shadowed by the WebView2 child window);
//! the page posts the `FileList` back with `postMessageWithAdditionalObjects` and the real paths
//! come from `ICoreWebView2File::Path`. Linux: Tauri's `DragDrop` window event.
//! Only paths the OS delivered by a drop (`allow_dropped`) are ever read.

use crate::config::load_settings;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use parking_lot::Mutex;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, EventTarget, Manager};

const MAX_FILE: u64 = 20 * 1024 * 1024;
const MAX_PREVIEW: u64 = 4 * 1024 * 1024;
const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const DROP_VALID: Duration = Duration::from_secs(120);
const MAX_DROPPED: usize = 64;

static DROPPED: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
/// File card on the Island: a Smart talk-key transcript is a question about this file.
static ACTIVE: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    pub name: String,
    pub path: String,
    pub size: u64,
    /// "image" | "pdf" | "text"
    pub kind: String,
    /// `data:image/...;base64,...` for images up to 4 MB.
    pub preview_data_url: Option<String>,
}

pub fn allow_dropped(paths: impl IntoIterator<Item = String>) {
    let mut list = DROPPED.lock();
    list.retain(|(_, at)| at.elapsed() < DROP_VALID);
    list.extend(paths.into_iter().map(|p| (p, Instant::now())));
    let over = list.len().saturating_sub(MAX_DROPPED);
    list.drain(..over);
}

/// True once per dropped path (and only within `DROP_VALID`).
pub fn take_dropped(path: &str) -> bool {
    let mut list = DROPPED.lock();
    list.retain(|(_, at)| at.elapsed() < DROP_VALID);
    match list.iter().position(|(p, _)| p == path) {
        Some(i) => {
            list.remove(i);
            true
        }
        None => false,
    }
}

pub fn inbox_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("inbox"))
}

/// `file` canonicalized, when it is a regular file directly inside `inbox`.
pub fn is_inside(inbox: &Path, file: &Path) -> Option<PathBuf> {
    let inbox = inbox.canonicalize().ok()?;
    let file = file.canonicalize().ok()?;
    (file.parent() == Some(inbox.as_path()) && fs::symlink_metadata(&file).ok()?.is_file()).then_some(file)
}

/// Copy a dropped file into `inbox` (never overwriting), then sweep old inbox files.
pub fn copy_into(inbox: &Path, src: &Path) -> Result<DroppedFile, String> {
    let meta = fs::metadata(src).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("Foldery nie są obsługiwane — upuść plik.".into());
    }
    if meta.len() > MAX_FILE {
        return Err("Plik jest za duży (maks. 20 MB).".into());
    }
    let name = src.file_name().and_then(|n| n.to_str()).ok_or("Nieprawidłowa nazwa pliku.")?;
    let mime = crate::ai::gemini::file_mime(name);
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(src).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
    if mime.is_none() && std::str::from_utf8(&bytes).is_err() {
        return Err("Ten typ pliku nie jest obsługiwany (obrazy, PDF, tekst, kod).".into());
    }
    fs::create_dir_all(inbox).map_err(|e| e.to_string())?;
    sweep(inbox);
    let (stem, ext) = match (Path::new(name).file_stem().and_then(|s| s.to_str()), Path::new(name).extension()) {
        (Some(stem), Some(ext)) => (stem.to_string(), format!(".{}", ext.to_string_lossy())),
        _ => (name.to_string(), String::new()),
    };
    let (dest, mut out) = (1..1000)
        .find_map(|n| {
            let file = if n == 1 { name.to_string() } else { format!("{stem} ({n}){ext}") };
            let dest = inbox.join(file);
            fs::OpenOptions::new().write(true).create_new(true).open(&dest).ok().map(|f| (dest, f))
        })
        .ok_or("Nie można zapisać pliku w skrzynce Soul.")?;
    out.write_all(&bytes).map_err(|e| e.to_string())?;
    // A copied old file keeps its mtime on some systems: the sweep must not take it right away.
    let _ = out.set_modified(SystemTime::now());
    let kind = match mime {
        Some("application/pdf") => "pdf",
        Some(_) => "image",
        None => "text",
    };
    let preview_data_url = (kind == "image" && meta.len() <= MAX_PREVIEW)
        .then(|| format!("data:{};base64,{}", mime.unwrap_or_default(), BASE64.encode(&bytes)));
    Ok(DroppedFile {
        name: dest.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        path: dest.to_string_lossy().into_owned(),
        size: bytes.len() as u64,
        kind: kind.into(),
        preview_data_url,
    })
}

fn sweep(inbox: &Path) {
    let Ok(entries) = fs::read_dir(inbox) else { return };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Copy a path the OS just delivered by a drop into the inbox.
pub fn ingest(app: &AppHandle, path: &str) -> Result<DroppedFile, String> {
    if !take_dropped(path) {
        return Err("Plik nie pochodzi z upuszczenia.".into());
    }
    copy_into(&inbox_dir(app)?, Path::new(path))
}

/// First dropped path → inbox → `island://file-drop` (off the UI thread).
fn on_dropped(app: &AppHandle, paths: Vec<String>) {
    allow_dropped(paths.iter().cloned());
    let Some(first) = paths.into_iter().next() else { return };
    let app = app.clone();
    thread::spawn(move || match ingest(&app, &first) {
        Ok(dropped) => {
            eprintln!("[soul][island] file dropped kind={} size={}", dropped.kind, dropped.size);
            let _ = app.emit_to(EventTarget::webview_window(super::ISLAND_LABEL), "island://file-drop", dropped);
        }
        Err(e) => crate::report_error(&app, e),
    });
}

#[cfg(windows)]
pub fn install(app: &AppHandle) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2File, ICoreWebView2WebMessageReceivedEventArgs2,
    };
    use webview2_com::{take_pwstr, WebMessageReceivedEventHandler};
    use wv2_core::{Interface, PWSTR};

    let Some(win) = app.get_webview_window(super::ISLAND_LABEL) else { return };
    let handle = app.clone();
    // SAFETY: WebView2 COM calls on the webview's own (UI) thread; out-params are initialized.
    let _ = win.with_webview(move |webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else { return };
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let Ok(args) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>() else { return Ok(()) };
            let Ok(objects) = args.AdditionalObjects() else { return Ok(()) };
            let mut count = 0u32;
            objects.Count(&mut count)?;
            let mut paths = Vec::new();
            for i in 0..count {
                let Ok(file) = objects.GetValueAtIndex(i)?.cast::<ICoreWebView2File>() else { continue };
                let mut path = PWSTR::null();
                file.Path(&mut path)?;
                paths.push(take_pwstr(path));
            }
            on_dropped(&handle, paths);
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WebMessageReceived(&handler, &mut token);
    });
}

#[cfg(not(windows))]
pub fn install(app: &AppHandle) {
    let Some(win) = app.get_webview_window(super::ISLAND_LABEL) else { return };
    let handle = app.clone();
    win.on_window_event(move |e| {
        if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = e {
            on_dropped(&handle, paths.iter().map(|p| p.to_string_lossy().into_owned()).collect());
        }
    });
}

pub fn active_file() -> Option<String> {
    ACTIVE.lock().clone()
}

/// The Island shows (Some) or dismissed (None) the dropped-file card.
#[tauri::command]
pub fn island_file_active(path: Option<String>) {
    *ACTIVE.lock() = path;
}

/// Typed question about the dropped file; the answer arrives as `island://text-result`.
#[tauri::command]
pub async fn island_ask_file(app: AppHandle, path: String, question: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ask_file(&app, &path, &question, None))
        .await
        .map_err(|e| e.to_string())?
}

/// Blocking: Gemini (Ask model, then the fallback) reads the file and answers on the Island.
pub fn ask_file(app: &AppHandle, path: &str, question: &str, window: Option<u64>) -> Result<(), String> {
    let file = is_inside(&inbox_dir(app)?, Path::new(path)).ok_or("Plik nie jest już dostępny — upuść go ponownie.")?;
    let settings = load_settings(app)?;
    if settings.api_key.trim().is_empty() {
        return Err("No API key set. Open Settings (tray → Settings) and paste your Gemini API key.".into());
    }
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let bytes = fs::read(&file).map_err(|e| e.to_string())?;
    let body = crate::ai::gemini::build_file_ask_body(&name, &bytes, question, &settings.language)?;
    let t0 = Instant::now();
    let client = crate::ai::gemini::GeminiClient::new(&settings.api_key, settings.effective_ask_model());
    let models = crate::ai::gemini::models_cached_or_fetch(&client);
    let fallback = crate::resolve_fallback_model(&settings, &models);
    let retire = app.clone();
    let value = client.agent_generate_resilient(body, Some(&fallback), |_, _| {}, move |m| {
        crate::retire_model_and_heal(&retire, m)
    })?;
    let answer = crate::ai::gemini::extract_text_from_value(&value)?;
    eprintln!("[soul][island] file answer ms={} chars={}", t0.elapsed().as_millis(), answer.chars().count());
    let id = crate::agent::text_action::next_result_id();
    crate::agent::text_action::show_result(app, id, name, answer, window);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_taken_once() {
        allow_dropped(["C:/a.txt".to_string(), "C:/b.png".to_string()]);
        assert!(take_dropped("C:/a.txt"));
        assert!(!take_dropped("C:/a.txt"));
        assert!(!take_dropped("C:/never-dropped.txt"));
        assert!(take_dropped("C:/b.png"));
        allow_dropped((0..100).map(|i| format!("p{i}")));
        assert!(!take_dropped("p0"), "only the newest MAX_DROPPED are kept");
        assert!(take_dropped("p99"));
    }

    #[test]
    fn copies_into_inbox_without_overwriting() {
        let root = std::env::temp_dir().join(format!("soul-drop-test-{}", std::process::id()));
        let (src_dir, inbox) = (root.join("src"), root.join("inbox"));
        fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("notatka.md");
        fs::write(&src, "zażółć").unwrap();
        let a = copy_into(&inbox, &src).unwrap();
        let b = copy_into(&inbox, &src).unwrap();
        assert_eq!((a.name.as_str(), a.kind.as_str()), ("notatka.md", "text"));
        assert_eq!(b.name, "notatka (2).md");
        assert!(is_inside(&inbox, Path::new(&b.path)).is_some());
        assert!(is_inside(&inbox, &src).is_none(), "outside the inbox");
        assert!(is_inside(&inbox, &inbox.join("..").join("src").join("notatka.md")).is_none());
        assert!(copy_into(&inbox, &src_dir).is_err(), "directories are rejected");
        fs::write(src_dir.join("x.bin"), [0xff, 0xfe, 0]).unwrap();
        assert!(copy_into(&inbox, &src_dir.join("x.bin")).is_err(), "binary non-image");
        let _ = fs::remove_dir_all(&root);
    }
}
