//! Local safety checks for the computer agent. They run on every step, independently of the
//! model: a model that forgets `risky: true` still gets a confirmation card before „Wyślij”, and
//! nothing the model says can unlock a blocklisted window, a password field or `Win+R`.
//!
//! Verdict order: Refuse > Confirm > Allow.

use super::action::{Action, Decision, Target};
use crate::agent::fold_word;
use crate::platform::input::Key;
use crate::platform::ui::{clip, Role, UiElement, UiSnapshot, WindowInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Ask the user first; the text is the reason shown on the card.
    Confirm(String),
    /// Never; the text is told to the user.
    Refuse(String),
}

/// `always` = confirm every state-changing action; `risky` = only risky ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmLevel {
    Always,
    Risky,
}

impl ConfirmLevel {
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("always") {
            Self::Always
        } else {
            Self::Risky
        }
    }
}

/// Default blocklist (case-insensitive substrings of the process name, window title or the
/// browser's address bar). Editable in Settings → Agent.
pub const DEFAULT_BLOCKLIST: &[&str] = &[
    // Password managers
    "1password", "bitwarden", "keepass", "lastpass", "dashlane", "nordpass", "enpass", "proton pass",
    "roboform", "keeper",
    // Banking / payments (titles and domains)
    "bank", "pko", "ipko", "mbank", "ing.pl", "ing bank", "santander", "pekao", "millennium", "alior",
    "credit agricole", "bnp paribas", "citi handlowy", "velobank", "nest bank", "inteligo", "revolut",
    "paypal", "blik", "przelewy24", "payu",
    // Windows Security, UAC and credential prompts
    "zabezpieczenia windows", "windows security", "sechealthui", "consent.exe", "credentialuibroker",
    "kontrola konta użytkownika", "user account control",
    // Shells, Run dialog, system tools (no arbitrary command execution through the UI)
    "cmd.exe", "powershell", "pwsh", "windowsterminal", "wt.exe", "conhost", "regedit", "mmc.exe",
    "taskmgr", "gnome-terminal", "konsole", "xterm", "alacritty", "kitty", "terminal",
];

/// Process names that are terminals / Run (never typed into even if the user removes the
/// blocklist entry — no arbitrary shell through the agent).
const ALWAYS_BLOCKED_PROCESSES: &[&str] = &[
    "cmd.exe", "powershell.exe", "pwsh.exe", "windowsterminal.exe", "wt.exe", "conhost.exe", "openconsole.exe",
    "regedit.exe", "consent.exe", "credentialuibroker.exe", "mmc.exe",
];

/// Window titles of the Run dialog (pl / en).
const RUN_DIALOG_TITLES: &[&str] = &["uruchamianie", "run"];

/// Folded words on a target that mean "this sends / buys / deletes", with the reason shown on
/// the confirmation card.
const RISK_GROUPS: &[(&[&str], &str)] = &[
    (
        &[
            "wyslij", "wyslac", "send", "przeslij", "opublikuj", "publish", "post", "udostepnij", "share", "submit",
            "zatwierdz", "potwierdz", "confirm", "tweet", "odpowiedz wszystkim",
        ],
        "wysłanie / zatwierdzenie",
    ),
    (
        &[
            "kup", "kupuje", "zaplac", "platnosc", "zamow", "zamawiam", "buy", "pay", "checkout", "purchase", "order",
            "subskrybuj", "subscribe",
        ],
        "płatność / zakup",
    ),
    (
        &[
            "usun", "usuwam", "delete", "remove", "odinstaluj", "uninstall", "wyczysc", "clear", "erase", "format",
            "sformatuj", "reset", "zresetuj", "trash", "kosz", "kosza", "wyrzuc",
        ],
        "usunięcie",
    ),
    (
        &[
            "discard", "odrzuc", "nie zapisuj", "dont save", "don t save", "zamknij bez zapisywania", "wyloguj",
            "sign out", "log out", "logout",
        ],
        "utrata zmian / wylogowanie",
    ),
];

/// The reason a target's name is risky, if it is. Short words (≤ 5 letters: "post", "order",
/// "clear") need a whole-word match so "Postęp" / "Ordered list" don't count; longer ones may
/// match as a word prefix („Wyślij” / „wysłanie”).
fn has_risky_word(text: &str) -> Option<&'static str> {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).map(fold_word).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let joined = words.join(" ");
    for (group, reason) in RISK_GROUPS {
        for w in group.iter() {
            let hit = if w.contains(' ') {
                joined.contains(w)
            } else if w.len() <= 5 {
                words.iter().any(|x| x == w)
            } else {
                words.iter().any(|x| x.starts_with(w))
            };
            if hit {
                return Some(reason);
            }
        }
    }
    None
}

