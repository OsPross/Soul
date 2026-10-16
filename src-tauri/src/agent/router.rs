//! Deterministic voice-command pre-router (Polish + English).
//! Runs on the transcript BEFORE Gemini tool calling.
//!
//! Polish filler openers ("weź mi", "czy możesz", "ej", "hej Soul"…) are stripped first, so
//! "weź mi znajdź jak ugotować obiad" routes exactly like "znajdź jak ugotować obiad".

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreRoute {
    pub tool: String,
    pub args: Value,
    pub reason: String,
}

/// Filler openers (ASCII-folded), longest first. `true` = a request form ("możesz…", "can you…")
/// after which an infinitive ("znaleźć", "otworzyć") also counts as a command.
const FILLERS: &[(&[&str], bool)] = &[
    (&["hej", "soul"], false),
    (&["hey", "soul"], false),
    (&["czy", "mozesz", "mi"], true),
    (&["czy", "mozesz"], true),
    (&["czy", "moglbys", "mi"], true),
    (&["czy", "moglabys", "mi"], true),
    (&["czy", "moglbys"], true),
    (&["czy", "moglabys"], true),
    (&["mozesz", "mi"], true),
    (&["mozesz"], true),
    (&["moglbys"], true),
    (&["moglabys"], true),
    (&["wez", "mi"], false),
    (&["wez"], false),
    (&["prosze", "cie"], false),
    (&["prosze"], false),
    (&["powiedz", "mi"], false),
    (&["sluchaj"], false),
    (&["no", "to"], false),
    (&["dobra"], false),
    (&["ej"], false),
    (&["a"], false),
    (&["hej"], false),
    (&["soul"], false),
    (&["soulu"], false),
    (&["could", "you"], true),
    (&["can", "you"], true),
    (&["please"], false),
];

/// Number of leading filler words and whether one of them was a request form ("możesz").
pub(super) fn strip_filler(folded: &[String]) -> (usize, bool) {
    let mut skip = 0;
    let mut modal = false;
    'outer: loop {
        for (phrase, is_modal) in FILLERS {
            let end = skip + phrase.len();
            // Never strip the whole utterance ("proszę" alone stays).
            if end < folded.len()
                && folded[skip..end].iter().zip(phrase.iter()).all(|(a, b)| a == b)
            {
                skip = end;
                modal |= *is_modal;
                continue 'outer;
            }
        }
        return (skip, modal);
    }
}

/// Lowercase, strip punctuation, fold Polish diacritics (ó→o, ś→s…).
pub fn fold_word(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric()).map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    match c {
        'Ą' | 'ą' => 'a',
        'Ć' | 'ć' => 'c',
        'Ę' | 'ę' => 'e',
        'Ł' | 'ł' => 'l',
        'Ń' | 'ń' => 'n',
        'Ó' | 'ó' => 'o',
        'Ś' | 'ś' => 's',
        'Ź' | 'ź' | 'Ż' | 'ż' => 'z',
        other => other.to_ascii_lowercase(),
    }
}

/// True for question-like search queries ("jak…", "co to…", "…?") that deserve a spoken answer.
pub fn is_question(query: &str) -> bool {
    let q = query.trim();
    if q.ends_with('?') {
        return true;
    }
    let first = q.split_whitespace().next().map(fold_word).unwrap_or_default();
    matches!(
        first.as_str(),
        "jak" | "jaki" | "jaka" | "jakie" | "jakiego" | "jakim" | "co" | "czy" | "kiedy" | "gdzie"
            | "dlaczego" | "czemu" | "ile" | "kto" | "ktory" | "ktora" | "ktore" | "po" | "skad"
            | "dokad" | "how" | "what" | "whats" | "why" | "when" | "where" | "who" | "which"
            | "is" | "are" | "does" | "do" | "can"
    )
}

/// Match a transcribed command. Returns `None` to fall through to the LLM.
pub fn match_command(transcript: &str) -> Option<PreRoute> {
    let raw = transcript.trim();
    if raw.is_empty() {
        return None;
    }
    let all_words: Vec<&str> = raw.split_whitespace().collect();
    let all_folded: Vec<String> = all_words.iter().map(|w| fold_word(w)).collect();
    let (skip, _) = strip_filler(&all_folded);
    let words = &all_words[skip..];
    let folded = &all_folded[skip..];
    let lower = normalize(&words.join(" "));
    let mut route = route(&lower, words, folded)?;
    if skip > 0 {
        route.reason.push_str("+filler");
    }
    Some(route)
}

fn pr(tool: &str, args: Value, reason: impl Into<String>) -> Option<PreRoute> {
    Some(PreRoute {
        tool: tool.into(),
        args,
        reason: reason.into(),
    })
}

