/**
 * Tiny i18n map. Polish is first-class; English is the fallback for any missing key.
 * UI language comes from the appearance config (`uiLang`), default Polish.
 */
export type UiLang = "pl" | "en";

const en = {
  // Island
  "island.listening": "Listening…",
  "island.dictation": "Dictation",
  "island.command": "Command",
  "island.ask": "Circle",
  "island.thinking": "Thinking…",
  "island.processing": "Cleaning up…",
  "island.retrying": "Retrying…",
  "island.speaking": "Speaking…",
  "island.done": "Done",
  "island.error": "Something went wrong",
  "island.confirm": "Confirm",
  "island.cancel": "Cancel",
  "island.confirmTitle": "Are you sure?",
  "island.idleHint": "Click or scroll down to open",
  "island.pasted": "Pasted",
  "island.nothingPlaying": "Nothing playing. Start Spotify or another player.",
  "island.unknownArtist": "Unknown artist",
  "tab.now": "Playing",
  "tab.clipboard": "Clipboard",
  "tab.plans": "Plans",
  "tab.apps": "Apps",
  "tab.settings": "Quick settings",
  "clip.last": "Last {n}",
  "clip.clear": "Clear",
  "clip.empty": "Copy text anywhere — it shows up here (secrets are skipped).",
  "plans.empty": "No plans yet — say “set a 10 minute timer” or “note …”",
  "plans.timers": "Timers",
  "plans.notes": "Notes",
  "apps.placeholder": "Pin an app…",
  "apps.add": "Add",
  "apps.empty": "Pin apps (e.g. Spotify, Chrome) — same fuzzy launch as voice commands.",
  "qs.volume": "Volume",
  "qs.mic": "Microphone",
  "qs.systemDefault": "System default",
  "qs.quiet": "Quiet mode",
  "qs.quietHint": "Island stays compact, no sounds",
  "qs.more": "More in Soul Settings",
  "media.prev": "Previous",
  "media.next": "Next",
  "media.play": "Play",
  "media.pause": "Pause",
  "status.ready": "Ready",
  "status.music": "Music",
  // Ask
  "ask.select": "Circle anything to ask about it",
  "ask.listening": "Listening… speak or type",
  "ask.thinking": "Thinking…",
  "ask.answer": "Answer",
  "ask.starting": "Starting…",
  "ask.typeQuestion": "Or type your question…",
  "ask.followup": "Follow-up question…",
  "ask.send": "Ask",
  "ask.copy": "Copy",
  "ask.copied": "Copied",
  "ask.close": "Close",
  "ask.you": "You",
  "ask.escHint": "Esc closes",
  "ask.voice": "Ask by voice",
} as const;

type Key = keyof typeof en;

const pl: Partial<Record<Key, string>> = {
  "island.listening": "Słucham…",
  "island.dictation": "Dyktowanie",
  "island.command": "Polecenie",
  "island.ask": "Circle",
  "island.thinking": "Myślę…",
  "island.processing": "Porządkuję…",
  "island.retrying": "Ponawiam…",
  "island.speaking": "Mówię…",
  "island.done": "Gotowe",
  "island.error": "Coś poszło nie tak",
  "island.confirm": "Potwierdź",
  "island.cancel": "Anuluj",
  "island.confirmTitle": "Na pewno?",
  "island.idleHint": "Kliknij lub przewiń w dół, aby otworzyć",
  "island.pasted": "Wklejono",
  "island.nothingPlaying": "Nic nie gra. Włącz Spotify lub inny odtwarzacz.",
  "island.unknownArtist": "Nieznany wykonawca",
  "tab.now": "Odtwarzanie",
  "tab.clipboard": "Schowek",
  "tab.plans": "Plany",
  "tab.apps": "Aplikacje",
  "tab.settings": "Szybkie ustawienia",
  "clip.last": "Ostatnie {n}",
  "clip.clear": "Wyczyść",
  "clip.empty": "Skopiuj tekst gdziekolwiek — pojawi się tutaj (hasła są pomijane).",
  "plans.empty": "Brak planów — powiedz „nastaw 10 minut na makaron” albo „zanotuj …”",
  "plans.timers": "Timery",
  "plans.notes": "Notatki",
  "apps.placeholder": "Przypnij aplikację…",
  "apps.add": "Dodaj",
  "apps.empty": "Przypnij aplikacje (np. Spotify, Chrome) — uruchamiane jak polecenia głosowe.",
  "qs.volume": "Głośność",
  "qs.mic": "Mikrofon",
  "qs.systemDefault": "Domyślny systemowy",
  "qs.quiet": "Tryb cichy",
  "qs.quietHint": "Wyspa zostaje kompaktowa, bez dźwięków",
  "qs.more": "Więcej w ustawieniach Soul",
  "media.prev": "Poprzedni",
  "media.next": "Następny",
  "media.play": "Odtwórz",
  "media.pause": "Pauza",
  "status.ready": "Gotowa",
  "status.music": "Muzyka",
  "ask.select": "Zaznacz coś na ekranie, aby zapytać",
  "ask.listening": "Słucham… mów lub pisz",
  "ask.thinking": "Myślę…",
  "ask.answer": "Odpowiedź",
  "ask.starting": "Uruchamiam…",
  "ask.typeQuestion": "Albo wpisz pytanie…",
  "ask.followup": "Pytanie uzupełniające…",
  "ask.send": "Zapytaj",
  "ask.copy": "Kopiuj",
  "ask.copied": "Skopiowano",
  "ask.close": "Zamknij",
  "ask.you": "Ty",
  "ask.escHint": "Esc zamyka",
  "ask.voice": "Zapytaj głosem",
};

const maps: Record<UiLang, Partial<Record<Key, string>>> = { pl, en };

export type I18nKey = Key;

export function translate(lang: UiLang, key: Key, vars?: Record<string, string | number>): string {
  let s = maps[lang]?.[key] ?? en[key] ?? key;
  if (vars) for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  return s;
}

/** Inline bilingual helper for one-off strings (Settings copy): `tr(lang, "Polski", "English")`. */
export function tr(lang: UiLang, pl: string, en: string): string {
  return lang === "en" ? en : pl;
}