/// Messenger / mail apps: Enter in a text box there sends a message.
const MESSENGER_HINTS: &[&str] = &[
    "discord", "messenger", "whatsapp", "telegram", "signal", "slack", "teams", "skype", "outlook", "thunderbird",
    "gmail", "poczta", "mail", "instagram", "facebook", "twitter", "x.com", "linkedin", "zoom",
];

/// System settings windows: any state change there needs a confirmation.
fn is_system_settings(w: &WindowInfo) -> bool {
    let p = w.process.to_lowercase();
    let t = w.title.to_lowercase();
    p == "systemsettings.exe"
        || p == "rundll32.exe"
        || p.starts_with("gnome-control-center")
        || p.starts_with("systemsettings")
        || (p == "applicationframehost.exe" && (t == "ustawienia" || t == "settings"))
        || t == "dźwięk"
        || t == "sound"
        || t.contains("panel sterowania")
        || t.contains("control panel")
}

fn is_messenger(w: &WindowInfo) -> bool {
    let p = w.process.to_lowercase();
    let t = w.title.to_lowercase();
    MESSENGER_HINTS.iter().any(|h| p.contains(h) || t.contains(h))
}

/// Address bar value of a browser window (Chromium „Pasek adresu i wyszukiwania” / Firefox).
fn address_bar(s: &UiSnapshot) -> Option<String> {
    s.elements
        .iter()
        .find(|e| {
            e.role == Role::Edit && {
                let n = fold_word_text(&e.name);
                n.contains("pasek adresu") || n.contains("address and search") || n.contains("address bar") || n.contains("wpisz adres")
            }
        })
        .and_then(|e| e.value.clone())
}

fn fold_word_text(s: &str) -> String {
    s.split_whitespace().map(fold_word).collect::<Vec<_>>().join(" ")
}

/// Blocklisted window → the matching entry. `extra` = the browser address bar (if any).
pub fn blocked_by(w: &WindowInfo, extra: Option<&str>, blocklist: &[String]) -> Option<String> {
    let process = w.process.to_lowercase();
    if ALWAYS_BLOCKED_PROCESSES.contains(&process.as_str()) {
        return Some(w.process.clone());
    }
    let title = w.title.to_lowercase();
    if process == "explorer.exe" && RUN_DIALOG_TITLES.contains(&title.trim()) {
        return Some(w.title.clone());
    }
    let hay = [process.as_str(), title.as_str(), &extra.unwrap_or_default().to_lowercase()].join(" \u{1f} ");
    blocklist
        .iter()
        .map(|b| b.trim().to_lowercase())
        .filter(|b| b.len() >= 3)
        .find(|b| {
            // Short entries ("pko", "ing", "bank") as whole words so "Bankietowa sala" in a
            // document title or "booking" don't trip it; longer ones as substrings.
            if b.len() <= 4 {
                hay.split(|c: char| !c.is_alphanumeric()).any(|w| w == b)
            } else {
                hay.contains(b.as_str())
            }
        })
}

/// Snapshot-level check run before every step.
pub fn check_window(s: &UiSnapshot, blocklist: &[String]) -> Verdict {
    match blocked_by(&s.window, address_bar(s).as_deref(), blocklist) {
        Some(entry) => Verdict::Refuse(format!(
            "To okno jest na liście zablokowanych ({}): nie steruję nim.",
            clip(&entry, 40)
        )),
        None => Verdict::Allow,
    }
}

/// Text that looks like a password / API key / token (shape-based, like clipboard history).
/// URLs, file paths and e-mail addresses are long single tokens too, but not secrets.
pub fn looks_like_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    let url = lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") || lower.contains("://");
    let path = t.get(1..3) == Some(":\\") || t.starts_with('/') || t.starts_with("~/") || t.starts_with("\\\\");
    let email = !t.contains(char::is_whitespace)
        && t.matches('@').count() == 1
        && t.rsplit('@').next().is_some_and(|d| d.contains('.'));
    if (url || path || email) && !lower.contains("password=") && !lower.contains("token=") {
        return false;
    }
    crate::island::clipboard::looks_like_secret(t)
}

