//! Smart talk key: decide whether a finished transcript is dictation (paste it) or a command.
//!
//! Command when the transcript
//! 1. starts with the wake word ("Soul, …", "Hej Soul …") — the wake word is stripped, or
//! 2. has a command verb (otwórz, zamknij, wyszukaj, ścisz, następna, pauza, open…) among its
//!    first four words after Polish filler openers ("weź mi", "czy możesz", "ej", "proszę"), or an
//!    infinitive right after a request form ("możesz mi znaleźć…"), and is short enough, or
//! 3. is a short utterance the deterministic pre-router recognizes ("następna piosenka"), or a
//!    lone greeting ("Dzień dobry." — but "Dzień dobry," starts a letter), or
//! 4. is a short question ("Słuchaj, jak ugotować spaghetti?") — unless a text field has focus,
//!    where a question is far more likely dictated into a chat or document.
//!
//! Everything else is dictation. Matching is diacritic-insensitive (otwórz = otworz).

use super::router;

/// Verb rule applies only up to this many words, so long sentences that merely start with
/// "Otwórz…"/"Pokaż…" are still dictated. The wake word has no limit.
const MAX_VERB_COMMAND_WORDS: usize = 12;
/// The pre-router matches keywords anywhere ("dalej", "stop"…) — only trust it on short text.
const MAX_PREROUTER_WORDS: usize = 6;
/// Question rule: fewer words than this after fillers, and the transcript ends with "?".
const MAX_QUESTION_WORDS: usize = 12;

const QUESTION_WORDS: &[&str] = &[
    "jak", "co", "gdzie", "kiedy", "dlaczego", "czemu", "ile", "kto", "jaki", "jaka", "jakie",
    "jakiego", "jakim", "jakiej", "ktory", "ktora", "ktore", "ktorym", "ktorej", "ktorego", "skad", "dokad",
    "how", "what", "whats", "where", "when", "why", "who", "which",
];

/// First words (ASCII-folded) that make an utterance a command. Polish imperatives + English.
const COMMAND_VERBS: &[&str] = &[
    // Polish
    "otworz", "odpal", "uruchom", "wlacz", "wylacz", "zamknij", "wyszukaj", "znajdz", "poszukaj",
    "wygoogluj", "scisz", "podglosnij", "wycisz", "nastepna", "nastepny", "nastepne",
    "poprzednia", "poprzedni", "pauza", "zatrzymaj", "wznow", "przypomnij", "pokaz", "pusc",
    "zagraj", "odtworz", "przelacz", "ustaw", "przejdz", "sprawdz", "nastaw", "zanotuj",
    "zapamietaj", "zapomnij", "mow", "stworz", "utworz", "odlicz", "zapauzuj", "spauzuj",
    "pauzuj", "wstrzymaj", "odtwarzaj",
    // English
    "open", "launch", "close", "quit", "search", "google", "mute", "unmute", "pause", "resume",
    "skip", "next", "previous", "remind", "show", "play", "find", "remember", "forget",
];

/// Infinitives that are commands only after a request form ("możesz mi znaleźć…").
const REQUEST_INFINITIVES: &[&str] = &[
    "otworzyc", "odpalic", "uruchomic", "wlaczyc", "wylaczyc", "zamknac", "wyszukac", "znalezc",
    "poszukac", "sprawdzic", "pokazac", "puscic", "zagrac", "odtworzyc", "sciszyc", "podglosnic",
    "wyciszyc", "nastawic", "ustawic", "przypomniec", "zanotowac", "zapamietac", "zapomniec",
    "open", "find", "search", "play", "show", "set", "remind",
];

/// The command verb may sit this deep (after fillers): "weź mi teraz znajdź …".
const VERB_WINDOW: usize = 4;

const WAKE_PREFIXES: &[&str] = &["hej", "hey", "ok", "okej", "okay", "hi", "czesc"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Dictation,
    Command {
        /// Text to execute (wake word stripped). May be empty ("Soul" alone).
        text: String,
        reason: &'static str,
    },
}

#[cfg(test)]
pub fn classify(transcript: &str, wake_word: &str) -> Intent {
    classify_with(transcript, wake_word, &|| None)
}

