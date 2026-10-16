//! `computer_task` routing: which commands go to the computer agent, and the deterministic local
//! plans that finish trivial tasks without a model call („kliknij Zapisz” with exactly one match,
//! „przewiń w dół”).
//!
//! Runs in `local_dispatch` before chains and the pre-router: „kliknij Dalej” must click the
//! button, not skip the track. Simple commands keep the fast path — media, volume, timers, plain
//! „otwórz / zamknij X” are never computer tasks.

use super::action::{Action, Decision, Target};
use crate::agent::fold_word;
use crate::agent::router;
use crate::platform::ui::{Role, ScrollDir, UiSnapshot};

/// Verbs that always mean "do something in the UI" (folded).
const UI_VERBS: &[&str] = &[
    "kliknij", "kliknac", "klikni", "kliknijcie", "dwukrotnie", "dwuklik", "przewin", "przewinac", "zjedz", "wpisz",
    "wpisac", "wprowadz", "zaznacz", "odznacz", "wypelnij", "wypelnic", "nacisnij", "nacisnac", "wcisnij", "wybierz",
    "rozwin", "przeciagnij", "click", "doubleclick", "double", "rightclick", "right", "scroll", "fill", "tap",
    "press", "select", "check", "uncheck", "drag",
];

/// Openers that may precede the verb in a follow-up („a teraz kliknij drugi”).
const SKIP_WORDS: &[&str] = &[
    "a", "i", "teraz", "jeszcze", "potem", "pozniej", "tez", "takze", "to", "now", "then", "also", "and", "next",
];

const MESSAGE_VERBS: &[&str] = &["wyslij", "wyslac", "napisz", "napisac", "odpisz", "send", "message", "text", "write", "reply"];

const MESSENGERS: &[&str] = &[
    "discord", "discordzie", "discorda", "messenger", "messengerze", "messengera", "whatsapp", "whatsappie",
    "whatsappa", "slack", "slacku", "slacka", "teams", "teamsie", "teamsach", "telegram", "telegramie", "telegrama",
    "signal", "signalu", "mail", "mailem", "maila", "email", "emailem", "gmail", "gmailu", "outlook", "outlooku",
    "instagram", "instagramie", "sms", "smsem",
];

/// Things that live inside an app (not an app name): „przejdź do ustawień prywatności”.
const IN_APP_NOUNS: &[&str] = &[
    "ustawien", "ustawienia", "zakladki", "zakladka", "karty", "strony", "sekcji", "menu", "pola", "kanalu",
    "folderu", "katalogu", "nastepnego", "poprzedniego", "konca", "poczatku", "settings", "tab", "page", "section",
    "field", "channel", "folder",
];

/// Settings vocabulary: „ustaw mikrofon na Voicemeeter”, „zmień tapetę”.
const SETTINGS_NOUNS: &[&str] = &[
    "mikrofon", "mikrofonu", "glosnik", "glosniki", "wyjscie", "wejscie", "urzadzenie", "rozdzielczosc",
    "rozdzielczosci", "jasnosc", "jasnosci", "tapete", "tapety", "tlo", "motyw", "bluetooth", "wifi", "siec",
    "drukarke", "microphone", "speaker", "speakers", "output", "input", "device", "resolution", "brightness",
    "wallpaper", "theme", "printer",
];

fn words_after_fillers(text: &str) -> Vec<String> {
    let folded: Vec<String> = text.split_whitespace().map(fold_word).filter(|w| !w.is_empty()).collect();
    let (mut skip, _) = router::strip_filler(&folded);
    while skip < folded.len().saturating_sub(1) && SKIP_WORDS.contains(&folded[skip].as_str()) {
        skip += 1;
    }
    folded[skip..].to_vec()
}