/// „Znajdź ustawienie”, „Szukaj”, "Search": typing there changes nothing.
fn is_search_box(name: &str) -> bool {
    let n = fold_word_text(name);
    ["szukaj", "wyszukaj", "znajdz", "search", "find"].iter().any(|w| n.contains(w))
}

/// Key combos the agent never presses.
pub fn refused_combo(keys: &crate::platform::input::Combo) -> Option<&'static str> {
    let win = keys.modifiers.contains(&Key::Win);
    match keys.key {
        Key::Char('R') if win => Some("Win+R (okno Uruchamianie)"),
        Key::Char('X') if win => Some("Win+X (menu systemowe)"),
        Key::Char('L') if win => Some("Win+L (blokada)"),
        Key::Delete if keys.modifiers.contains(&Key::Ctrl) && keys.modifiers.contains(&Key::Alt) => Some("Ctrl+Alt+Del"),
        Key::Escape if keys.modifiers.contains(&Key::Ctrl) && keys.modifiers.contains(&Key::Shift) => {
            Some("Ctrl+Shift+Esc (Menedżer zadań)")
        }
        _ => None,
    }
}

fn element<'a>(s: &'a UiSnapshot, t: &Target) -> Option<&'a UiElement> {
    match t {
        Target::Id(id) => s.get(*id),
        Target::Point { .. } => None,
    }
}

/// Verdict for one decided step. `focused` = what has keyboard focus now (typing without a
/// target goes there).
pub fn check_step(d: &Decision, s: &UiSnapshot, level: ConfirmLevel) -> Verdict {
    let w = &s.window;
    let focused = s.focused();
    // ── Refuse ──
    match &d.action {
        Action::TypeText { target, text, .. } => {
            let into = target.as_ref().and_then(|t| element(s, t)).or(focused);
            if into.is_some_and(|e| e.password) {
                return Verdict::Refuse("To pole hasła — nie wpisuję haseł. Zrób to sam(a).".into());
            }
            if looks_like_secret(text) {
                return Verdict::Refuse("To wygląda na hasło albo klucz — nie wpisuję sekretów.".into());
            }
        }
        Action::ReadText(id) => {
            if s.get(*id).is_some_and(|e| e.password) {
                return Verdict::Refuse("Nie czytam pól z hasłem.".into());
            }
        }
        Action::KeyCombo(c) => {
            if let Some(what) = refused_combo(c) {
                return Verdict::Refuse(format!("{what} jest zablokowane dla agenta."));
            }
        }
        _ => {}
    }

    // ── Confirm ──
    let mut reason: Option<String> = None;
    let target_el = d.action.targets().first().and_then(|t| element(s, t));
    match &d.action {
        Action::Click { button, .. } if *button != crate::platform::input::MouseButton::Right => {
            if let Some(e) = target_el {
                if let Some(why) = has_risky_word(&format!("{} {}", e.name, e.automation_id)) {
                    reason = Some(why.to_string());
                } else if is_system_settings(w) && e.role.changes_state() {
                    reason = Some(format!("zmiana ustawienia: {}", clip(&e.display_name(), 40)));
                } else if is_system_settings(w) && e.role == Role::ListItem && s.elements.iter().any(|x| x.role == Role::ComboBox && x.expanded == Some(true)) {
                    reason = Some(format!("wybór: {}", clip(&e.display_name(), 40)));
                }
            }
        }
        Action::TypeText { target, submit, clear, .. } => {
            let into = target.as_ref().and_then(|t| element(s, t)).or(focused);
            if *submit && (is_messenger(w) || into.is_some_and(|e| has_risky_word(&e.name).is_some())) {
                reason = Some("wysłanie wiadomości (Enter)".into());
            } else if *clear && into.is_some_and(|e| e.role == Role::Document && e.value.as_deref().is_some_and(|v| v.trim().chars().count() > 20)) {
                reason = Some("zastąpienie całej treści dokumentu".into());
            } else if is_system_settings(w) && !into.is_some_and(|e| is_search_box(&e.name)) {
                reason = Some("zmiana wartości w ustawieniach systemu".into());
            }
        }
        Action::KeyCombo(c) => {
            let ctrl = c.modifiers.contains(&Key::Ctrl);
            let label = c.label();
            if c.key == Key::Enter && (ctrl || is_messenger(w)) {
                reason = Some(format!("{label} (wysłanie)"));
            } else if (c.key == Key::F(4) && c.modifiers.contains(&Key::Alt))
                || (ctrl && matches!(c.key, Key::Char('W') | Key::Char('Q')))
            {
                reason = Some(format!("{label} (zamknięcie okna)"));
            } else if c.key == Key::Delete {
                reason = Some(format!("{label} (usuwanie)"));
            } else if c.key == Key::Enter {
                if let Some(e) = focused.filter(|e| has_risky_word(&e.name).is_some()) {
                    reason = Some(format!("Enter na {}", clip(&e.display_name(), 40)));
                }
            }
        }
        Action::Drag { .. } if is_system_settings(w) => reason = Some("przeciąganie w ustawieniach systemu".into()),
        _ => {}
    }
    if reason.is_none() && d.risky && d.action.acts() {
        reason = Some("model oznaczył to jako ryzykowne".into());
    }
    if reason.is_none() && level == ConfirmLevel::Always && d.action.acts() && !matches!(d.action, Action::Scroll { .. }) {
        reason = Some("potwierdzanie każdej akcji (ustawienie)".into());
    }
    match reason {
        Some(r) => Verdict::Confirm(r),
        None => Verdict::Allow,
    }
}