fn route(lower: &str, words: &[&str], folded: &[String]) -> Option<PreRoute> {
    if folded.is_empty() {
        return None;
    }
    if let Some(r) = match_voice_volume(lower) {
        return Some(r);
    }
    if contains_any(
        lower,
        &[
            "anuluj timer", "anuluj minutnik", "anuluj przypomnienie", "wylacz timer",
            "wylacz minutnik", "usun timer", "zatrzymaj timer", "zatrzymaj minutnik",
            "stop timer", "cancel timer", "cancel the timer",
        ],
    ) {
        return pr("cancel_timer", json!({}), "timer:cancel");
    }
    if let Some(r) = match_timer(lower, words, folded) {
        return Some(r);
    }
    if let Some(r) = match_memory(lower, words, folded) {
        return Some(r);
    }
    if let Some(r) = match_note(words, folded) {
        return Some(r);
    }
    if let Some(r) = match_scene(words, folded) {
        return Some(r);
    }
    if is_now_playing(lower, folded) {
        return pr("now_playing", json!({}), "now-playing");
    }
    if is_morning_greeting(folded) || lower.contains("briefing") || lower.contains("poranny raport") {
        return pr("morning_briefing", json!({}), "briefing");
    }

    // „to samo na YouTube” points back at an earlier request: the follow-up resolver or the model
    // (with the [CONTEXT] block) knows what „to samo” is — „samo” is never the query.
    if super::context::is_same_reference(folded) {
        return None;
    }

    // YouTube (mention of youtube / yt / jutub…)
    if has_youtube(lower) {
        let query = strip_youtube_and_search_triggers(lower);
        if query.is_empty() {
            // „Weź mi przecież tego YouTube.” — nothing to search for: just open the site.
            return pr("open_url", json!({ "url": "https://www.youtube.com" }), "youtube+open");
        }
        if is_youtube_play_intent(lower) {
            return pr("youtube_play", json!({ "query": query }), "youtube+play");
        }
        return pr("youtube_search", json!({ "query": query }), "youtube");
    }

    // Explicit web search triggers only
    if let Some(query) = extract_web_search_query(words, folded) {
        let reason = if is_question(&query) {
            "search-trigger+question"
        } else {
            "search-trigger"
        };
        return pr("web_search", json!({ "query": query }), reason);
    }

    if let Some(action) = match_media(lower) {
        return pr("media_control", json!({ "action": action }), format!("media:{action}"));
    }

    // Volume (needs a 0–100 number); „ścisz o 20” / „podgłośnij o 10” are relative.
    if let Some(percent) = match_volume(lower) {
        if percent > 0 {
            if let Some(delta) = relative_volume(lower) {
                return pr("set_volume", json!({ "delta": delta }), format!("volume:{delta:+}"));
            }
        }
        return pr("set_volume", json!({ "percent": percent }), format!("volume:{percent}"));
    }

    None
}

/// „ścisz o 20” → -20, „podgłośnij o 10 procent” → +10 (needs „o N” / "by N").
fn relative_volume(lower: &str) -> Option<i64> {
    let dir = if contains_any(lower, &["scisz", "sciszy", "przycisz", "ciszej", "quieter", "lower"]) {
        -1
    } else if contains_any(lower, &["podglos", "glosniej", "louder", "raise", "turn up"]) {
        1
    } else {
        return None;
    };
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    tokens
        .windows(2)
        .find(|p| p[0] == "o" || p[0] == "by")
        .and_then(|p| p[1].trim_end_matches(['%', '.', ',', '!']).parse::<i64>().ok())
        .filter(|n| (1..=100).contains(n))
        .map(|n| dir * n)
}

/// „otwórz Discord”, „zamknij Spotify”, „przełącz na Chrome” → open/close/focus_app. Used only
/// for the parts of a chained command — a single „otwórz X” still goes to the model, which is
/// better at odd app names.
pub(super) fn match_app_verb(text: &str) -> Option<PreRoute> {
    let all_words: Vec<&str> = text.split_whitespace().collect();
    let all_folded: Vec<String> = all_words.iter().map(|w| fold_word(w)).collect();
    let (skip, _) = strip_filler(&all_folded);
    let (words, folded) = (&all_words[skip..], &all_folded[skip..]);
    let tool = match folded.first()?.as_str() {
        "otworz" | "odpal" | "uruchom" | "wlacz" | "open" | "launch" | "start" => "open_app",
        "zamknij" | "close" | "quit" | "wylacz" => "close_app",
        "przelacz" | "przejdz" | "focus" | "switch" => "focus_app",
        _ => return None,
    };
    let name = tail_after(
        words,
        folded,
        1,
        &["na", "do", "to", "mi", "aplikacje", "aplikacja", "program", "app", "the"],
    );
    let n = name.split_whitespace().count();
    if n == 0 || n > 4 || names_screen_item(&folded[1..]) {
        return None;
    }
    pr(tool, json!({ "name": name }), format!("app-verb:{tool}"))
}

/// „włącz drugi wynik”, „otwórz trzeci link”: something on the screen, not an app name.
pub(crate) fn names_screen_item(folded: &[String]) -> bool {
    const ITEMS: &[&str] = &[
        "wynik", "wyniki", "wyniku", "link", "linka", "linku", "film", "filmik", "filmu", "wideo", "result",
        "video",
    ];
    const ORDINALS: &[&str] = &[
        "pierwsz", "drugi", "druga", "drugie", "trzec", "czwart", "piat", "szost", "ostatni", "first", "second",
        "third", "last",
    ];
    folded.iter().any(|w| ITEMS.contains(&w.as_str()) || ORDINALS.iter().any(|o| w.starts_with(o)))
}

/// Words that may sit inside a now-playing question: „co (teraz) (mi) leci”.
const NOW_PLAYING_EXTRA: &[&str] = &["teraz", "mi", "tu", "tutaj", "aktualnie", "wlasnie", "sobie", "tam", "to"];