/// Should this command be handled by the computer agent?
pub fn is_computer_task(text: &str) -> bool {
    // A browser command Vio does directly („przewiń do sekcji X”, „znajdź na stronie X”).
    if crate::agent::browser::route(text).is_some() {
        return false;
    }
    let words = words_after_fillers(text);
    let Some(first) = words.first() else {
        return false;
    };
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    let joined = words.join(" ");

    // 1. A UI verb up front.
    if UI_VERBS.contains(&first.as_str()) {
        // „przewiń piosenkę / utwór” is media seeking, not the UI.
        if matches!(first.as_str(), "przewin" | "przewinac") && has(&["piosenke", "utwor", "muzyke", "kawalek"]) {
            return false;
        }
        // English "right"/"double"/"check"/"select"/"press" only as UI phrases.
        return match first.as_str() {
            "right" | "double" => words.get(1).is_some_and(|w| w.starts_with("click")),
            "check" | "select" | "press" | "tap" => words.len() >= 2 && !joined.contains("check the weather"),
            _ => true,
        };
    }
    // 2. „przejdź do …” / "go to …" inside an app (one bare name = focus that app, the fast path).
    let goto = matches!(first.as_str(), "przejdz" | "przejsc" | "idz") && words.get(1).is_some_and(|w| w == "do" || w == "na")
        || (first == "go" && words.get(1).is_some_and(|w| w == "to"));
    if goto {
        if router::match_command(text).is_some() {
            return false;
        }
        let tail = &words[2..];
        return tail.len() >= 2 || tail.iter().any(|w| IN_APP_NOUNS.contains(&w.as_str()));
    }
    // 3. Sending a message through an app.
    if has(MESSAGE_VERBS) && has(MESSENGERS) {
        return true;
    }
    // 4. Settings.
    if joined.contains("w ustawieniach") || joined.contains("in settings") || joined.contains("in the settings") {
        return true;
    }
    let settings_verb = matches!(first.as_str(), "ustaw" | "zmien" | "przelacz" | "set" | "change" | "switch");
    if settings_verb && has(SETTINGS_NOUNS) && router::match_command(text).is_none() {
        return true;
    }
    // „otwórz ustawienia dźwięku (i …)” — a settings page, not just the app.
    let open = matches!(first.as_str(), "otworz" | "odpal" | "uruchom" | "pokaz" | "open" | "show");
    if open && words.get(1).is_some_and(|w| w == "ustawienia" || w == "settings") && words.len() >= 3 {
        return true;
    }
    // 5. About something on the screen: „sprawdź, które pola w formularzu są puste” (not a web
    // search unless it says so: „wyszukaj, jak wypełnić pole w formularzu PIT”).
    let on_screen = SCREEN_PLACES.iter().any(|p| format!(" {joined} ").contains(&format!(" {p} ")));
    if on_screen && !matches!(first.as_str(), "wyszukaj" | "wygoogluj" | "poszukaj" | "search" | "google") {
        return true;
    }
    if super::vio::connected() && is_browser_phrase(text) {
        return true;
    }
    picks_listed_item(&words) || is_screen_question(&words)
}

/// „…i włącz drugi wynik”, „kliknij trzeci link”, „puść pierwszy film”: a pick verb right before an
/// ordinal + item on the page (not „wyszukaj pierwszy film Nolana” — a search).
fn picks_listed_item(words: &[String]) -> bool {
    const PICK: &[&str] = &["wlacz", "kliknij", "otworz", "pusc", "wybierz", "odpal", "zagraj", "play", "open", "click"];
    words.iter().enumerate().any(|(i, w)| {
        let next = &words[i + 1..words.len().min(i + 4)];
        PICK.contains(&w.as_str())
            && next.len() >= 2
            && router::names_screen_item(&next[..1])
            && router::names_screen_item(&next[1..])
    })
}

/// Words naming the browser itself (folded): with Vio connected such a task goes to Vio even when
/// another window is in front.
const BROWSER_WORDS: &[&str] = &[
    "przegladarce", "przegladarke", "przegladarka", "przegladarki", "vio", "karte", "karcie", "karty", "browser",
    "tab", "tabs",
];

pub fn names_browser(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric()).map(fold_word).any(|w| BROWSER_WORDS.contains(&w.as_str()))
}

/// Verbs about a page / form, and the page / form nouns they need.
const PAGE_VERBS: &[&str] = &[
    "przeczytaj", "stresc", "streszcz", "podsumuj", "sprawdz", "wypelnij", "read", "summarize", "summarise", "check",
    "fill",
];
const PAGE_NOUNS: &[&str] = &["strone", "stronie", "strony", "formularz", "formularzu", "pola", "pole", "page", "form", "fields"];

/// A page noun, or „ten / tego / tym artykuł” (a bare „artykuł” may be dictated: „streść artykuł do jutra”).
fn names_page(words: &[String]) -> bool {
    words.iter().any(|w| PAGE_NOUNS.contains(&w.as_str()))
        || words.windows(2).any(|p| {
            matches!(p[0].as_str(), "ten" | "tego" | "tym" | "this") && matches!(p[1].as_str(), "artykul" | "artykulu" | "artykule" | "article")
        })
}

