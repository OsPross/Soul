//! Selected-text actions: „popraw to”, „przetłumacz to na angielski”, „streść to”, „zrób to
//! bardziej formalne / luźniejsze”, „rozwiń to”, „wyjaśnij to”.
//!
//! Only an explicit, deterministic match of one of these phrases reads the selection (Windows:
//! simulated copy with clipboard save/restore; X11: PRIMARY) and sends it to Gemini — the models
//! have no tool for it. Replace actions paste the result over the selection with a 10 s „Cofnij”
//! on the Island; info actions show the result on the Island with „Kopiuj” / „Wklej”.

use super::router::{fold_word, strip_filler};
use crate::ai::gemini::{reply_language_rule, GeminiClient};
use crate::config::load_settings;
use crate::platform::Platform;
use crate::{emit_overlay, resolve_fallback_model, AppState};
use parking_lot::Mutex;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// How long „Cofnij” stays valid after a replace action.
pub const UNDO_WINDOW: Duration = Duration::from_secs(10);
/// Larger selections are refused (prompt size, and it is rarely what the user meant).
const MAX_SELECTION_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextAction {
    Fix,
    /// Target language as an English name („English”); None = Polish ↔ English.
    Translate(Option<&'static str>),
    Summarize,
    Formal,
    Casual,
    Expand,
    Explain,
}

impl TextAction {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Fix => "fix",
            Self::Translate(_) => "translate",
            Self::Summarize => "summarize",
            Self::Formal => "formal",
            Self::Casual => "casual",
            Self::Expand => "expand",
            Self::Explain => "explain",
        }
    }

    /// Recorded in the context / harness: the action, never the text.
    pub fn args(&self) -> serde_json::Value {
        match self {
            Self::Translate(Some(lang)) => json!({ "action": self.id(), "language": lang }),
            _ => json!({ "action": self.id() }),
        }
    }

    /// Replace actions paste over the selection; the others only show the result.
    pub fn replaces(&self) -> bool {
        !matches!(self, Self::Summarize | Self::Explain)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Fix => "Poprawiono tekst".into(),
            Self::Translate(Some(lang)) => format!("Przetłumaczono ({})", polish_name(lang)),
            Self::Translate(None) => "Przetłumaczono".into(),
            Self::Summarize => "Streszczenie".into(),
            Self::Formal => "Bardziej formalnie".into(),
            Self::Casual => "Luźniej".into(),
            Self::Expand => "Rozwinięto tekst".into(),
            Self::Explain => "Wyjaśnienie".into(),
        }
    }

    /// Model instruction; `language` = Soul's reply-language setting (info actions only).
    pub fn instruction(&self, language: &str) -> String {
        const KEEP: &str = "Keep the original language, meaning, names and formatting (line breaks, lists, markdown, code).";
        match self {
            Self::Fix => format!("Correct spelling, grammar, punctuation and Polish diacritics. Change nothing else: keep the wording and tone. {KEEP}"),
            Self::Translate(Some(lang)) => format!("Translate the text into {lang}. Keep names, tone and formatting (line breaks, lists, markdown, code)."),
            Self::Translate(None) => "Translate the text: Polish text into English, text in any other language into Polish. Keep names, tone and formatting (line breaks, lists, markdown, code).".into(),
            Self::Formal => format!("Rewrite the text in a more formal, professional register. {KEEP}"),
            Self::Casual => format!("Rewrite the text in a more casual, relaxed and friendly register. {KEEP}"),
            Self::Expand => format!("Expand the text: develop its ideas with more detail and a smoother flow, about 1.5–2 times longer. {KEEP}"),
            Self::Summarize => format!("Summarize the text in two to four short sentences (a few bullet points for long, structured text). {}", reply_language_rule(language)),
            Self::Explain => format!("Explain briefly and clearly what the text means: terms, context, or what the code does. At most six sentences. {}", reply_language_rule(language)),
        }
    }
}

fn polish_name(english: &str) -> &'static str {
    LANGUAGES
        .iter()
        .find(|(_, en, _)| *en == english)
        .map(|(_, _, pl)| *pl)
        .unwrap_or("inny język")
}

/// (folded word stems, English name, Polish label)
const LANGUAGES: &[(&[&str], &str, &str)] = &[
    (&["angielski", "angielsku", "english"], "English", "angielski"),
    (&["polski", "polsku", "polish"], "Polish", "polski"),
    (&["niemiecki", "niemiecku", "german"], "German", "niemiecki"),
    (&["francuski", "francusku", "french"], "French", "francuski"),
    (&["hiszpanski", "hiszpansku", "spanish"], "Spanish", "hiszpański"),
    (&["wloski", "wlosku", "italian"], "Italian", "włoski"),
    (&["ukrainski", "ukrainsku", "ukrainian"], "Ukrainian", "ukraiński"),
    (&["czeski", "czesku", "czech"], "Czech", "czeski"),
    (&["rosyjski", "rosyjsku", "russian"], "Russian", "rosyjski"),
    (&["japonski", "japonsku", "japanese"], "Japanese", "japoński"),
];

