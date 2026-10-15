//! Chained commands: „zamknij Spotify i otwórz Discord”, „pauza i ścisz o 20”, „włącz scenę
//! granie, a potem ustaw timer na 30 minut”. Split on Polish/English connectors only where every
//! part is confidently a command on its own; anything else stays one command (the model may then
//! still return several function calls). Dictation never reaches this module.

use super::router::fold_word;
use serde_json::Value;

/// Never run more than this many actions for one utterance.
pub const MAX_ACTIONS: usize = 4;

/// Connector at `words[i..]` (ASCII-folded): its length in words.
fn connector_len(folded: &[String], i: usize) -> Option<usize> {
    let w = folded[i].as_str();
    let next = folded.get(i + 1).map(String::as_str).unwrap_or("");
    match (w, next) {
        ("a" | "i" | "and", "potem" | "nastepnie" | "then") => Some(2),
        ("i" | "oraz" | "potem" | "nastepnie" | "and" | "then", _) => Some(1),
        _ => None,
    }
}

/// Split into ≥ 2 command parts (original words, punctuation trimmed), or None.
/// `is_command` decides whether a part stands on its own; parts that don't are glued back
/// onto the previous one („timer na godzinę i 20 minut” stays whole).
pub fn split_with(text: &str, is_command: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 {
        return None;
    }
    let folded: Vec<String> = words.iter().map(|w| fold_word(w)).collect();
    // (separator before the segment, segment words)
    let mut segments: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    let mut i = 0;
    while i < words.len() {
        let at_start = segments.last().is_some_and(|(_, s)| s.is_empty());
        if !at_start {
            if let Some(n) = connector_len(&folded, i) {
                let sep = words[i..i + n].join(" ");
                segments.push((format!(" {sep} "), Vec::new()));
                i += n;
                continue;
            }
        } else if segments.len() > 1 {
            // „…, a potem …”: a connector right after a comma belongs to the comma.
            if let Some(n) = connector_len(&folded, i) {
                let (sep, _) = segments.last_mut().unwrap();
                *sep = format!("{} {} ", sep.trim_end(), words[i..i + n].join(" "));
                i += n;
                continue;
            }
        }
        let w = words[i];
        let trimmed = w.trim_end_matches([',', ';']);
        if !trimmed.is_empty() {
            segments.last_mut().unwrap().1.push(trimmed);
        }
        if w.ends_with([',', ';']) && i + 1 < words.len() {
            segments.push((", ".into(), Vec::new()));
        }
        i += 1;
    }

    let mut parts: Vec<String> = Vec::new();
    for (sep, seg) in segments.into_iter().filter(|(_, s)| !s.is_empty()) {
        let text = seg.join(" ");
        let clean = text.trim_end_matches(['.', '!', '?']).trim().to_string();
        match parts.last_mut() {
            Some(prev) if !is_command(&clean) => {
                prev.push_str(&sep);
                prev.push_str(&clean);
            }
            _ => parts.push(clean),
        }
    }
    (parts.len() >= 2 && parts.iter().all(|p| is_command(p))).then_some(parts)
}

/// Short Polish result for the combined Island line: „Spotify zamknięty”, „Pauza”, „Głośność 40%”.
pub fn step_label(tool: &str, args: &Value, result: &str) -> String {
    let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
    let short = |s: &str| {
        let s = s.trim();
        if s.chars().count() > 40 {
            format!("{}…", s.chars().take(39).collect::<String>())
        } else {
            s.to_string()
        }
    };
    match tool {
        "open_app" => format!("{} otwarty", capitalize(name)),
        "close_app" => format!("{} zamknięty", capitalize(name)),
        "focus_app" => format!("{} na wierzchu", capitalize(name)),
        "media_control" => match args.get("action").and_then(Value::as_str).unwrap_or_default() {
            "pause" => "Pauza".into(),
            "play" => "Wznowiono".into(),
            "next" => "Następny utwór".into(),
            "prev" | "previous" => "Poprzedni utwór".into(),
            other => format!("Media: {other}"),
        },
        "set_volume" => {
            // "Volume set to 40%" → „Głośność 40%”
            let pct: String = result.chars().filter(|c| c.is_ascii_digit()).collect();
            if pct.is_empty() {
                short(result)
            } else {
                format!("Głośność {pct}%")
            }
        }
        "run_scene" => format!(
            "Scena {}",
            args.get("name").and_then(Value::as_str).unwrap_or_default()
        ),
        _ => short(result),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// „✓ Spotify zamknięty · ✓ Discord otwarty” (+ „✕ …” for the failed step, „– N pominięte”).
pub fn combine(done: &[(bool, String)], skipped: usize) -> String {
    let mut out: Vec<String> = done
        .iter()
        .map(|(ok, label)| format!("{} {label}", if *ok { "✓" } else { "✕" }))
        .collect();
    if skipped > 0 {
        out.push(format!("– pominięte: {skipped}"));
    }
    out.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The runtime predicate without scene triggers (no database in unit tests).
    fn split(text: &str) -> Option<Vec<String>> {
        split_with(text, &|p| super::super::part_is_command(p, false))
    }

    #[test]
    fn splits_confident_commands() {
        assert_eq!(
            split("Zamknij Spotify i otwórz Discord.").unwrap(),
            vec!["Zamknij Spotify", "otwórz Discord"]
        );
        assert_eq!(split("pauza i ścisz o 20").unwrap(), vec!["pauza", "ścisz o 20"]);
        assert_eq!(
            split("Włącz scenę granie, a potem ustaw timer na 30 minut.").unwrap(),
            vec!["Włącz scenę granie", "ustaw timer na 30 minut"]
        );
        assert_eq!(
            split("open Spotify and then pause").unwrap(),
            vec!["open Spotify", "pause"]
        );
        assert_eq!(
            split("następna piosenka, potem głośność 40 procent").unwrap(),
            vec!["następna piosenka", "głośność 40 procent"]
        );
    }

    #[test]
    fn keeps_single_commands_whole() {
        for t in [
            "timer 1 godzina i 20 minut",
            "otwórz Discord i Spotify",
            "zanotuj kupić mleko i chleb",
            "zapamiętaj, że lubię kawę i herbatę",
            "uruchom scenę praca i nauka",
            "wyszukaj przepis na pierogi i barszcz",
            "otwórz Spotify",
        ] {
            assert_eq!(split(t), None, "{t}");
        }
    }

    #[test]
    fn dictation_is_never_split() {
        // Even if it reached the splitter: the parts are not commands.
        for t in [
            "Kup mleko, to znaczy kefir i chleb.",
            "Spotkanie przenosimy na piątek i potem idziemy na obiad.",
            "Let's meet tomorrow and then go home.",
        ] {
            assert_eq!(split(t), None, "{t}");
        }
    }

    #[test]
    fn combined_line() {
        let labels = vec![
            (true, step_label("close_app", &json!({ "name": "spotify" }), "Closed Spotify")),
            (true, step_label("open_app", &json!({ "name": "Discord" }), "Opened Discord")),
        ];
        assert_eq!(combine(&labels, 0), "✓ Spotify zamknięty · ✓ Discord otwarty");
        assert_eq!(
            step_label("set_volume", &json!({ "delta": -20 }), "Volume set to 40%"),
            "Głośność 40%"
        );
        assert_eq!(
            combine(&[(true, "Pauza".into()), (false, "Brak aplikacji".into())], 1),
            "✓ Pauza · ✕ Brak aplikacji · – pominięte: 1"
        );
    }
}