/// Browser phrases routed to the agent while Vio is connected: „otwórz w przeglądarce X”,
/// „przeczytaj / streść tę stronę”, „wypełnij formularz”, „sprawdź które pola są puste”.
pub fn is_browser_phrase(text: &str) -> bool {
    let words = words_after_fillers(text);
    let Some(first) = words.first() else { return false };
    // „otwórz przeglądarkę” alone is the app (router), not a browser task.
    (names_browser(text) && words.len() >= 3) || (PAGE_VERBS.contains(&first.as_str()) && names_page(&words))
}

/// „przeczytaj / streść / podsumuj tę stronę”: answered from the page text, no clicking.
pub fn is_read_page_task(text: &str) -> bool {
    let words = words_after_fillers(text);
    words.first().is_some_and(|w| matches!(w.as_str(), "przeczytaj" | "stresc" | "streszcz" | "podsumuj" | "read" | "summarize" | "summarise"))
        && names_page(&words)
}

/// The user points at what is in front of them („na ekranie”, „na tej stronie”, „w formularzu”).
pub fn refers_to_screen(text: &str) -> bool {
    let words = words_after_fillers(text);
    let joined = format!(" {} ", words.join(" "));
    is_screen_question(&words) || SCREEN_PLACES.iter().any(|p| joined.contains(&format!(" {p} ")))
}

/// Places on the screen (folded, locative): the command is about the UI in front of the user.
const SCREEN_PLACES: &[&str] = &[
    "w formularzu", "na ekranie", "na tej stronie", "w tym oknie", "w tym polu", "w polu", "w tym artykule",
    "wedlug tego artykulu", "z tego artykulu", "wedlug tej strony", "on the screen", "on my screen", "on this page",
    "in the form", "in this window", "in this article",
];

/// „co jest na ekranie?”, „co widzisz?”, „przeczytaj, co jest na ekranie”, "what's on my screen".
/// („ekranu” is left out on purpose: „jasność ekranu”, „zrzut ekranu” are not about its content.)
fn is_screen_question(words: &[String]) -> bool {
    let Some(first) = words.first() else { return false };
    let asks = matches!(
        first.as_str(),
        "co" | "czy" | "przeczytaj" | "opisz" | "powiedz" | "what" | "whats" | "read" | "describe" | "tell"
    );
    let on_screen = words.iter().any(|w| matches!(w.as_str(), "ekranie" | "ekran" | "monitorze" | "screen"));
    asks && (on_screen || words.iter().any(|w| w == "widzisz"))
}

/// Messenger apps named outright (generic „mail” / „SMS” stay dictation in Smart routing).
const NAMED_MESSENGERS: &[&str] = &[
    "discord", "discordzie", "discorda", "messenger", "messengerze", "messengera", "whatsapp", "whatsappie",
    "whatsappa", "slack", "slacku", "slacka", "teams", "teamsie", "teamsach", "telegram", "telegramie", "telegrama",
    "signal", "signalu", "instagram", "instagramie",
];

/// Smart talk key: a dictated sentence must never turn into UI control by accident. Only an
/// imperative UI verb at the very start (≤ 16 words), a send verb up front with a named messenger
/// app, or the explicit settings / „przejdź do …” phrases count. Everything else is dictated (the
/// wake word or command mode still reach the agent through `is_computer_task`).
pub fn is_explicit_ui_command(text: &str) -> bool {
    let words = words_after_fillers(text);
    // „Wypełnij formularz: imię …, telefon …, e-mail …” lists every field: no length cap.
    let form_fill = words.len() >= 2 && words[0] == "wypelnij" && words[1].starts_with("formularz");
    if words.is_empty() || (words.len() > 16 && !form_fill) {
        return false;
    }
    let first = words[0].as_str();
    const STRONG: &[&str] = &[
        "kliknij", "przewin", "wpisz", "zaznacz", "odznacz", "wypelnij", "nacisnij", "wcisnij", "click", "scroll",
        "doubleclick", "rightclick",
    ];
    let english_click = matches!(first, "double" | "right") && words.get(1).is_some_and(|w| w.starts_with("click"));
    // „Wybierz sos ostry” (a short pick), not „wybierz się do kina…”.
    let pick = first == "wybierz" && words.len() <= 6 && words.get(1).is_some_and(|w| w != "sie");
    if STRONG.contains(&first) || english_click || pick {
        return is_computer_task(text);
    }
    if MESSAGE_VERBS.contains(&first) && words.iter().any(|w| NAMED_MESSENGERS.contains(&w.as_str())) {
        return true;
    }
    if is_screen_question(&words) && words.len() <= 8 {
        return true;
    }
    if words.len() <= 8 && super::vio::connected() && is_browser_phrase(text) {
        return true;
    }
    matches!(first, "przejdz" | "ustaw" | "zmien" | "otworz" | "w" | "go" | "open" | "set" | "change") && is_computer_task(text)
}