/// What the action applies to: „to”, „ten tekst”, „zaznaczenie”, "this", "the selection".
const OBJECT: &[&str] = &[
    "to", "ten", "tekst", "tekstu", "tekscie", "zaznaczenie", "zaznaczeniu", "zaznaczony", "zaznaczone", "zaznaczona",
    "fragment", "this", "it", "that", "text", "selection", "selected", "the",
];
/// Harmless extra words („popraw mi to proszę”).
const EXTRA: &[&str] = &["mi", "prosze", "please", "for", "me", "troche", "nieco", "a", "bit", "jeszcze"];
const FIX_OBJECTS: &[&str] = &["bledy", "literowki", "gramatyke", "ortografie", "interpunkcje", "mistakes", "typos", "grammar", "spelling"];
const FORMAL: &[&str] = &["formalne", "formalny", "formalna", "formalnie", "oficjalne", "oficjalnie", "profesjonalne", "profesjonalnie", "formal", "professional"];
const CASUAL: &[&str] = &["luzniejsze", "luzniejszy", "luzniej", "luzne", "luzno", "swobodniejsze", "swobodniej", "casual", "informal", "relaxed", "friendlier"];

/// Match a text-action command (after Polish fillers); None for everything else.
/// Every remaining word must be an object / filler / language word, so dictation like
/// „Popraw mi humor” or „Wyjaśnij mi, jak działa silnik” never reads the selection.
pub fn parse(text: &str) -> Option<TextAction> {
    let all: Vec<String> = text
        .split_whitespace()
        .map(fold_word)
        .filter(|w| !w.is_empty())
        .collect();
    let (skip, _) = strip_filler(&all);
    let words = &all[skip..];
    let (verb, rest) = words.split_first()?;
    if rest.len() > 6 {
        return None;
    }
    let in_set = |w: &String, sets: &[&[&str]]| sets.iter().any(|s| s.contains(&w.as_str()));
    let has_object = rest.iter().any(|w| OBJECT.contains(&w.as_str()));
    match verb.as_str() {
        "przetlumacz" | "tlumacz" | "translate" => {
            let lang = rest
                .iter()
                .find_map(|w| LANGUAGES.iter().find(|(stems, _, _)| stems.contains(&w.as_str())))
                .map(|(_, en, _)| *en);
            let ok = rest.iter().all(|w| {
                in_set(w, &[OBJECT, EXTRA, &["na", "po", "into", "to"]])
                    || LANGUAGES.iter().any(|(stems, _, _)| stems.contains(&w.as_str()))
            });
            (ok && (has_object || lang.is_some() || rest.is_empty())).then_some(TextAction::Translate(lang))
        }
        "popraw" | "correct" | "fix" | "proofread" => {
            let ok = rest.iter().all(|w| in_set(w, &[OBJECT, EXTRA, FIX_OBJECTS, &["w", "in"]]));
            let target = has_object || rest.iter().any(|w| FIX_OBJECTS.contains(&w.as_str()));
            (ok && target).then_some(TextAction::Fix)
        }
        "stresc" | "streszcz" | "podsumuj" | "summarize" | "summarise" => {
            let ok = rest.iter().all(|w| in_set(w, &[OBJECT, EXTRA]));
            (ok && (has_object || rest.is_empty())).then_some(TextAction::Summarize)
        }
        "rozwin" | "rozbuduj" | "expand" | "elaborate" => {
            let ok = rest.iter().all(|w| in_set(w, &[OBJECT, EXTRA]));
            (ok && (has_object || rest.is_empty())).then_some(TextAction::Expand)
        }
        "wyjasnij" | "objasnij" | "explain" => {
            let ok = rest.iter().all(|w| in_set(w, &[OBJECT, EXTRA]));
            (ok && has_object).then_some(TextAction::Explain)
        }
        "zrob" | "napisz" | "przeredaguj" | "przepisz" | "make" | "rewrite" => {
            let less = rest.iter().any(|w| w == "mniej" || w == "less");
            let formal = rest.iter().any(|w| FORMAL.contains(&w.as_str()));
            let casual = rest.iter().any(|w| CASUAL.contains(&w.as_str()));
            let ok = rest.iter().all(|w| {
                in_set(w, &[OBJECT, EXTRA, FORMAL, CASUAL, &["bardziej", "mniej", "more", "less", "w", "in", "way", "sposob", "tone", "tonie"]])
            });
            if !ok || !has_object {
                return None;
            }
            match (formal, casual, less) {
                (true, false, false) => Some(TextAction::Formal),
                (true, false, true) | (false, true, _) => Some(TextAction::Casual),
                _ => None,
            }
        }
        _ => None,
    }
}

