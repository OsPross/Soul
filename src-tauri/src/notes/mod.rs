//! Call notetaker: mic + system loopback recorded to WAV, live transcript (Parakeet), then a
//! speaker-attributed transcript (diarization + echo removal) and a Gemini summary with to-dos.
//! Notes live in `<app data>/notes/<id>/note.json`; the WAVs are deleted once transcribed.

pub mod builder;

use crate::config::load_settings;
use crate::i18n::{tr, ui_lang};
use crate::speech::{self, audio};
use builder::{Line, Note, Speaker, Status};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const RATE: usize = audio::SAMPLE_RATE as usize;

/// One recorded channel: the WAV on disk + the samples not yet transcribed live.
#[derive(Default)]
struct Channel {
    wav: Option<audio::WavWriter>,
    pending: Vec<f32>,
    /// Samples before `pending` (timestamps of live lines).
    consumed: usize,
}

type Shared = Arc<Mutex<Channel>>;

struct Recording {
    id: String,
    mic: audio::Capture,
    system: Option<audio::Capture>,
    channels: [Shared; 2],
    live_stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    notes: Vec<Note>,
    recording: Option<Recording>,
    working: HashSet<String>,
}

static STATE: Lazy<Mutex<Inner>> = Lazy::new(Default::default);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesState {
    pub notes: Vec<Note>,
    pub recording: Option<String>,
    pub working: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordingPayload {
    id: String,
    started: i64,
    last_line: Option<String>,
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("notes"))
}

fn dir_of(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    // Ids come from the frontend: never let one escape the notes folder.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("Invalid note id.".into());
    }
    Ok(root(app)?.join(id))
}

fn write_note(app: &AppHandle, note: &Note) -> Result<(), String> {
    let dir = dir_of(app, &note.id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("note.json.tmp");
    let json = serde_json::to_vec_pretty(note).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("note.json")).map_err(|e| e.to_string())
}

fn changed(app: &AppHandle) {
    let _ = app.emit("soul://notes-changed", ());
}

fn emit_recording(app: &AppHandle, payload: Option<RecordingPayload>) {
    let _ = app.emit("island://note-recording", payload);
}

/// Change a note in memory, persist it and tell the UI. Returns the closure's value.
fn update<T>(app: &AppHandle, id: &str, f: impl FnOnce(&mut Note) -> T) -> Result<T, String> {
    let (out, note) = {
        let mut st = STATE.lock();
        let note = st.notes.iter_mut().find(|n| n.id == id).ok_or("Note not found.")?;
        let out = f(note);
        (out, note.clone())
    };
    write_note(app, &note)?;
    changed(app);
    Ok(out)
}

pub fn is_recording() -> bool {
    STATE.lock().recording.is_some()
}

pub fn init(app: &AppHandle) {
    let Ok(root) = root(app) else { return };
    let mut notes: Vec<Note> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read(e.path().join("note.json")).ok())
        .filter_map(|b| serde_json::from_slice(&b).ok())
        .collect();
    notes.sort_by(|a, b| b.created.cmp(&a.created));
    let unfinished: Vec<String> = notes
        .iter()
        .filter(|n| matches!(n.status, Status::Recording | Status::Processing))
        .map(|n| n.id.clone())
        .collect();
    STATE.lock().notes = notes;
    for id in unfinished {
        eprintln!("[soul][notes] resuming unfinished note {id}");
        spawn_process(app, id);
    }
}

/// Flush the WAV headers of a running recording; the next start transcribes it.
pub fn shutdown(app: &AppHandle) {
    if let Some(id) = stop_capture() {
        let _ = update(app, &id, |n| n.status = Status::Processing);
    }
}

fn foreground_app(app: &AppHandle) -> Option<String> {
    let info = app.state::<crate::AppState>().platform.foreground_window()?;
    let stem = info.process.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    let lower = stem.to_lowercase();
    let shell = ["soul", "explorer", "shellexperiencehost", "searchhost", "startmenuexperiencehost", ""];
    (!shell.contains(&lower.as_str())).then_some(stem)
}