/// „co gra?”, „co teraz mi leci?”, „jaka to piosenka?”, „co to za utwór?”, "what's playing".
fn is_now_playing(lower: &str, folded: &[String]) -> bool {
    if contains_any(
        lower,
        &[
            "co gra", "co leci", "jaka to piosenka", "jaki to utwor", "jaki to kawalek",
            "co to za piosenka", "co to za utwor", "co to za kawalek", "what's playing",
            "whats playing", "what is playing", "what song is this",
        ],
    ) {
        return true;
    }
    let core: Vec<&str> = folded
        .iter()
        .map(String::as_str)
        .filter(|w| !NOW_PLAYING_EXTRA.contains(w))
        .collect();
    matches!(
        core.as_slice(),
        ["co", "leci" | "gra" | "slychac"]
            | ["co", "za", "piosenka" | "utwor" | "kawalek" | "muzyka"]
            | ["jaka", "piosenka" | "muzyka"]
            | ["jaki", "utwor" | "kawalek"]
    )
}

/// "dzień dobry" / "good morning" (+ at most two more words, e.g. "dzień dobry Soul").
pub(super) fn is_morning_greeting(folded: &[String]) -> bool {
    folded.len() >= 2
        && folded.len() <= 4
        && ((folded[0] == "dzien" && folded[1] == "dobry")
            || (folded[0] == "good" && folded[1] == "morning"))
}

/// Soul's own voice: "mów ciszej / głośniej", "wycisz się", "głos Soul na 40%".
fn match_voice_volume(lower: &str) -> Option<PreRoute> {
    if contains_any(lower, &["wycisz sie", "badz cicho", "zamknij sie", "mute yourself", "be quiet"]) {
        return pr("set_voice_volume", json!({ "percent": 0 }), "voice-volume:mute");
    }
    if contains_any(
        lower,
        &[
            "mow ciszej", "ciszej mow", "mow troche ciszej", "mow nieco ciszej", "badz ciszej",
            "speak quieter", "speak softer", "talk quieter",
        ],
    ) {
        return pr("set_voice_volume", json!({ "delta": -20 }), "voice-volume:down");
    }
    if contains_any(
        lower,
        &[
            "mow glosniej", "glosniej mow", "mow troche glosniej", "mow nieco glosniej",
            "speak louder", "talk louder", "speak up",
        ],
    ) {
        return pr("set_voice_volume", json!({ "delta": 20 }), "voice-volume:up");
    }
    if contains_any(
        lower,
        &["glosu", "twoj glos", "swoj glos", "glos soul", "voice volume", "your voice"],
    ) {
        if let Some(p) = extract_percent(lower) {
            return pr("set_voice_volume", json!({ "percent": p.min(100) }), "voice-volume:set");
        }
    }
    None
}

/// "nastaw 10 minut na makaron", "przypomnij mi za godzinę żeby zadzwonić", "timer 30 sekund".
fn match_timer(lower: &str, words: &[&str], folded: &[String]) -> Option<PreRoute> {
    let strong = contains_any(lower, &["timer", "minutnik", "odlicz", "przypomnij", "remind", "budzik"]);
    let setter = matches!(folded[0].as_str(), "nastaw" | "ustaw" | "set");
    if !strong && !setter {
        return None;
    }
    let (seconds, end) = parse_duration(folded)?;
    if seconds == 0 {
        return None;
    }
    let label = label_after(words, folded, end);
    pr("set_timer", json!({ "seconds": seconds, "label": label }), "timer")
}

fn unit_seconds(w: &str) -> Option<u64> {
    if w.starts_with("sek") || matches!(w, "s" | "sec" | "second" | "seconds") {
        Some(1)
    } else if w.starts_with("min") {
        Some(60)
    } else if w.starts_with("godz") || matches!(w, "h" | "hour" | "hours") {
        Some(3600)
    } else {
        None
    }
}

pub(super) fn number_word(w: &str) -> Option<u64> {
    Some(match w {
        "jeden" | "jedna" | "jedno" | "one" => 1,
        "dwa" | "dwie" | "two" => 2,
        "trzy" | "three" => 3,
        "cztery" | "four" => 4,
        "piec" | "five" => 5,
        "szesc" | "six" => 6,
        "siedem" | "seven" => 7,
        "osiem" | "eight" => 8,
        "dziewiec" | "nine" => 9,
        "dziesiec" | "ten" => 10,
        "jedenascie" => 11,
        "dwanascie" | "twelve" => 12,
        "pietnascie" | "fifteen" => 15,
        "dwadziescia" | "twenty" => 20,
        "trzydziesci" | "thirty" => 30,
        "czterdziesci" | "forty" => 40,
        "piecdziesiat" | "fifty" => 50,
        "szescdziesiat" | "sixty" => 60,
        "dziewiecdziesiat" | "ninety" => 90,
        _ => return None,
    })
}