// ───────────────────────────── execution ─────────────────────────────

struct Undo {
    id: u64,
    window: Option<u64>,
    at: Instant,
}

struct Info {
    id: u64,
    text: String,
    window: Option<u64>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static LAST_UNDO: Mutex<Option<Undo>> = Mutex::new(None);
static LAST_INFO: Mutex<Option<Info>> = Mutex::new(None);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TextResultPayload {
    id: u64,
    title: String,
    text: String,
}

/// Read the selection, transform it, then paste it back (replace actions) or show it.
/// Blocking: runs on the command's worker thread.
pub fn run(app: &AppHandle, platform: &Arc<dyn Platform>, action: &TextAction) -> Result<String, String> {
    super::context::note_tool(super::context::TEXT_ACTION_TOOL, &action.args(), true);
    if super::dry_run() {
        eprintln!("[soul][agent] dry_run text_action={} (selection not read)", action.id());
        return Ok("dry-run: text_action was not executed".into());
    }
    eprintln!("[soul][agent] text_action={}", action.id());
    crate::island::emit_tool_labeled(app, "text_action", Some(&action.label()), "start");
    let result = run_inner(app, platform, action);
    crate::island::emit_tool_labeled(
        app,
        "text_action",
        Some(&action.label()),
        if result.is_ok() { "ok" } else { "error" },
    );
    result
}

fn run_inner(app: &AppHandle, platform: &Arc<dyn Platform>, action: &TextAction) -> Result<String, String> {
    let settings = load_settings(app)?;
    if settings.api_key.trim().is_empty() {
        return Err("No API key set. Open Settings (tray → Settings) and paste your Gemini API key.".into());
    }
    // The app the user is working in (the Island takes focus when its buttons are clicked).
    let window = platform.active_window();
    let t0 = Instant::now();
    let selection = platform
        .read_selection()?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "Nic nie jest zaznaczone — zaznacz tekst i powtórz polecenie.".to_string())?;
    let chars = selection.chars().count();
    eprintln!(
        "[soul][agent] text_action selection chars={chars} ms={}",
        t0.elapsed().as_millis()
    );
    if chars > MAX_SELECTION_CHARS {
        return Err(format!(
            "Zaznaczenie jest za długie ({chars} znaków, maks. {MAX_SELECTION_CHARS})."
        ));
    }
    let _ = emit_overlay(app, "thinking", Some(format!("{}…", action.label())));
    let client = GeminiClient::new(&settings.api_key, settings.effective_ask_model());
    let fallback = resolve_fallback_model(&settings, &crate::ai::gemini::cached_models());
    let out = client.text_action(&action.instruction(&settings.language), &selection, Some(&fallback))?;
    eprintln!(
        "[soul][agent] text_action done ms={} chars={}",
        t0.elapsed().as_millis(),
        out.chars().count()
    );
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    if action.replaces() {
        if let Some(w) = window {
            let _ = platform.activate_window(w);
        }
        platform.paste_text(&out)?;
        *LAST_UNDO.lock() = Some(Undo {
            id,
            window,
            at: Instant::now(),
        });
        crate::island::toast(
            app,
            "text",
            &action.label(),
            Some(crate::island::ToastAction {
                label: "Cofnij".into(),
                command: "text_action_undo".into(),
                args: json!({ "id": id }),
            }),
        );
        Ok(format!("{} — wklejono w miejsce zaznaczenia.", action.label()))
    } else {
        *LAST_INFO.lock() = Some(Info {
            id,
            text: out.clone(),
            window,
        });
        let _ = app.emit_to(
            tauri::EventTarget::webview_window("island"),
            "island://text-result",
            TextResultPayload {
                id,
                title: action.label(),
                text: out,
            },
        );
        Ok(format!("{} — wynik na wyspie.", action.label()))
    }
}

fn platform_of(app: &AppHandle) -> Arc<dyn Platform> {
    Arc::clone(&app.state::<AppState>().platform)
}