fn sink(ch: &Shared) -> audio::Sink {
    let ch = Arc::clone(ch);
    Box::new(move |samples: &[f32]| {
        let mut c = ch.lock();
        if let Some(w) = c.wav.as_mut() {
            if let Err(e) = w.write(samples) {
                eprintln!("[soul][notes] wav write failed: {e}");
                c.wav = None;
            }
        }
        c.pending.extend_from_slice(samples);
    })
}

pub fn start(app: &AppHandle) -> Result<String, String> {
    let settings = load_settings(app).unwrap_or_default();
    let lang = ui_lang(&settings);
    if !speech::is_ready() {
        return Err(tr(
            lang,
            "Najpierw pobierz lokalny model mowy (Ustawienia → Modele).",
            "Download the local speech model first (Settings → Models).",
        )
        .into());
    }
    if is_recording() {
        return Err(tr(lang, "Rozmowa jest już nagrywana.", "A call is already being recorded.").into());
    }
    let now = chrono::Local::now();
    let id = format!("{}-{:04x}", now.format("%Y%m%d-%H%M%S"), now.timestamp_subsec_micros() & 0xffff);
    let dir = dir_of(app, &id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let open = |name: &str| -> Result<Shared, String> {
        let wav = audio::WavWriter::create(&dir.join(name)).map_err(|e| e.to_string())?;
        Ok(Arc::new(Mutex::new(Channel { wav: Some(wav), ..Default::default() })))
    };
    let channels = [open("mic.wav")?, open("system.wav")?];
    let mic_name = Some(settings.microphone.clone()).filter(|m| !m.trim().is_empty());
    let mic = audio::Capture::start(audio::Source::Mic(mic_name), sink(&channels[0]))?;
    let system = match audio::Capture::start(audio::Source::System, sink(&channels[1])) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("[soul][notes] system audio unavailable, mic only: {e}");
            channels[1].lock().wav = None;
            let _ = std::fs::remove_file(dir.join("system.wav"));
            None
        }
    };
    let app_name = foreground_app(app);
    let title = match &app_name {
        Some(a) => format!("{} · {a} · {}", tr(lang, "Rozmowa", "Call"), now.format("%d.%m %H:%M")),
        None => format!("{} · {}", tr(lang, "Rozmowa", "Call"), now.format("%d.%m %H:%M")),
    };
    let note = Note {
        id: id.clone(),
        title,
        title_edited: false,
        created: now.timestamp_millis(),
        duration: 0.0,
        app: app_name,
        status: Status::Recording,
        speakers: vec![
            Speaker { id: builder::ME.into(), name: tr(lang, "Ja", "Me").into(), color: 0, is_me: true },
            Speaker { id: "p2".into(), name: format!("{} 2", tr(lang, "Osoba", "Person")), color: 1, is_me: false },
        ],
        lines: Vec::new(),
        summary: None,
        key_points: Vec::new(),
        todos: Vec::new(),
        error: None,
    };
    write_note(app, &note)?;
    let live_stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = STATE.lock();
        st.notes.insert(0, note);
        st.recording = Some(Recording {
            id: id.clone(),
            mic,
            system,
            channels: channels.clone(),
            live_stop: Arc::clone(&live_stop),
        });
    }
    spawn_live(app.clone(), id.clone(), channels, live_stop);
    eprintln!("[soul][notes] recording started {id}");
    emit_recording(app, Some(RecordingPayload { id: id.clone(), started: now.timestamp_millis(), last_line: None }));
    changed(app);
    Ok(id)
}

/// Stop the captures and finish both WAVs. Returns the note id.
fn stop_capture() -> Option<String> {
    let rec = STATE.lock().recording.take()?;
    rec.live_stop.store(true, Ordering::SeqCst);
    rec.mic.stop();
    if let Some(s) = rec.system {
        s.stop();
    }
    for ch in &rec.channels {
        if let Some(w) = ch.lock().wav.take() {
            if let Err(e) = w.finish() {
                eprintln!("[soul][notes] wav finish failed: {e}");
            }
        }
    }
    Some(rec.id)
}

pub fn stop(app: &AppHandle) -> Result<String, String> {
    let id = stop_capture().ok_or("No call is being recorded.")?;
    eprintln!("[soul][notes] recording stopped {id}");
    emit_recording(app, None);
    update(app, &id, |n| n.status = Status::Processing)?;
    spawn_process(app, id.clone());
    Ok(id)
}