/// Sum of all durations in the utterance ("godzinę i 20 minut") and the index after the last one.
pub(super) fn parse_duration(folded: &[String]) -> Option<(u64, usize)> {
    let mut total = 0u64;
    let mut end = None;
    let mut i = 0;
    while i < folded.len() {
        let w = folded[i].as_str();
        let digits: String = w.chars().take_while(|c| c.is_ascii_digit()).collect();
        // "10min", "30s"
        if !digits.is_empty() && digits.len() < w.len() {
            if let (Ok(n), Some(u)) = (digits.parse::<u64>(), unit_seconds(&w[digits.len()..])) {
                total += n * u;
                end = Some(i + 1);
                i += 1;
                continue;
            }
        }
        let n = if !digits.is_empty() && digits.len() == w.len() {
            digits.parse::<u64>().ok()
        } else {
            number_word(w)
        };
        if let Some(n) = n {
            if let Some(u) = folded.get(i + 1).and_then(|u| unit_seconds(u)) {
                total += n * u;
                end = Some(i + 2);
                i += 2;
                continue;
            }
        }
        if w == "pol" || w == "half" {
            // "pół godziny", "half an hour"
            let gap = if folded.get(i + 1).and_then(|u| unit_seconds(u)).is_some() { 1 } else { 2 };
            if let Some(u) = folded.get(i + gap).and_then(|u| unit_seconds(u)) {
                total += u / 2;
                end = Some(i + gap + 1);
                i += gap + 1;
                continue;
            }
        }
        if w == "kwadrans" {
            total += 900;
            end = Some(i + 1);
        } else if matches!(w, "minute" | "minutke" | "godzine" | "sekunde")
            && i > 0
            && matches!(folded[i - 1].as_str(), "za" | "na" | "in" | "for")
        {
            // "za minutę", "na godzinę"
            total += unit_seconds(w).unwrap_or(0);
            end = Some(i + 1);
        }
        i += 1;
    }
    end.map(|e| (total, e))
}

/// Timer label: the words after the duration minus connectors ("na makaron" → "makaron").
fn label_after(words: &[&str], folded: &[String], from: usize) -> String {
    tail_after(
        words,
        folded,
        from,
        &["na", "o", "ze", "zeby", "aby", "by", "do", "for", "to", "about", "i", "and"],
    )
}

fn clean_tail(s: &str) -> String {
    drop_leading_connector(s.trim()
        .trim_matches(|c: char| {
            matches!(c, '.' | ',' | '!' | '?' | ':' | ';' | '"' | '\u{201e}' | '\u{201d}')
        })
        .trim())
}

/// A leading connector from a chained utterance („… i jak ugotować spaghetti”).
fn drop_leading_connector(t: &str) -> String {
    let words: Vec<&str> = t.split_whitespace().collect();
    let skip = words.iter().take_while(|w| matches!(fold_word(w).as_str(), "i" | "oraz" | "and" | "no")).count();
    if skip == 0 || skip == words.len() { t.to_string() } else { words[skip..].join(" ") }
}

/// Original words from `from` on, skipping leading connectors (diacritics kept).
fn tail_after(words: &[&str], folded: &[String], from: usize, skip: &[&str]) -> String {
    let mut i = from;
    while i < folded.len() && (skip.contains(&folded[i].as_str()) || folded[i].is_empty()) {
        i += 1;
    }
    clean_tail(&words[i.min(words.len())..].join(" "))
}

/// "zapamiętaj, że …", "zapomnij o …", "co pamiętasz o …".
fn match_memory(lower: &str, words: &[&str], folded: &[String]) -> Option<PreRoute> {
    match folded[0].as_str() {
        "zapamietaj" | "remember" => {
            let fact = tail_after(words, folded, 1, &["ze", "to", "sobie", "that", "prosze", "mi"]);
            if fact.is_empty() {
                return None;
            }
            return pr("remember", json!({ "fact": capitalize(&fact) }), "memory:remember");
        }
        "zapomnij" | "forget" => {
            let query = tail_after(words, folded, 1, &["o", "ze", "about", "that", "to"]);
            if query.is_empty() {
                return None;
            }
            return pr("forget", json!({ "query": query }), "memory:forget");
        }
        _ => {}
    }
    if folded.len() >= 2 && folded[0] == "co" && folded[1] == "pamietasz" {
        let query = tail_after(words, folded, 2, &["o", "na", "temat"]);
        return pr("recall", json!({ "query": query }), "memory:recall");
    }
    if contains_any(
        lower,
        &["co o mnie wiesz", "co wiesz o mnie", "what do you remember", "what do you know about me"],
    ) {
        return pr("recall", json!({ "query": "" }), "memory:recall");
    }
    None
}

/// "zanotuj …", "notatka: …", "zapisz notatkę …", "take a note …".
fn match_note(words: &[&str], folded: &[String]) -> Option<PreRoute> {
    let f0 = folded[0].as_str();
    let f1 = folded.get(1).map(String::as_str).unwrap_or("");
    let start = if matches!(f0, "zanotuj" | "notatka" | "note") {
        1
    } else if matches!(f0, "zapisz" | "dodaj" | "zrob" | "take" | "make")
        && (f1.starts_with("notat") || f1 == "note" || f1 == "a")
    {
        2
    } else {
        return None;
    };
    let text = tail_after(words, folded, start, &["ze", "notatke", "notatka", "note", "a", "prosze"]);
    if text.is_empty() {
        return None;
    }
    pr("add_note", json!({ "text": capitalize(&text) }), "note")
}

