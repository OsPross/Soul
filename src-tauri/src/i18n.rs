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
}