/// Island „Cofnij” (10 s): back to the app, then its own undo restores the original text.
#[tauri::command]
pub fn text_action_undo(app: AppHandle, id: u64) -> Result<(), String> {
    let undo = LAST_UNDO
        .lock()
        .take()
        .filter(|u| u.id == id && u.at.elapsed() < UNDO_WINDOW + Duration::from_secs(1))
        .ok_or_else(|| "Nie można już cofnąć.".to_string())?;
    let platform = platform_of(&app);
    thread::spawn(move || {
        if let Some(w) = undo.window {
            let _ = platform.activate_window(w);
            thread::sleep(Duration::from_millis(120));
        }
        match platform.send_undo() {
            Ok(()) => eprintln!("[soul][agent] text_action undo sent"),
            Err(e) => crate::report_error(&app, format!("Cofnięcie nie powiodło się: {e}")),
        }
    });
    Ok(())
}

/// Island „Kopiuj” on an info result (summary / explanation).
#[tauri::command]
pub fn text_result_copy(id: u64) -> Result<(), String> {
    let text = LAST_INFO
        .lock()
        .as_ref()
        .filter(|i| i.id == id)
        .map(|i| i.text.clone())
        .ok_or_else(|| "Wynik nie jest już dostępny.".to_string())?;
    crate::platform::clipboard::copy_only(&text)
}

/// Island „Wklej”: paste the result into the app the text came from (clipboard restored).
#[tauri::command]
pub fn text_result_paste(app: AppHandle, id: u64) -> Result<(), String> {
    let (text, window) = LAST_INFO
        .lock()
        .as_ref()
        .filter(|i| i.id == id)
        .map(|i| (i.text.clone(), i.window))
        .ok_or_else(|| "Wynik nie jest już dostępny.".to_string())?;
    let platform = platform_of(&app);
    thread::spawn(move || {
        if let Some(w) = window {
            let _ = platform.activate_window(w);
            thread::sleep(Duration::from_millis(120));
        }
        if let Err(e) = platform.paste_text(&text) {
            crate::report_error(&app, e);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_actions() {
        assert_eq!(parse("Przetłumacz to na angielski."), Some(TextAction::Translate(Some("English"))));
        assert_eq!(parse("przetlumacz to po niemiecku"), Some(TextAction::Translate(Some("German"))));
        assert_eq!(parse("Przetłumacz zaznaczony tekst"), Some(TextAction::Translate(None)));
        assert_eq!(parse("translate this to Polish"), Some(TextAction::Translate(Some("Polish"))));
        assert_eq!(parse("Popraw to."), Some(TextAction::Fix));
        assert_eq!(parse("popraw błędy w tekście"), Some(TextAction::Fix));
        assert_eq!(parse("Soul, popraw mi to proszę"), Some(TextAction::Fix));
        assert_eq!(parse("weź popraw mi to proszę"), Some(TextAction::Fix));
        assert_eq!(parse("Streść to"), Some(TextAction::Summarize));
        assert_eq!(parse("podsumuj zaznaczenie"), Some(TextAction::Summarize));
        assert_eq!(parse("Zrób to bardziej formalne"), Some(TextAction::Formal));
        assert_eq!(parse("napisz to bardziej oficjalnie"), Some(TextAction::Formal));
        assert_eq!(parse("Zrób to luźniejsze"), Some(TextAction::Casual));
        assert_eq!(parse("zrób to mniej formalne"), Some(TextAction::Casual));
        assert_eq!(parse("make it more casual"), Some(TextAction::Casual));
        assert_eq!(parse("Rozwiń to"), Some(TextAction::Expand));
        assert_eq!(parse("Wyjaśnij to"), Some(TextAction::Explain));
        assert_eq!(parse("explain this"), Some(TextAction::Explain));
    }

    #[test]
    fn dictation_lookalikes_do_not_read_the_selection() {
        for t in [
            "Popraw mi humor",
            "Wyjaśnij mi, jak działa silnik spalinowy",
            "Wyjaśnij",
            "Przetłumacz ten dokument dla klienta do jutra rana",
            "Zrób to jutro rano",
            "Zrób to bardziej",
            "Rozwiń skrzydła i leć",
            "Popraw",
            "Streść rozdział trzeci książki dla klasy",
        ] {
            assert_eq!(parse(t), None, "{t}");
        }
    }

    #[test]
    fn replace_vs_info() {
        assert!(TextAction::Fix.replaces());
        assert!(TextAction::Translate(None).replaces());
        assert!(TextAction::Formal.replaces() && TextAction::Casual.replaces() && TextAction::Expand.replaces());
        assert!(!TextAction::Summarize.replaces() && !TextAction::Explain.replaces());
        assert!(TextAction::Fix.instruction("auto").contains("original language"));
        assert!(TextAction::Translate(Some("English")).instruction("pl").contains("into English"));
        assert_eq!(TextAction::Translate(Some("English")).label(), "Przetłumaczono (angielski)");
    }
}