/// "stwórz scenę granie: otwórz Discord, FiveM i Spotify", "uruchom scenę granie".
fn match_scene(words: &[&str], folded: &[String]) -> Option<PreRoute> {
    let f0 = folded[0].as_str();
    let f1_scene = folded.get(1).is_some_and(|w| w.starts_with("scen") || w == "macro");
    if matches!(f0, "stworz" | "utworz" | "dodaj" | "zrob" | "create" | "make" | "nowa") && f1_scene {
        // The name runs until a word ending with ':' (or a connector); the steps follow.
        let mut split = None;
        for (i, w) in words.iter().enumerate().skip(2) {
            if w.ends_with(':') {
                split = Some((i + 1, i + 1));
                break;
            }
            if i > 2 && matches!(folded[i].as_str(), "ktora" | "ktore" | "that" | "with" | "z") {
                split = Some((i, i + 1));
                break;
            }
        }
        let (name_end, steps_from) = split.unwrap_or((3.min(words.len()), 3.min(words.len())));
        let name = clean_tail(&words[2.min(name_end)..name_end].join(" "));
        let steps = parse_scene_steps(&words[steps_from.min(words.len())..]);
        if name.is_empty() || steps.is_empty() {
            return None;
        }
        return pr(
            "create_scene",
            json!({ "name": name, "steps": steps, "triggers": [] }),
            "scene:create",
        );
    }
    let run_verb = matches!(f0, "uruchom" | "odpal" | "wlacz" | "aktywuj" | "run" | "start" | "play");
    let start = if run_verb && f1_scene {
        2
    } else if f0.starts_with("scen") && folded.len() > 1 {
        1
    } else {
        return None;
    };
    let name = clean_tail(&words[start..].join(" "));
    if name.is_empty() {
        return None;
    }
    pr("run_scene", json!({ "name": name }), "scene:run")
}