/// `text_field_focused` is asked only when the question rule would fire: `Some(true)` keeps a
/// question as dictation, `None` (unknown) / `Some(false)` route it as a command.
pub fn classify_with(
    transcript: &str,
    wake_word: &str,
    text_field_focused: &dyn Fn() -> Option<bool>,
) -> Intent {
    let raw = transcript.trim();
    if raw.is_empty() {
        return Intent::Dictation;
    }
    let words: Vec<&str> = raw.split_whitespace().collect();
    let folded: Vec<String> = words.iter().map(|w| fold_word(w)).collect();

    if let Some(skip) = wake_word_len(&folded, wake_word) {
        let text = words[skip..]
            .join(" ")
            .trim_start_matches([',', '.', '!', '?', ':', ';', '-', '—'])
            .trim()
            .to_string();
        return Intent::Command {
            text,
            reason: "wake-word",
        };
    }

    let (filler, modal) = router::strip_filler(&folded);
    let rest = &folded[filler..];
    if words.len() - filler <= MAX_VERB_COMMAND_WORDS {
        let verb_at = rest
            .iter()
            .take(VERB_WINDOW)
            .position(|w| COMMAND_VERBS.contains(&w.as_str()));
        if let Some(at) = verb_at {
            return Intent::Command {
                text: raw.to_string(),
                reason: match (filler > 0, at) {
                    (false, 0) => "command-verb",
                    (true, 0) => "filler+command-verb",
                    _ => "command-verb-early",
                },
            };
        }
        if modal
            && rest
                .iter()
                .take(2)
                .any(|w| REQUEST_INFINITIVES.contains(&w.as_str()))
        {
            return Intent::Command {
                text: raw.to_string(),
                reason: "request-infinitive",
            };
        }
    }

    // A greeting that opens a letter ("Dzień dobry, …") stays dictation; said alone it is the
    // morning briefing.
    if router::is_morning_greeting(rest) {
        let alone = rest.len() == 2
            || (rest.len() == 3 && wake_key(&rest[2]) == wake_key(&fold_word(wake_word)));
        return if alone && !raw.ends_with(',') && !raw.ends_with(':') {
            Intent::Command {
                text: raw.to_string(),
                reason: "greeting",
            }
        } else {
            Intent::Dictation
        };
    }
    if words.len() - filler <= MAX_PREROUTER_WORDS && router::match_command(raw).is_some() {
        return Intent::Command {
            text: raw.to_string(),
            reason: "pre-router",
        };
    }

    // „W którym roku…?”, „O której…?”: a preposition may come first.
    let asks = match rest {
        [p, w, ..] if matches!(p.as_str(), "w" | "o" | "na" | "z" | "do" | "od" | "po" | "dla" | "przez" | "in" | "at") => {
            QUESTION_WORDS.contains(&w.as_str())
        }
        [w, ..] => QUESTION_WORDS.contains(&w.as_str()),
        [] => false,
    };
    if raw.ends_with('?') && words.len() - filler < MAX_QUESTION_WORDS && asks {
        if text_field_focused() == Some(true) {
            return Intent::Dictation;
        }
        return Intent::Command {
            text: raw.to_string(),
            reason: "question",
        };
    }

    Intent::Dictation
}

/// First word (after fillers) is a command verb, or an infinitive after a request form — one
/// part of a chained command („…, a potem ustaw timer na 30 minut”).
pub fn starts_with_command_verb(text: &str) -> bool {
    let folded: Vec<String> = text.split_whitespace().map(fold_word).collect();
    let (skip, modal) = router::strip_filler(&folded);
    folded.get(skip).is_some_and(|w| {
        COMMAND_VERBS.contains(&w.as_str()) || (modal && REQUEST_INFINITIVES.contains(&w.as_str()))
    })
}

/// Number of leading words that form the wake phrase ("soul", "hej soul"), if any.
fn wake_word_len(folded: &[String], wake_word: &str) -> Option<usize> {
    let wake = fold_word(wake_word);
    if wake.is_empty() {
        return None;
    }
    let key = wake_key(&wake);
    let is_wake = |w: &str| {
        w == wake
            || wake_key(w) == key // Soulu, Soula, Sołl, Sol, Sól
            || (wake == "soul" && w == "seoul")
    };
    match folded {
        [first, ..] if is_wake(first) => Some(1),
        [prefix, second, ..] if WAKE_PREFIXES.contains(&prefix.as_str()) && is_wake(second) => {
            Some(2)
        }
        _ => None,
    }
}