/// A live chunk is ready: ≥ 1.5 s ending in 0.5 s of quiet, or 12 s of continuous speech.
fn live_chunk_ready(pending: &[f32]) -> bool {
    if pending.len() < RATE * 3 / 2 {
        return false;
    }
    let tail = &pending[pending.len() - RATE / 2..];
    pending.len() >= RATE * 12 || tail.iter().all(|s| s.abs() < 0.01)
}

fn spawn_live(app: AppHandle, id: String, channels: [Shared; 2], stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let started = STATE.lock().notes.iter().find(|n| n.id == id).map(|n| n.created).unwrap_or(0);
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(1));
            for (i, ch) in channels.iter().enumerate() {
                let (chunk, offset) = {
                    let mut c = ch.lock();
                    if !live_chunk_ready(&c.pending) {
                        continue;
                    }
                    let chunk = std::mem::take(&mut c.pending);
                    let offset = c.consumed;
                    c.consumed += chunk.len();
                    (chunk, offset)
                };
                if chunk.iter().all(|s| s.abs() < 0.01) {
                    continue; // silence: nothing to transcribe
                }
                let Some(t) = speech::transcribe(&chunk) else { continue };
                let shift = offset as f64 / RATE as f64;
                let speaker = if i == 0 { builder::ME } else { "p2" };
                let words: Vec<_> = t
                    .words
                    .into_iter()
                    .map(|mut w| {
                        w.start += shift;
                        w.end += shift;
                        (speaker.to_string(), w)
                    })
                    .collect();
                let new_lines = builder::lines(&words);
                let last = update(&app, &id, |n| {
                    if n.status != Status::Recording {
                        return None; // stopped meanwhile: the full pass owns the lines now
                    }
                    let mut add = if i == 0 {
                        let theirs: Vec<Line> =
                            n.lines.iter().filter(|l| l.speaker != builder::ME).cloned().collect();
                        builder::removing_echo(&new_lines, &theirs)
                    } else {
                        new_lines
                    };
                    for l in &mut add {
                        l.id = format!("live{}", n.lines.len());
                        n.lines.push(l.clone());
                    }
                    n.lines.sort_by(|a, b| a.start.total_cmp(&b.start));
                    n.duration = n.duration.max(n.lines.last().map(|l| l.end).unwrap_or(0.0));
                    add.last().map(|l| l.text.chars().take(80).collect::<String>())
                });
                if let Ok(Some(last_line)) = last {
                    if is_recording() {
                        emit_recording(
                            &app,
                            Some(RecordingPayload { id: id.clone(), started, last_line: Some(last_line) }),
                        );
                    }
                }
            }
        }
    });
}

fn spawn_process(app: &AppHandle, id: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        if !STATE.lock().working.insert(id.clone()) {
            return;
        }
        changed(&app);
        let result = process(&app, &id);
        STATE.lock().working.remove(&id);
        match result {
            Ok(summarize_next) => {
                if summarize_next {
                    summarize_and_toast(&app, &id);
                }
            }
            Err(e) => {
                eprintln!("[soul][notes] processing {id} failed: {e}");
                let _ = update(&app, &id, |n| {
                    n.status = Status::Failed;
                    n.error = Some(e);
                });
            }
        }
        changed(&app);
    });
}