/// Split "otwórz Discord, FiveM i Spotify, głośność 40%" into ordered scene steps
/// (`{type, value}`; bare names after an "otwórz" inherit it).
pub fn parse_scene_steps(words: &[&str]) -> Vec<Value> {
    let mut segments: Vec<Vec<&str>> = vec![Vec::new()];
    for (i, w) in words.iter().enumerate() {
        let f = fold_word(w);
        let next = words.get(i + 1).map(|n| fold_word(n)).unwrap_or_default();
        if matches!(f.as_str(), "i" | "oraz" | "potem" | "nastepnie" | "then" | "and")
            || (f == "a" && next == "potem")
        {
            segments.push(Vec::new());
            continue;
        }
        let trimmed = w.trim_end_matches([',', ';', '.']);
        if !trimmed.is_empty() {
            segments.last_mut().unwrap().push(trimmed);
        }
        if w.ends_with([',', ';']) {
            segments.push(Vec::new());
        }
    }
    let step = |t: &str, v: String| json!({ "type": t, "value": v });
    let mut steps = Vec::new();
    let mut last_open = false;
    for seg in segments.into_iter().filter(|s| !s.is_empty()) {
        let folded: Vec<String> = seg.iter().map(|w| fold_word(w)).collect();
        let lower = folded.join(" ");
        let rest = seg[1..].join(" ");
        if lower.contains("nie przeszkadzac") || lower.contains("dnd") || lower.contains("do not disturb") {
            steps.push(step("dnd", "on".into()));
            last_open = false;
            continue;
        }
        match folded[0].as_str() {
            "otworz" | "odpal" | "uruchom" | "wlacz" | "open" | "launch" | "start" => {
                last_open = true;
                if rest.starts_with("http") {
                    steps.push(step("open_url", rest));
                } else if !rest.is_empty() {
                    steps.push(step("open_app", rest));
                }
            }
            "pusc" | "odtworz" | "zagraj" | "play" => {
                last_open = false;
                let q: Vec<&str> = seg[1..]
                    .iter()
                    .copied()
                    .filter(|w| !matches!(fold_word(w).as_str(), "na" | "youtube" | "yt" | "on"))
                    .collect();
                if q.is_empty() {
                    steps.push(step("media", "play".into()));
                } else {
                    steps.push(step("youtube_play", q.join(" ")));
                }
            }
            "poczekaj" | "czekaj" | "odczekaj" | "wait" => {
                last_open = false;
                let ms = parse_duration(&folded).map(|(s, _)| s * 1000).unwrap_or(2000);
                steps.push(step("delay", ms.to_string()));
            }
            _ if contains_any(&lower, &["glosnosc", "scisz", "podglosnij", "volume"]) => {
                last_open = false;
                if let Some(p) = extract_percent(&normalize(&seg.join(" "))) {
                    steps.push(step("set_volume", p.to_string()));
                }
            }
            _ if last_open => steps.push(step("open_app", seg.join(" "))),
            _ => {}
        }
    }
    steps
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

const SEARCH_TRIGGERS: &[&str] = &[
    "wyszukaj", "wyszukac", "znajdz", "znalezc", "poszukaj", "poszukac", "wygoogluj", "sprawdz",
    "sprawdzic", "search", "find", "google", "lookup", "googlnij",
];

/// Query after an explicit search verb in the first four words, with the user's original words
/// (diacritics kept): "znajdź mi jak ugotować obiad" → "jak ugotować obiad".
fn extract_web_search_query(words: &[&str], folded: &[String]) -> Option<String> {
    let mut at = folded.iter().take(4).position(|w| SEARCH_TRIGGERS.contains(&w.as_str()));
    if at.is_none() {
        // "look up …"
        at = folded
            .windows(2)
            .take(3)
            .position(|p| p[0] == "look" && p[1] == "up")
            .map(|i| i + 1);
    }
    if at.is_none() {
        // "pokaż (mi) jak / gdzie …"
        if let Some(i) = folded.iter().take(2).position(|w| w == "pokaz") {
            let j = if folded.get(i + 1).map(String::as_str) == Some("mi") { i + 2 } else { i + 1 };
            if folded.get(j).is_some_and(|w| matches!(w.as_str(), "jak" | "gdzie" | "co" | "ile")) {
                at = Some(i);
            }
        }
    }
    let at = at?;
    let filler = |w: &str| {
        matches!(w, "mi" | "dla" | "mnie" | "prosze" | "please" | "me" | "for" | "the" | "up" | "cos" | "")
    };
    let web = |w: &str| {
        matches!(w, "google" | "googlu" | "internecie" | "necie" | "sieci" | "web" | "internet")
    };
    let place = |w: &str| matches!(w, "w" | "na" | "in" | "on");
    let mut start = at + 1;
    let mut end = words.len();
    loop {
        if start < end && (filler(&folded[start]) || web(&folded[start])) {
            start += 1;
        } else if start + 1 < end && place(&folded[start]) && web(&folded[start + 1]) {
            start += 2;
        } else {
            break;
        }
    }
    while end >= start + 2 && web(&folded[end - 1]) && place(&folded[end - 2]) {
        end -= 2;
    }
    let query = clean_tail(&words[start.min(end)..end].join(" "));
    (!query.is_empty()).then_some(query)
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(fold_char)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_youtube(lower: &str) -> bool {
    lower.contains("youtube")
        || lower.split_whitespace().any(|w| w == "yt")
        || lower.contains("jutub") // jutube / na jutubie
        || lower.contains("youtub")
}

fn is_youtube_play_intent(lower: &str) -> bool {
    // After normalize, diacritics are stripped — use ASCII forms.
    const PLAY_NORM: &[&str] = &["odtworz", "zagraj", "pusc", "play", "wlacz film"];
    PLAY_NORM.iter().any(|p| lower.contains(p))
}

fn strip_youtube_and_search_triggers(lower: &str) -> String {
    let mut s = lower.to_string();
    for t in [
        "look up",
        "wyszukaj",
        "znajdz",
        "poszukaj",
        "wygoogluj",
        "search",
        "find",
        "lookup",
        "google",
        "na youtube",
        "on youtube",
        "youtube",
        "youtub",
        "jutubie",
        "jutube",
        "jutub",
        " yt ",
        "odtworz",
        "zagraj",
        "pusc",
        "play",
        "na jutubie",
        "na yt",
        "dla mnie",
        "please",
        "prosze",
    ] {
        s = format!(" {s} ").replace(t, " ");
    }
    // Lone "yt" token
    s = s
        .split_whitespace()
        .filter(|w| *w != "yt" && *w != "na" && *w != "on" && *w != "the" && *w != "a")
        .collect::<Vec<_>>()
        .join(" ");
    trim_query_fillers(&cleanup_query(&s))
}

/// Filler words and bare punctuation at the edges of a YouTube query („przeciez tego .” → „”).
fn trim_query_fillers(q: &str) -> String {
    const NOISE: &[&str] = &[
        "przeciez", "tego", "tej", "ten", "te", "to", "mi", "mnie", "no", "wlasnie", "prostu", "jakis", "jakies", "teraz",
        "juz", "tak", "nam", "sobie", "moze", "i", "oraz", "and", "just",
    ];
    let words: Vec<&str> = q
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let (mut a, mut b) = (0, words.len());
    while a < b && NOISE.contains(&words[a]) {
        a += 1;
    }
    while b > a && NOISE.contains(&words[b - 1]) {
        b -= 1;
    }
    words[a..b].join(" ")
}

fn cleanup_query(s: &str) -> String {
    s.split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "na" | "w" | "do" | "dla" | "o" | "the" | "a" | "an" | "for" | "me" | "please" | "prosze"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Resume phrases checked before the "dalej" → next rule ("graj dalej" is play, not next).
const RESUME_PHRASES: &[&str] = &[
    "graj dalej", "pusc dalej", "odtwarzaj dalej", "wznow", "resume", "unpause",
    "odtwarzaj muzyke", "odtworz muzyke", "pusc muzyke", "wlacz muzyke", "wlacz z powrotem muzyke",
    "play music", "play track", "play song",
];

fn match_media(lower: &str) -> Option<&'static str> {
    // Prefer more specific phrases first.
    if contains_any(lower, RESUME_PHRASES) && !contains_any(lower, &["wylacz", "zatrzymaj"]) {
        return Some("play");
    }
    // next
    if contains_any(
        lower,
        &[
            "nastepna",
            "nastepny",
            "nastepne",
            "dalej",
            "skip",
            "next track",
            "next song",
            "next",
        ],
    ) {
        return Some("next");
    }
    // prev
    if contains_any(
        lower,
        &[
            "poprzednia",
            "poprzedni",
            "poprzednie",
            "wroc",
            "cofnij",
            "previous",
            "prev",
            "last track",
            "last song",
        ],
    ) {
        return Some("prev");
    }
    // pause / stop
    if contains_any(
        lower,
        &[
            "pauza",
            "pauzuj", // zapauzuj, spauzuj
            "zatrzymaj",
            "wstrzymaj",
            "stop",
            "pause",
            "wylacz muzyke",
        ],
    ) {
        return Some("pause");
    }
    // play / resume — avoid matching when it's clearly "open/play an app"
    let bare = lower.trim_end_matches(['.', '!', '?']);
    if matches!(bare, "graj" | "play" | "odtworz" | "odtwarzaj" | "pusc")
        || lower.starts_with("graj ")
    {
        // Don't steal "otworz …" / open-app style commands.
        if lower.contains("otworz") || lower.contains("open ") {
            return None;
        }
        return Some("play");
    }
    None
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

fn match_volume(lower: &str) -> Option<u8> {
    let volumeish = contains_any(
        lower,
        &[
            "glosnosc",
            "glosniej",
            "glosno",
            "scisz",
            "sciszy",
            "podglosnij",
            "podglos",
            "volume",
            "louder",
            "quieter",
            "mute",
            "wycisz",
            "procent",
            "%",
        ],
    );
    if !volumeish {
        return None;
    }

    if lower.contains("wycisz") || lower.contains("mute") {
        return Some(0);
    }

    let percent = extract_percent(lower)?;
    Some(percent.min(100))
}

fn extract_percent(lower: &str) -> Option<u8> {
    // Scan tokens for a 0–100 integer (optionally followed by % / procent).
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let cleaned = tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '%');
        let num_part = cleaned.trim_end_matches('%');
        if let Ok(n) = num_part.parse::<u16>() {
            if n <= 100 {
                // Prefer numbers near volume words / "procent"
                let nearby = tokens
                    .iter()
                    .skip(i.saturating_sub(2))
                    .take(5)
                    .any(|t| {
                        t.contains("procent")
                            || t.contains('%')
                            || t.contains("glos")
                            || t.contains("volume")
                            || t.contains("scisz")
                            || t.contains("podglos")
                    });
                if nearby || cleaned.ends_with('%') {
                    return Some(n as u8);
                }
                // If the whole phrase is volume-ish, accept the first small number.
                if n <= 100 {
                    return Some(n as u8);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(transcript: &str) -> Option<(String, Value)> {
        match_command(transcript).map(|r| (r.tool, r.args))
    }

    #[test]
    fn nastepna_piosenka_is_media_next_not_search() {
        let r = match_command("następna piosenka").unwrap();
        assert_eq!(r.tool, "media_control");
        assert_eq!(r.args["action"], "next");
    }

    #[test]
    fn media_inflections() {
        for (t, action) in [
            ("Zapauzuj muzykę.", "pause"),
            ("spauzuj", "pause"),
            ("Wstrzymaj muzykę", "pause"),
            ("Zatrzymaj muzykę", "pause"),
            ("Odtwarzaj muzykę.", "play"),
            ("odtwórz muzykę", "play"),
            ("Puść muzykę", "play"),
            ("Puść dalej", "play"),
            ("Graj dalej.", "play"),
            ("Włącz muzykę", "play"),
            ("Wznów.", "play"),
            ("Wyłącz muzykę", "pause"),
            ("Dalej.", "next"),
        ] {
            let (tool, args) = tool(t).unwrap_or_else(|| panic!("{t}"));
            assert_eq!((tool.as_str(), args["action"].as_str()), ("media_control", Some(action)), "{t}");
        }
    }

    #[test]
    fn now_playing_variants() {
        for t in ["Co teraz mi leci?", "co mi teraz gra", "Co to za utwór?", "Jaka to piosenka?", "co tu leci"] {
            assert_eq!(tool(t).map(|(n, _)| n), Some("now_playing".into()), "{t}");
        }
    }

    #[test]
    fn pauza_is_pause() {
        let r = match_command("pauza").unwrap();
        assert_eq!(r.tool, "media_control");
        assert_eq!(r.args["action"], "pause");
    }

    #[test]
    fn web_search_requires_trigger() {
        let r = match_command("wyszukaj przepis na pizzę").unwrap();
        assert_eq!(r.tool, "web_search");
        let q = r.args["query"].as_str().unwrap();
        assert!(q.contains("przepis"));
        assert!(q.contains("pizzę"), "{q}");
    }

    #[test]
    fn youtube_search_from_znajdz() {
        let r = match_command("znajdź na YouTube lo-fi do nauki").unwrap();
        assert_eq!(r.tool, "youtube_search");
        let q = r.args["query"].as_str().unwrap();
        assert!(q.contains("lo-fi") || q.contains("lofi") || q.contains("nauki"));
        assert!(!q.contains("youtube"));
    }

    #[test]
    fn youtube_filler_is_not_a_query() {
        let r = match_command("Weź mi przecież tego YouTube.").unwrap();
        assert_eq!((r.tool.as_str(), r.args["url"].as_str()), ("open_url", Some("https://www.youtube.com")));
        let r = match_command("puść mi no tego queen na youtube").unwrap();
        assert_eq!(r.args["query"], "queen");
        assert_eq!(trim_query_fillers("przeciez tego ."), "");
    }

    #[test]
    fn leading_connector_is_dropped_from_queries() {
        assert_eq!(clean_tail("i jak ugotować spaghetti"), "jak ugotować spaghetti");
        assert_eq!(clean_tail("i"), "i");
        let r = match_command("wyszukaj i jak ugotować spaghetti").unwrap();
        assert_eq!(r.args["query"], "jak ugotować spaghetti");
    }

    #[test]
    fn otworz_spotify_falls_through() {
        assert!(match_command("otwórz Spotify").is_none());
    }

    #[test]
    fn volume_percent() {
        let r = match_command("głośność 30 procent").unwrap();
        assert_eq!(r.tool, "set_volume");
        assert_eq!(r.args["percent"], 30);
    }

    #[test]
    fn music_without_search_trigger_not_web() {
        assert!(tool("puść następną").is_some());
        let r = match_command("puść następną").unwrap();
        assert_eq!(r.tool, "media_control");
    }

    #[test]
    fn filler_prefix_then_search_keeps_polish_query() {
        let r = match_command("weź mi znajdź jak ugotować jakieś dobre jedzenie").unwrap();
        assert_eq!(r.tool, "web_search");
        assert_eq!(r.args["query"], "jak ugotować jakieś dobre jedzenie");
        assert!(r.reason.contains("filler") && r.reason.contains("question"), "{}", r.reason);
        for t in [
            "możesz wyszukać przepis na sernik",
            "czy możesz mi poszukać przepis na sernik",
            "ej Soul sprawdź przepis na sernik",
            "proszę wyszukaj przepis na sernik w google",
        ] {
            let r = match_command(t).unwrap_or_else(|| panic!("no route for {t}"));
            assert_eq!(r.tool, "web_search", "{t}");
            assert_eq!(r.args["query"], "przepis na sernik", "{t}");
        }
    }

    #[test]
    fn question_detection() {
        assert!(is_question("jak ugotować spaghetti"));
        assert!(is_question("co to jest kwantowy komputer"));
        assert!(is_question("pogoda Kraków?"));
        assert!(!is_question("przepis na sernik"));
    }

    #[test]
    fn voice_volume_commands() {
        assert_eq!(match_command("mów ciszej").unwrap().args["delta"], -20);
        assert_eq!(match_command("Mów głośniej proszę").unwrap().args["delta"], 20);
        let r = match_command("wycisz się").unwrap();
        assert_eq!(r.tool, "set_voice_volume");
        assert_eq!(r.args["percent"], 0);
        // System volume stays system volume.
        assert_eq!(match_command("wycisz").unwrap().tool, "set_volume");
    }

    #[test]
    fn timers() {
        let r = match_command("nastaw 10 minut na makaron").unwrap();
        assert_eq!(r.tool, "set_timer");
        assert_eq!(r.args["seconds"], 600);
        assert_eq!(r.args["label"], "makaron");
        let r = match_command("przypomnij mi za pół godziny żeby zadzwonić do mamy").unwrap();
        assert_eq!(r.args["seconds"], 1800);
        assert_eq!(r.args["label"], "zadzwonić do mamy");
        assert_eq!(match_command("timer 1 godzina i 20 minut").unwrap().args["seconds"], 4800);
        assert_eq!(match_command("minutnik dwie minuty").unwrap().args["seconds"], 120);
        assert_eq!(match_command("anuluj minutnik").unwrap().tool, "cancel_timer");
    }

    #[test]
    fn memory_notes_and_extras() {
        let r = match_command("zapamiętaj, że gram na serwerze CityRP").unwrap();
        assert_eq!(r.tool, "remember");
        assert_eq!(r.args["fact"], "Gram na serwerze CityRP");
        assert_eq!(match_command("zapomnij o serwerze").unwrap().args["query"], "serwerze");
        assert_eq!(match_command("co pamiętasz o muzyce").unwrap().tool, "recall");
        let r = match_command("zanotuj kupić mleko").unwrap();
        assert_eq!(r.tool, "add_note");
        assert_eq!(r.args["text"], "Kupić mleko");
        assert_eq!(match_command("co gra?").unwrap().tool, "now_playing");
        assert_eq!(match_command("Dzień dobry!").unwrap().tool, "morning_briefing");
        assert!(match_command("Dzień dobry, piszę w sprawie faktury za październik").is_none());
    }

    #[test]
    fn relative_volume() {
        let r = match_command("ścisz o 20").unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("set_volume", &json!({ "delta": -20 })));
        assert_eq!(match_command("podgłośnij o 10 procent").unwrap().args, json!({ "delta": 10 }));
        // „do N” and plain numbers stay absolute.
        assert_eq!(match_command("ścisz do 20").unwrap().args, json!({ "percent": 20 }));
        assert_eq!(match_command("głośność 30 procent").unwrap().args, json!({ "percent": 30 }));
    }

    #[test]
    fn app_verbs_for_chain_parts() {
        let r = match_app_verb("otwórz Discord").unwrap();
        assert_eq!((r.tool.as_str(), &r.args), ("open_app", &json!({ "name": "Discord" })));
        assert_eq!(match_app_verb("Zamknij Spotify").unwrap().tool, "close_app");
        assert_eq!(match_app_verb("przełącz na Chrome").unwrap().args, json!({ "name": "Chrome" }));
        assert_eq!(match_app_verb("weź odpal Visual Studio Code").unwrap().args, json!({ "name": "Visual Studio Code" }));
        assert!(match_app_verb("otwórz").is_none());
        assert!(match_app_verb("pauza").is_none());
    }

    #[test]
    fn scenes_create_and_run() {
        let r = match_command("stwórz scenę granie: otwórz Discord, FiveM i Spotify, głośność 40%").unwrap();
        assert_eq!(r.tool, "create_scene");
        assert_eq!(r.args["name"], "granie");
        let steps = r.args["steps"].as_array().unwrap();
        let apps: Vec<&str> = steps
            .iter()
            .filter(|s| s["type"] == "open_app")
            .map(|s| s["value"].as_str().unwrap())
            .collect();
        assert_eq!(apps, vec!["Discord", "FiveM", "Spotify"]);
        assert!(steps.iter().any(|s| s["type"] == "set_volume" && s["value"] == "40"));
        let r = match_command("uruchom scenę granie").unwrap();
        assert_eq!(r.tool, "run_scene");
        assert_eq!(r.args["name"], "granie");
    }
}