fn fold_word(w: &str) -> String {
    router::fold_word(w)
}

/// Loose spelling of a (folded) wake word: no doubled letters, "ou" → "o", and one Polish case
/// ending dropped — "soulu", "soula", "soll" (Sołl), "sol" (Sól) all become "sol".
fn wake_key(w: &str) -> String {
    let mut k = String::new();
    for c in w.chars() {
        if !k.ends_with(c) {
            k.push(c);
        }
    }
    let mut k = k.replace("ou", "o");
    if k.chars().count() > 3 && (k.ends_with('u') || k.ends_with('a')) {
        k.pop();
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_cmd(t: &str) -> bool {
        matches!(classify(t, "Soul"), Intent::Command { .. })
    }

    #[test]
    fn wake_word_is_stripped() {
        assert_eq!(
            classify("Soul, jaka jest pogoda?", "Soul"),
            Intent::Command {
                text: "jaka jest pogoda?".into(),
                reason: "wake-word"
            }
        );
        assert_eq!(
            classify("Hej Soul otwórz notatnik", "Soul"),
            Intent::Command {
                text: "otwórz notatnik".into(),
                reason: "wake-word"
            }
        );
        assert!(is_cmd("Soulu, która godzina"));
        assert!(is_cmd("Sól, ścisz muzykę"));
    }

    #[test]
    fn command_verbs_with_and_without_diacritics() {
        assert!(is_cmd("Otwórz Spotify"));
        assert!(is_cmd("otworz spotify"));
        assert!(is_cmd("Zamknij Chrome."));
        assert!(is_cmd("Wyszukaj przepis na pierogi"));
        assert!(is_cmd("ścisz do 20 procent"));
        assert!(is_cmd("Podgłośnij"));
        assert!(is_cmd("Przypomnij mi jutro o spotkaniu"));
        assert!(is_cmd("open Visual Studio Code"));
    }

    #[test]
    fn short_prerouter_phrases_are_commands() {
        assert!(is_cmd("następna piosenka"));
        assert!(is_cmd("głośność 30 procent"));
    }

    #[test]
    fn normal_sentences_are_dictation() {
        assert_eq!(
            classify("Spotkajmy się jutro o dziesiątej.", "Soul"),
            Intent::Dictation
        );
        assert_eq!(
            classify(
                "Dalej uważam, że to dobry pomysł, ale musimy jeszcze porozmawiać o budżecie.",
                "Soul"
            ),
            Intent::Dictation
        );
        assert_eq!(
            classify(
                "Pokaż mi proszę jutro rano te wszystkie dokumenty, o których rozmawialiśmy na ostatnim spotkaniu zespołu.",
                "Soul"
            ),
            Intent::Dictation
        );
        assert_eq!(classify("Solidny plan na weekend.", "Soul"), Intent::Dictation);
        assert_eq!(classify("", "Soul"), Intent::Dictation);
    }

    #[test]
    fn polish_filler_prefixes_route_to_commands() {
        assert_eq!(
            classify("weź mi znajdź jak ugotować jakieś dobre jedzenie", "Soul"),
            Intent::Command {
                text: "weź mi znajdź jak ugotować jakieś dobre jedzenie".into(),
                reason: "filler+command-verb"
            }
        );
        assert!(is_cmd("Weź otwórz Spotify"));
        assert!(is_cmd("czy możesz mi znaleźć przepis na pierogi"));
        assert!(is_cmd("możesz wyszukać pogodę w Krakowie?"));
        assert!(is_cmd("ej, sprawdź kurs euro"));
        assert!(is_cmd("proszę zanotuj kupić chleb"));
        assert!(is_cmd("mów ciszej"));
        assert!(is_cmd("nastaw 10 minut na makaron"));
    }

    #[test]
    fn filler_like_dictation_stays_dictation() {
        assert_eq!(classify("Proszę o przesłanie raportu do piątku.", "Soul"), Intent::Dictation);
        assert_eq!(classify("Możesz przyjść jutro?", "Soul"), Intent::Dictation);
        assert_eq!(classify("Dzień dobry,", "Soul"), Intent::Dictation);
        assert!(is_cmd("Soul, dzień dobry"));
    }

    #[test]
    fn media_inflections_are_commands() {
        for t in [
            "Zapauzuj muzykę.", "Spauzuj.", "Zatrzymaj muzykę.", "Wstrzymaj muzykę",
            "Odtwarzaj muzykę.", "Odtwórz muzykę", "Puść muzykę", "Wznów.", "Graj dalej.",
            "Włącz muzykę", "Puść dalej",
        ] {
            assert!(is_cmd(t), "{t}");
        }
    }

    #[test]
    fn now_playing_questions_are_commands() {
        for t in ["Co teraz mi leci?", "Co leci?", "co mi teraz gra", "Jaka to piosenka?", "Co to za utwór?"] {
            assert!(is_cmd(t), "{t}");
        }
    }

    #[test]
    fn short_questions_are_commands_unless_typing() {
        assert_eq!(
            classify("Słuchaj, jak ugotować spaghetti?", "Soul"),
            Intent::Command {
                text: "Słuchaj, jak ugotować spaghetti?".into(),
                reason: "question"
            }
        );
        assert!(is_cmd("Powiedz mi, ile jest dni w lutym?"));
        assert!(is_cmd("A gdzie jest najbliższa apteka?"));
        assert!(is_cmd("Ej, kiedy jest Wielkanoc?"));
        // Text field focused: the question is being typed somewhere.
        assert_eq!(
            classify_with("Jak się masz?", "Soul", &|| Some(true)),
            Intent::Dictation
        );
        assert!(matches!(classify_with("Jak się masz?", "Soul", &|| Some(false)), Intent::Command { .. }));
        // No question mark, a statement, or too long → dictation.
        assert_eq!(classify("Jak już mówiłem, spotkanie jest w piątek.", "Soul"), Intent::Dictation);
        assert_eq!(classify("Wiesz może, co z tym zrobić?", "Soul"), Intent::Dictation);
        assert_eq!(
            classify("Jak myślisz, czy powinniśmy przenieść spotkanie zespołu na przyszły tydzień po świętach?", "Soul"),
            Intent::Dictation
        );
    }

    #[test]
    fn fuzzy_wake_word() {
        for t in ["Soul, otwórz notatnik", "Sol, która godzina", "Soula, ścisz", "Sołl, pauza", "Hej Soul, co gra", "Hej, Soul, pauza", "Soulu, pomóż"] {
            assert!(
                matches!(classify(t, "Soul"), Intent::Command { reason: "wake-word", .. }),
                "{t}"
            );
        }
        assert_eq!(classify("Solidny plan na weekend.", "Soul"), Intent::Dictation);
        assert_eq!(classify("Solo na gitarze było świetne.", "Soul"), Intent::Dictation);
    }

    #[test]
    fn lone_greeting_is_briefing() {
        assert!(matches!(classify("Dzień dobry.", "Soul"), Intent::Command { reason: "greeting", .. }));
        assert!(is_cmd("Dzień dobry, Soul!"));
        assert!(is_cmd("Good morning"));
        assert_eq!(classify("Dzień dobry,", "Soul"), Intent::Dictation);
        assert_eq!(classify("Dzień dobry wszystkim.", "Soul"), Intent::Dictation);
        assert_eq!(classify("Dzień dobry, przesyłam w załączniku fakturę za październik.", "Soul"), Intent::Dictation);
    }

    #[test]
    fn recorded_traps_stay_dictation() {
        for t in [
            "Odwiedziłem wczoraj nowy sklep kolegi.",
            "Proszę o przesłanie raportu do piątku.",
            "Spotkanie przenosimy na piątek o 10:00.",
            "Kup mleko, to znaczy kefir i chleb.",
            "Let's meet tomorrow at 5:00.",
            "Zażółć gęślą jaźń.",
            "Dalej nie wiem, co o tym myśleć.",
        ] {
            assert_eq!(classify(t, "Soul"), Intent::Dictation, "{t}");
        }
    }

    #[test]
    fn custom_wake_word() {
        assert!(matches!(
            classify("Jarvis włącz muzykę", "Jarvis"),
            Intent::Command { reason: "wake-word", .. }
        ));
        assert_eq!(classify("Soul music is great", "Jarvis"), Intent::Dictation);
    }
}