/// Fullscreen game / presentation in front: refuse unless the task names that app.
pub fn fullscreen_guard(w: &WindowInfo, task: &str) -> Verdict {
    if !w.fullscreen {
        return Verdict::Allow;
    }
    let task = fold_word_text(task);
    let stem = w.process.to_lowercase();
    let stem = stem.trim_end_matches(".exe");
    let title_words: Vec<String> = w.title.split_whitespace().map(fold_word).filter(|x| x.len() >= 3).collect();
    let named = (!stem.is_empty() && stem.len() >= 3 && task.contains(stem)) || title_words.iter().any(|t| task.split(' ').any(|x| x == t));
    if named {
        Verdict::Allow
    } else {
        Verdict::Refuse(format!(
            "Na pierwszym planie jest aplikacja pełnoekranowa ({}). Wstrzymuję się — powiedz jej nazwę, jeśli to celowe.",
            clip(if w.title.trim().is_empty() { &w.process } else { &w.title }, 40)
        ))
    }
}

pub fn default_blocklist() -> Vec<String> {
    DEFAULT_BLOCKLIST.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::computer::action::parse;
    use crate::platform::ui::{finalize, Rect, Source, MAX_ELEMENTS};
    use serde_json::json;

    fn win(process: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id: 1,
            title: title.into(),
            process: process.into(),
            rect: Rect::new(0, 0, 1000, 800),
            ..Default::default()
        }
    }

    fn snap(w: WindowInfo, els: Vec<UiElement>) -> UiSnapshot {
        let (elements, truncated) = finalize(els, &w.rect, MAX_ELEMENTS);
        UiSnapshot { gen: 1, window: w, elements, source: Source::Uia, took_ms: 0, truncated }
    }

    fn el(role: Role, name: &str, y: i32) -> UiElement {
        UiElement::new(role, name, Rect::new(10, y, 100, 20))
    }

    fn d(name: &str, args: serde_json::Value) -> Decision {
        parse(name, &args).unwrap()
    }

    #[test]
    fn send_buy_delete_buttons_force_confirm_even_unflagged() {
        for name in ["Wyślij", "Send", "Kup teraz", "Zapłać 49,99 zł", "Usuń", "Delete", "Submit", "Prześlij", "Opublikuj",
            "Zamów i zapłać", "Nie zapisuj", "Don't save", "Odinstaluj", "Wyloguj się", "Przenieś do kosza"]
        {
            let s = snap(win("chrome.exe", "Sklep"), vec![el(Role::Button, name, 10)]);
            let v = check_step(&d("click", json!({ "id": 1 })), &s, ConfirmLevel::Risky);
            assert!(matches!(v, Verdict::Confirm(_)), "{name}: {v:?}");
        }
    }

    #[test]
    fn harmless_buttons_are_allowed() {
        for name in ["Zapisz", "Save", "Anuluj", "Dalej", "Postęp", "Ordered list", "Wyczyszczone filtry? nie", "Plik", "Szukaj", "OK"] {
            let s = snap(win("notepad.exe", "Notatnik"), vec![el(Role::Button, name, 10)]);
            let v = check_step(&d("click", json!({ "id": 1 })), &s, ConfirmLevel::Risky);
            // "Wyczyszczone…" starts with "wyczysc"? folded "wyczyszczone" doesn't start with "wyczysc" → allowed.
            assert_eq!(v, Verdict::Allow, "{name}");
        }
    }

    #[test]
    fn model_flag_and_always_level() {
        let s = snap(win("notepad.exe", "Notatnik"), vec![el(Role::Button, "Zapisz", 10)]);
        assert!(matches!(check_step(&d("click", json!({ "id": 1, "risky": true })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)));
        assert!(matches!(check_step(&d("click", json!({ "id": 1 })), &s, ConfirmLevel::Always), Verdict::Confirm(_)));
        // Reading / scrolling never asks.
        assert_eq!(check_step(&d("scroll", json!({ "direction": "down" })), &s, ConfirmLevel::Always), Verdict::Allow);
        assert_eq!(check_step(&d("read_text", json!({ "id": 1 })), &s, ConfirmLevel::Always), Verdict::Allow);
    }

    #[test]
    fn enter_in_a_messenger_confirms() {
        let mut input = el(Role::Edit, "Wyślij wiadomość do @Silver", 700);
        input.focused = true;
        let s = snap(win("Discord.exe", "#ogólny - Discord"), vec![input]);
        assert!(matches!(
            check_step(&d("type_text", json!({ "id": 1, "text": "będę za 10 minut", "submit": true })), &s, ConfirmLevel::Risky),
            Verdict::Confirm(_)
        ));
        assert!(matches!(check_step(&d("key_combo", json!({ "keys": "enter" })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)));
        // Typing without Enter is fine.
        assert_eq!(check_step(&d("type_text", json!({ "id": 1, "text": "będę za 10 minut" })), &s, ConfirmLevel::Risky), Verdict::Allow);
        // Enter in a search box elsewhere is fine.
        let mut search = el(Role::Edit, "Szukaj", 10);
        search.focused = true;
        let s = snap(win("chrome.exe", "Google"), vec![search]);
        assert_eq!(check_step(&d("type_text", json!({ "id": 1, "text": "FiveM serwery", "submit": true })), &s, ConfirmLevel::Risky), Verdict::Allow);
    }

    #[test]
    fn passwords_and_secrets_are_refused() {
        let mut pw = el(Role::Edit, "Hasło", 10);
        pw.password = true;
        pw.focused = true;
        let s = snap(win("chrome.exe", "Logowanie"), vec![pw]);
        assert!(matches!(check_step(&d("type_text", json!({ "id": 1, "text": "abc" })), &s, ConfirmLevel::Risky), Verdict::Refuse(_)));
        // Focused password field, no explicit target.
        assert!(matches!(check_step(&d("type_text", json!({ "text": "abc" })), &s, ConfirmLevel::Risky), Verdict::Refuse(_)));
        assert!(matches!(check_step(&d("read_text", json!({ "id": 1 })), &s, ConfirmLevel::Risky), Verdict::Refuse(_)));
        let s = snap(win("notepad.exe", "Notatnik"), vec![el(Role::Document, "Edytor", 10)]);
        for secret in ["sk-proj-AbC123dEf456GhI789jKl", "password=hunter2", "AIzaSyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"] {
            assert!(matches!(check_step(&d("type_text", json!({ "id": 1, "text": secret })), &s, ConfirmLevel::Risky), Verdict::Refuse(_)), "{secret}");
        }
        assert_eq!(check_step(&d("type_text", json!({ "id": 1, "text": "Zażółć gęślą jaźń" })), &s, ConfirmLevel::Risky), Verdict::Allow);
        // Long single tokens that are not secrets.
        for ok in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            r"C:\Users\Oskar\AppData\Local\Temp\soul-agent-Test42.txt",
            "silver.gamer99@gmail.com",
            "zmień hasło do wifi",
        ] {
            assert!(!looks_like_secret(ok), "{ok}");
        }
    }

    #[test]
    fn refused_combos() {
        let s = snap(win("explorer.exe", "Pulpit"), vec![]);
        for k in ["win+r", "Win+X", "ctrl+alt+delete", "ctrl+shift+esc", "win+l"] {
            assert!(matches!(check_step(&d("key_combo", json!({ "keys": k })), &s, ConfirmLevel::Risky), Verdict::Refuse(_)), "{k}");
        }
        for k in ["alt+f4", "ctrl+w", "delete", "ctrl+enter"] {
            assert!(matches!(check_step(&d("key_combo", json!({ "keys": k })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)), "{k}");
        }
        for k in ["ctrl+s", "ctrl+f", "tab", "pagedown", "ctrl+l"] {
            assert_eq!(check_step(&d("key_combo", json!({ "keys": k })), &s, ConfirmLevel::Risky), Verdict::Allow, "{k}");
        }
    }

    #[test]
    fn blocklist_windows_and_bank_urls() {
        let bl = default_blocklist();
        for (p, t) in [
            ("KeePassXC.exe", "Baza.kdbx - KeePassXC"),
            ("1Password.exe", "1Password"),
            ("chrome.exe", "Bankowość internetowa PKO - Google Chrome"),
            ("chrome.exe", "mBank - logowanie - Google Chrome"),
            ("SecHealthUI.exe", "Zabezpieczenia Windows"),
            ("cmd.exe", "Wiersz polecenia"),
            ("WindowsTerminal.exe", "PowerShell"),
            ("explorer.exe", "Uruchamianie"),
            ("powershell.exe", "x"),
        ] {
            assert!(blocked_by(&win(p, t), None, &bl).is_some(), "{p} {t}");
        }
        // Terminals stay blocked even with an empty user blocklist.
        assert!(blocked_by(&win("cmd.exe", "x"), None, &[]).is_some());
        // Address bar on a bank domain.
        let mut bar = el(Role::Edit, "Pasek adresu i wyszukiwania", 5);
        bar.value = Some("https://online.mbank.pl/pl/Login".into());
        let s = snap(win("chrome.exe", "Logowanie"), vec![bar]);
        assert!(matches!(check_window(&s, &bl), Verdict::Refuse(_)));
        // Ordinary windows.
        for (p, t) in [
            ("notepad.exe", "Bez tytułu — Notatnik"),
            ("chrome.exe", "FiveM serwery - Google Szukaj"),
            ("Discord.exe", "Discord"),
            ("SystemSettings.exe", "Ustawienia"),
            ("WINWORD.EXE", "Bankietowa sala.docx - Word"),
            ("chrome.exe", "Booking.com"),
        ] {
            assert!(blocked_by(&win(p, t), None, &bl).is_none(), "{p} {t}");
        }
    }

    #[test]
    fn system_settings_changes_confirm_navigation_does_not() {
        let w = win("SystemSettings.exe", "Ustawienia");
        let mut combo = el(Role::ComboBox, "Wybierz urządzenie wejściowe", 300);
        combo.expanded = Some(true);
        let s = snap(w.clone(), vec![el(Role::ListItem, "Dźwięk", 100), combo, el(Role::ListItem, "Voicemeeter Out B1", 340)]);
        // Navigation item in the side list while a combo is open → still a selection; picking the
        // device in the open drop-down → confirm.
        assert!(matches!(check_step(&d("click", json!({ "id": 3 })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)));
        let s = snap(w, vec![el(Role::ListItem, "Dźwięk", 100), el(Role::CheckBox, "Mono audio", 200)]);
        assert_eq!(check_step(&d("click", json!({ "id": 1 })), &s, ConfirmLevel::Risky), Verdict::Allow);
        assert!(matches!(check_step(&d("click", json!({ "id": 2 })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)));
    }

    #[test]
    fn fullscreen_game_pauses_unless_named() {
        let mut w = win("GTA5.exe", "Grand Theft Auto V");
        w.fullscreen = true;
        assert!(matches!(fullscreen_guard(&w, "kliknij Zapisz"), Verdict::Refuse(_)));
        assert_eq!(fullscreen_guard(&w, "w GTA5 kliknij Graj"), Verdict::Allow);
        assert_eq!(fullscreen_guard(&w, "w grand theft auto kliknij menu"), Verdict::Allow);
        w.fullscreen = false;
        assert_eq!(fullscreen_guard(&w, "kliknij Zapisz"), Verdict::Allow);
    }

    #[test]
    fn document_wipe_confirms() {
        let mut doc = el(Role::Document, "Edytor tekstów", 100);
        doc.value = Some("Bardzo ważny tekst, którego nie chcę stracić.".into());
        let s = snap(win("notepad.exe", "notatki.txt — Notatnik"), vec![doc]);
        assert!(matches!(check_step(&d("type_text", json!({ "id": 1, "text": "x", "clear": true })), &s, ConfirmLevel::Risky), Verdict::Confirm(_)));
        assert_eq!(check_step(&d("type_text", json!({ "id": 1, "text": "x" })), &s, ConfirmLevel::Risky), Verdict::Allow);
    }
}