/// Click words for the local plan.
const CLICK_VERBS: &[&str] = &["kliknij", "nacisnij", "wcisnij", "click", "press"];
/// Words between the verb and the name: „kliknij (w / na) (przycisk) Zapisz”.
const TARGET_FILLERS: &[&str] = &["w", "na", "przycisk", "przycisku", "button", "the", "on", "link", "opcje", "zakladke"];

/// Name for matching: folded words, without a trailing shortcut hint („Zapisz (Ctrl+S)”).
fn match_key(name: &str) -> String {
    let base = name.split(" (").next().unwrap_or(name);
    base.split_whitespace().map(fold_word).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Trivial tasks decided without the model (0 API calls). Only on the first step.
pub fn local_plan(task: &str, snap: &UiSnapshot) -> Option<Decision> {
    let words = words_after_fillers(task);
    let first = words.first()?.as_str();
    if CLICK_VERBS.contains(&first) {
        let mut rest = &words[1..];
        while rest.first().is_some_and(|w| TARGET_FILLERS.contains(&w.as_str())) {
            rest = &rest[1..];
        }
        if rest.is_empty() || rest.len() > 5 {
            return None;
        }
        let want = rest.join(" ");
        let matches: Vec<_> = snap
            .elements
            .iter()
            .filter(|e| e.enabled && !e.password && (!e.role.is_passive() || e.caps.invoke) && e.role != Role::Document)
            .filter(|e| !e.name.trim().is_empty() && match_key(&e.name) == want)
            .collect();
        if matches.len() != 1 {
            return None;
        }
        return Some(Decision {
            action: Action::Click {
                target: Target::Id(matches[0].id),
                button: crate::platform::input::MouseButton::Left,
                count: 1,
            },
            risky: false,
            final_step: true,
            look: false,
            name: "click".into(),
        });
    }
    if matches!(first, "przewin" | "zjedz" | "scroll") {
        let rest = &words[1..];
        let dir_words = ["w", "dol", "dolu", "gore", "gory", "lewo", "prawo", "na", "down", "up", "left", "right"];
        let amount_words = ["troche", "lekko", "duzo", "bardzo", "mocno", "maksymalnie", "a", "bit", "lot", "little", "strone", "jedna"];
        if rest.is_empty() || !rest.iter().all(|w| dir_words.contains(&w.as_str()) || amount_words.contains(&w.as_str())) {
            return None;
        }
        let has = |set: &[&str]| rest.iter().any(|w| set.contains(&w.as_str()));
        let dir = if has(&["dol", "dolu", "down"]) {
            ScrollDir::Down
        } else if has(&["gore", "gory", "up"]) {
            ScrollDir::Up
        } else if has(&["lewo", "left"]) {
            ScrollDir::Left
        } else if has(&["prawo", "right"]) {
            ScrollDir::Right
        } else {
            return None;
        };
        let amount = if has(&["troche", "lekko", "bit", "little"]) {
            3
        } else if has(&["duzo", "bardzo", "mocno", "maksymalnie", "lot"]) {
            10
        } else {
            5
        };
        return Some(Decision {
            action: Action::Scroll { target: None, dir, amount },
            risky: false,
            final_step: true,
            look: false,
            name: "scroll".into(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ui::{finalize, Rect, Source, UiElement, WindowInfo, MAX_ELEMENTS};

    #[test]
    fn browser_phrases() {
        for t in [
            "otwórz w przeglądarce youtube.com",
            "przeczytaj tę stronę",
            "streść tę stronę",
            "podsumuj tę stronę",
            "wypełnij formularz",
            "sprawdź które pola są puste",
            "sprawdź, które pola są jeszcze puste",
            "otwórz nową kartę z pogodą",
            "read this page",
            "summarize this page",
        ] {
            assert!(is_browser_phrase(t), "{t}");
        }
        for t in ["otwórz przeglądarkę", "przeczytaj wiadomość", "streść to", "otwórz spotify", "sprawdź pogodę", "zapłać kartą"] {
            assert!(!is_browser_phrase(t), "{t}");
        }
        assert!(names_browser("otwórz w przeglądarce youtube") && names_browser("w Vio kliknij Zapisz"));
        assert!(!names_browser("sprawdź które pola są puste") && !names_browser("vivaldi"));
    }

    #[test]
    fn ui_tasks_route_to_the_agent() {
        for t in [
            "kliknij Zapisz",
            "Kliknij w przycisk Dalej",
            "kliknij dalej",
            "kliknij cofnij",
            "przewiń w dół",
            "Przewiń trochę w górę.",
            "wpisz w wyszukiwarkę FiveM serwery",
            "Wpisz moje imię w pole Imię",
            "zaznacz wszystkie checkboxy",
            "odznacz newsletter",
            "wypełnij ten formularz moimi danymi",
            "naciśnij enter",
            "otwórz ustawienia dźwięku i ustaw mikrofon na Voicemeeter",
            "otwórz ustawienia dźwięku",
            "ustaw mikrofon na Voicemeeter",
            "zmień tapetę na ciemną",
            "w ustawieniach wyłącz Bluetooth",
            "wyślij Silverowi na Discordzie: będę za 10 minut",
            "napisz do mamy na Messengerze że będę później",
            "przejdź do ustawień prywatności",
            "przejdź do zakładki Historia",
            "a teraz kliknij drugi",
            "i jeszcze przewiń w dół",
            "weź kliknij Zapisz",
            "click Save",
            "double click the file",
            "right click the icon",
            "scroll down",
            "send Silver a message on Discord saying I'll be late",
            "go to the privacy settings",
            // Voice-harness agent_* cases.
            "Wpisz w wyszukiwarkę serwery FiveM",
            "Zaznacz newsletter w formularzu",
            "Co jest na ekranie?",
            "co widzisz?",
            "przeczytaj, co jest na ekranie",
            "what's on my screen?",
            "sprawdź, które pola w formularzu są jeszcze puste",
            "ile jest serwerów na tej stronie",
        ] {
            assert!(is_computer_task(t), "{t}");
        }
    }

    #[test]
    fn simple_commands_keep_the_fast_path() {
        for t in [
            "otwórz Spotify",
            "otwórz ustawienia",
            "zamknij Discorda",
            "następna piosenka",
            "dalej",
            "pauza",
            "ścisz o 20",
            "ustaw głośność na 30",
            "ustaw timer na 10 minut",
            "nastaw 10 minut na makaron",
            "przejdź do Discorda",
            "przewiń piosenkę do przodu",
            "wyszukaj przepis na pierogi",
            "puść na YouTube lo-fi",
            "zapamiętaj że gram na serwerze Polska RP",
            "co gra?",
            "wyślij mi przypomnienie jutro",
            "check the weather",
            "przejdź do następnej piosenki",
            "co słychać?",
            "zrób zrzut ekranu",
            "wyszukaj, co wpisać w polu NIP w formularzu PIT",
            "",
        ] {
            assert!(!is_computer_task(t), "{t}");
        }
    }

    #[test]
    fn smart_routing_sends_ui_commands_and_keeps_dictation() {
        use crate::agent::{classify_smart, Intent};
        let is_cmd = |t: &str| matches!(classify_smart(t, "Soul", &|| None, false), Intent::Command { .. });
        for t in [
            "kliknij Zapisz",
            "Przewiń w dół.",
            "wpisz w wyszukiwarkę FiveM serwery",
            "wyślij Silverowi na Discordzie: będę za 10 minut",
            "otwórz ustawienia dźwięku i ustaw mikrofon na Voicemeeter",
            "przejdź do ustawień prywatności",
            "wypełnij ten formularz moimi danymi",
            "Zaznacz newsletter w formularzu",
            "Co jest na ekranie?",
        ] {
            assert!(is_cmd(t), "{t}");
        }
        // Dictation stays dictation, including a mail-ish sentence without a named app.
        for t in [
            "Wyślij mi to na maila jutro rano.",
            "Napisz do mnie, jak dojedziesz.",
            "Wczoraj kliknąłem w ten link i nic się nie stało.",
            "Przewinąłem cały dokument i nie znalazłem błędu.",
            "Zaznaczyłem w kalendarzu nasze spotkanie.",
        ] {
            assert!(!is_cmd(t), "{t}");
        }
    }

    /// Every dictation case of the voice suite (`tests/voice/synth.json`, incl. all `pulapka_*`)
    /// must stay dictation with the agent verbs added.
    #[test]
    fn voice_suite_dictation_traps_stay_dictation() {
        use crate::agent::{classify_smart, Intent};
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/voice/synth.json");
        let raw = std::fs::read_to_string(&path).expect("tests/voice/synth.json");
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        let mut traps = 0;
        for c in &cases {
            if c["expect"]["route"] != "dictation" {
                continue;
            }
            let text = c["text"].as_str().unwrap();
            if c["id"].as_str().is_some_and(|id| id.starts_with("pulapka")) {
                traps += 1;
            }
            assert_eq!(classify_smart(text, "Soul", &|| None, false), Intent::Dictation, "{text}");
            assert!(!is_computer_task(text), "{text}");
        }
        assert!(traps >= 3, "expected the pulapka_* cases in synth.json");
        for t in ["Odwiedziłem wczoraj nowy sklep kolegi.", "Proszę o przesłanie raportu do piątku."] {
            assert_eq!(classify_smart(t, "Soul", &|| None, false), Intent::Dictation, "{t}");
        }
    }

    fn snap(els: Vec<UiElement>) -> UiSnapshot {
        let w = WindowInfo { rect: Rect::new(0, 0, 1000, 800), ..Default::default() };
        let (elements, truncated) = finalize(els, &w.rect, MAX_ELEMENTS);
        UiSnapshot { gen: 1, window: w, elements, source: Source::Uia, took_ms: 0, truncated }
    }

    fn btn(name: &str, x: i32) -> UiElement {
        let mut e = UiElement::new(Role::Button, name, Rect::new(x, 700, 80, 24));
        e.caps.invoke = true;
        e
    }

    #[test]
    fn local_click_plan_needs_exactly_one_match() {
        let s = snap(vec![btn("Zapisz (Ctrl+S)", 100), btn("Anuluj", 300), btn("Pomoc", 500), btn("Pomoc", 600)]);
        let d = local_plan("kliknij Zapisz", &s).unwrap();
        assert_eq!(d.action, Action::Click { target: Target::Id(1), button: crate::platform::input::MouseButton::Left, count: 1 });
        assert!(d.final_step);
        assert!(local_plan("Kliknij w przycisk anuluj.", &s).is_some());
        // Two „Pomoc” buttons → the model decides.
        assert!(local_plan("kliknij Pomoc", &s).is_none());
        // No exact match → the model decides.
        assert!(local_plan("kliknij Zapisz jako", &s).is_none());
        assert!(local_plan("kliknij", &s).is_none());
        // Disabled buttons are not picked.
        let mut off = btn("Dalej", 100);
        off.enabled = false;
        assert!(local_plan("kliknij Dalej", &snap(vec![off])).is_none());
    }

    #[test]
    fn local_scroll_plan() {
        let s = snap(vec![]);
        let d = local_plan("przewiń w dół", &s).unwrap();
        assert_eq!(d.action, Action::Scroll { target: None, dir: ScrollDir::Down, amount: 5 });
        assert_eq!(local_plan("przewiń trochę w górę", &s).unwrap().action, Action::Scroll { target: None, dir: ScrollDir::Up, amount: 3 });
        assert_eq!(local_plan("scroll down a lot", &s).unwrap().action, Action::Scroll { target: None, dir: ScrollDir::Down, amount: 10 });
        // A named target needs the model.
        assert!(local_plan("przewiń listę serwerów w dół", &s).is_none());
        assert!(local_plan("przewiń", &s).is_none());
    }

    #[test]
    fn read_page_tasks_and_screen_references() {
        assert!(is_read_page_task("streść mi tą stronę"));
        assert!(is_read_page_task("przeczytaj tę stronę"));
        assert!(!is_read_page_task("wypełnij formularz"));
        assert!(!is_read_page_task("streść artykuł"));
        assert!(is_read_page_task("streść mi ten artykuł") && is_read_page_task("Streszcz mi ten artykuł."));
        assert!(refers_to_screen("co jest na ekranie"));
        assert!(refers_to_screen("przeczytaj co jest na tej stronie"));
        assert!(!refers_to_screen("streść tę stronę"));
        assert!(is_browser_phrase("streść mi tą stronę"));
    }
}
