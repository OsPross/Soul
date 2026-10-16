//! Rust-side UI strings. The UI language lives in the opaque `appearance` setting (`uiLang`,
//! owned by the frontend — `src/i18n.ts`); Polish is the default, like there.

use crate::config::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Pl,
    En,
}

pub fn ui_lang(settings: &Settings) -> Lang {
    match settings.appearance.get("uiLang").and_then(|v| v.as_str()) {
        Some("en") => Lang::En,
        _ => Lang::Pl,
    }
}

/// Pick the Polish or English variant: `tr(lang, format!("Zamknąć {x}?"), format!("Close {x}?"))`.
pub fn tr<T>(lang: Lang, pl: T, en: T) -> T {
    match lang {
        Lang::Pl => pl,
        Lang::En => en,
    }
}

/// A Live transcript that is clearly not Polish (the commands model sometimes hears Slovak,
/// Spanish, Italian or French): ≥ 2 foreign signals (letters Polish lacks, foreign function
/// words) and nothing Polish. English passes (no foreign letters, no listed words).
pub fn clearly_not_polish(text: &str) -> bool {
    const PL: &str = "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ";
    const PL_WORDS: &[&str] = &[
        "się", "mi", "nie", "jest", "jak", "czy", "otwórz", "otworz", "włącz", "wlacz", "zamknij", "puść", "pusc",
        "proszę", "prosze", "ścisz", "scisz", "daj", "zrób", "zrob", "głośniej", "glosniej",
    ];
    const FOREIGN_WORDS: &[&str] = &[
        "el", "los", "las", "que", "por", "para", "una", "del", "abre", "pon", "il", "che", "della", "apri", "metti",
        "per", "gli", "le", "les", "des", "est", "pour", "ouvre", "mets", "je", "du", "otvor", "zapni", "prosím",
    ];
    let (mut foreign, mut polish) = (0, 0);
    for w in text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()) {
        let lower = w.to_lowercase();
        if w.chars().any(|c| PL.contains(c)) || PL_WORDS.contains(&lower.as_str()) {
            polish += 1;
        } else if !w.is_ascii() || FOREIGN_WORDS.contains(&lower.as_str()) {
            foreign += 1;
        }
    }
    foreign >= 2 && polish == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_unless_english_is_chosen() {
        let mut s = Settings::default();
        assert_eq!(ui_lang(&s), Lang::Pl);
        s.appearance = serde_json::json!({ "uiLang": "en" });
        assert_eq!(ui_lang(&s), Lang::En);
        s.appearance = serde_json::json!({ "uiLang": "de" });
        assert_eq!(ui_lang(&s), Lang::Pl);
        assert_eq!(tr(Lang::Pl, "Zamknąć Discord?", "Close Discord?"), "Zamknąć Discord?");
    }

    #[test]
    fn foreign_transcripts_are_detected() {
        for t in ["Pon la música de Queen", "Zvýš hlasitosť", "Apri il browser per favore", "Ouvre les paramètres"] {
            assert!(clearly_not_polish(t), "{t}");
        }
        for t in ["otwórz Spotify", "ścisz o 20", "open YouTube", "puść coś z Queen", "zamknij café nero", "włącz muzykę", ""] {
            assert!(!clearly_not_polish(t), "{t}");
        }
    }
}