/// Full transcript from the WAVs. Ok(true) = summarize next.
fn process(app: &AppHandle, id: &str) -> Result<bool, String> {
    let settings = load_settings(app).unwrap_or_default();
    let lang = ui_lang(&settings);
    let dir = dir_of(app, id)?;
    let (mic_path, sys_path) = (dir.join("mic.wav"), dir.join("system.wav"));
    if !mic_path.exists() {
        // Already transcribed (WAVs deleted): just make sure it's marked ready.
        update(app, id, |n| n.status = Status::Ready)?;
        return Ok(false);
    }
    let _ = update(app, id, |n| {
        n.status = Status::Processing;
        n.error = None;
    });
    let missing = || tr(lang, "Brak lokalnego modelu mowy.", "Local speech model missing.").to_string();
    let mic = audio::read_wav(&mic_path).map_err(|e| e.to_string())?;
    let sys = audio::read_wav(&sys_path).unwrap_or_default();
    let t = std::time::Instant::now();
    let mine = speech::transcribe(&mic).ok_or_else(missing)?.words;
    let others = if sys.is_empty() { Vec::new() } else { speech::transcribe(&sys).ok_or_else(missing)?.words };
    let me_name = tr(lang, "Ja", "Me");
    let person = tr(lang, "Osoba", "Person");
    let (speakers, lines) = if others.len() >= 5 {
        builder::build(&mine, &others, &speech::diarize(&sys), me_name, person)
    } else if mine.len() >= 20 {
        let turns = speech::diarize(&mic);
        let voices: HashSet<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
        if voices.len() > 1 {
            builder::build_room(&mine, &turns, person)
        } else {
            builder::build(&mine, &[], &[], me_name, person)
        }
    } else {
        builder::build(&mine, &others, &[], me_name, person)
    };
    eprintln!(
        "[soul][notes] {id} transcribed ms={} lines={} speakers={}",
        t.elapsed().as_millis(),
        lines.len(),
        speakers.len()
    );
    let duration = mic.len().max(sys.len()) as f64 / RATE as f64;
    let has_lines = !lines.is_empty();
    update(app, id, |n| {
        // Keep names the user already gave to speakers that still exist.
        let renamed: Vec<Speaker> = n.speakers.clone();
        n.speakers = speakers
            .into_iter()
            .map(|mut s| {
                if let Some(old) = renamed.iter().find(|o| o.id == s.id && o.name != s.name && !s.is_me) {
                    s.name = old.name.clone();
                }
                s
            })
            .collect();
        n.lines = lines;
        n.duration = duration;
        n.status = Status::Ready;
    })?;
    let _ = std::fs::remove_file(&mic_path);
    let _ = std::fs::remove_file(&sys_path);
    Ok(has_lines && !settings.api_key.trim().is_empty())
}

fn summarize(app: &AppHandle, id: &str) -> Result<(), String> {
    let settings = load_settings(app)?;
    let note = STATE.lock().notes.iter().find(|n| n.id == id).cloned().ok_or("Note not found.")?;
    if note.lines.is_empty() {
        return Err("Empty transcript.".into());
    }
    let fallback = crate::resolve_fallback_model(&settings, &crate::ai::gemini::cached_models());
    let client = crate::ai::gemini::GeminiClient::new(&settings.api_key, settings.effective_ask_model());
    let system = format!(
        "{} {}",
        builder::SUMMARY_INSTRUCTIONS,
        crate::ai::gemini::reply_language_rule(&settings.language)
    );
    let body = serde_json::json!({
        "system_instruction": { "parts": [{ "text": system }] },
        "contents": [{ "role": "user", "parts": [{ "text": format!("<transcript>\n{}\n</transcript>", builder::transcript_text(&note)) }] }],
        "generation_config": { "temperature": 0.3, "responseMimeType": "application/json" }
    });
    let value = client.agent_generate_resilient(body, Some(&fallback), |_, _| {}, |_| {})?;
    let reply = crate::ai::gemini::extract_text_from_value(&value)?;
    let summary = builder::parse_summary(&reply).ok_or("Could not read the summary.")?;
    update(app, id, |n| {
        if !n.title_edited && !summary.title.is_empty() {
            n.title = summary.title;
        }
        n.summary = Some(summary.summary);
        n.key_points = summary.key_points;
        n.todos = summary.todos;
        n.error = None;
    })
}

fn summarize_and_toast(app: &AppHandle, id: &str) {
    let lang = ui_lang(&load_settings(app).unwrap_or_default());
    match summarize(app, id) {
        Ok(()) => {
            eprintln!("[soul][notes] {id} summarized");
            let title = STATE.lock().notes.iter().find(|n| n.id == id).map(|n| n.title.clone()).unwrap_or_default();
            crate::island::toast(
                app,
                "info",
                &format!("{}: {title}", tr(lang, "Notatki ze spotkania gotowe", "Meeting notes ready")),
                None,
            );
        }
        Err(e) => {
            eprintln!("[soul][notes] {id} summary failed: {e}");
            let _ = update(app, id, |n| n.error = Some(e));
        }
    }
}

fn file_name(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_end_matches('.').chars().take(80).collect::<String>();
    if clean.is_empty() { "Notes".into() } else { clean }
}

fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.md"));
    if !first.exists() {
        return first;
    }
    (2..).map(|i| dir.join(format!("{stem} ({i}).md"))).find(|p| !p.exists()).unwrap_or(first)
}

#[tauri::command]
pub fn notes_state() -> NotesState {
    let st = STATE.lock();
    NotesState {
        notes: st.notes.clone(),
        recording: st.recording.as_ref().map(|r| r.id.clone()),
        working: st.working.iter().cloned().collect(),
    }
}

#[tauri::command]
pub fn notes_start(app: AppHandle) -> Result<String, String> {
    start(&app)
}

#[tauri::command]
pub fn notes_stop(app: AppHandle) -> Result<String, String> {
    stop(&app)
}

#[tauri::command]
pub fn notes_rename(app: AppHandle, id: String, title: String) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Empty title.".into());
    }
    update(&app, &id, |n| {
        n.title = title;
        n.title_edited = true;
    })
}

#[tauri::command]
pub fn notes_rename_speaker(app: AppHandle, id: String, speaker: String, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Empty name.".into());
    }
    update(&app, &id, |n| {
        if let Some(s) = n.speakers.iter_mut().find(|s| s.id == speaker) {
            s.name = name;
        }
    })
}

#[tauri::command]
pub fn notes_toggle_todo(app: AppHandle, id: String, todo: String) -> Result<(), String> {
    update(&app, &id, |n| {
        if let Some(t) = n.todos.iter_mut().find(|t| t.id == todo) {
            t.done = !t.done;
        }
    })
}

#[tauri::command]
pub fn notes_delete(app: AppHandle, id: String) -> Result<(), String> {
    let dir = dir_of(&app, &id)?;
    {
        let mut st = STATE.lock();
        if st.recording.as_ref().is_some_and(|r| r.id == id) || st.working.contains(&id) {
            return Err("This note is still being recorded or processed.".into());
        }
        st.notes.retain(|n| n.id != id);
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn notes_summarize(app: AppHandle, id: String) {
    std::thread::spawn(move || {
        if !STATE.lock().working.insert(id.clone()) {
            return;
        }
        changed(&app);
        summarize_and_toast(&app, &id);
        STATE.lock().working.remove(&id);
        changed(&app);
    });
}

#[tauri::command]
pub fn notes_retry(app: AppHandle, id: String) -> Result<(), String> {
    let wavs = dir_of(&app, &id)?.join("mic.wav").exists();
    if wavs {
        spawn_process(&app, id);
    } else {
        notes_summarize(app, id);
    }
    Ok(())
}

#[tauri::command]
pub fn notes_markdown(app: AppHandle, id: String) -> Option<String> {
    let lang = ui_lang(&load_settings(&app).unwrap_or_default());
    let st = STATE.lock();
    st.notes.iter().find(|n| n.id == id).map(|n| builder::markdown(n, lang))
}

#[tauri::command]
pub fn notes_export(app: AppHandle, id: String) -> Result<String, String> {
    let lang = ui_lang(&load_settings(&app).unwrap_or_default());
    let note = STATE.lock().notes.iter().find(|n| n.id == id).cloned().ok_or("Note not found.")?;
    let dir = app.path().document_dir().map_err(|e| e.to_string())?;
    let path = free_path(&dir, &file_name(&note.title));
    std::fs::write(&path, builder::markdown(&note, lang)).map_err(|e| e.to_string())?;
    crate::coding_agents::session_window::reveal(&dir);
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_chunks_wait_for_a_pause_or_12_s() {
        let mut speech = vec![0.3f32; RATE * 2];
        assert!(!live_chunk_ready(&speech));
        speech.extend(vec![0.0; RATE / 2]);
        assert!(live_chunk_ready(&speech));
        assert!(!live_chunk_ready(&[0.0; RATE]));
        assert!(live_chunk_ready(&vec![0.3; RATE * 12]));
    }

    #[test]
    fn export_names_are_safe_and_never_overwrite() {
        assert_eq!(file_name("Plan: Q4/2026?"), "Plan_ Q4_2026_");
        assert_eq!(file_name("  ..."), "Notes");
        let dir = std::env::temp_dir().join(format!("soul-notes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.md"), "x").unwrap();
        assert_eq!(free_path(&dir, "A"), dir.join("A (2).md"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
