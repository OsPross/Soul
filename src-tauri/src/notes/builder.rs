//! Pure call-transcript building: words → speaker lines, speaker echo removal, summary parsing,
//! Markdown export. No I/O (unit-tested).

use crate::i18n::{tr, Lang};
use serde::{Deserialize, Serialize};

pub use crate::speech::{TimedWord, Turn};

/// A pause this long starts a new line.
pub const PAUSE: f64 = 1.4;
pub const MAX_LINE_SECONDS: f64 = 40.0;
pub const ME: &str = "me";

pub const SUMMARY_INSTRUCTIONS: &str = "You summarize a recorded call or meeting from its transcript \
(between <transcript> and </transcript>, one line per utterance: `Name (m:ss): text`). The transcript \
comes from speech recognition and may contain errors. Reply with JSON only, no markdown: \
{\"title\": \"short title, max 8 words\", \"summary\": \"3-6 sentences\", \"key_points\": [\"...\"], \
\"todos\": [{\"text\": \"action item\", \"owner\": \"name or null\", \"due\": \"deadline as said, or null\"}]}. \
Only list to-dos that someone actually committed to or was asked to do; use [] when there are none. \
Never invent names, numbers or decisions that are not in the transcript.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    pub id: String,
    pub name: String,
    pub color: usize,
    pub is_me: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub id: String,
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Recording,
    Processing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub title_edited: bool,
    /// Unix milliseconds.
    pub created: i64,
    /// Seconds.
    pub duration: f64,
    pub app: Option<String>,
    pub status: Status,
    pub speakers: Vec<Speaker>,
    pub lines: Vec<Line>,
    pub summary: Option<String>,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub title: String,
    pub summary: String,
    pub key_points: Vec<String>,
    pub todos: Vec<Todo>,
}

fn mid(w: &TimedWord) -> f64 {
    (w.start + w.end) / 2.0
}

/// Speaker of each word: the turn covering its midpoint, else the nearest turn within 2 s.
pub fn assign(words: &[TimedWord], turns: &[Turn], fallback: &str) -> Vec<(String, TimedWord)> {
    words
        .iter()
        .map(|w| {
            let m = mid(w);
            let covering = turns.iter().find(|t| t.start <= m && m <= t.end);
            let nearest = || {
                turns
                    .iter()
                    .map(|t| ((t.start - m).max(m - t.end), t))
                    .filter(|(d, _)| *d <= 2.0)
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, t)| t)
            };
            let speaker = covering.or_else(nearest).map(|t| t.speaker.as_str()).unwrap_or(fallback);
            (speaker.to_string(), w.clone())
        })
        .collect()
}

/// Consecutive words of one speaker joined into lines (split on a pause or after 40 s).
pub fn lines(words: &[(String, TimedWord)]) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for (speaker, w) in words {
        match out.last_mut() {
            Some(l)
                if &l.speaker == speaker
                    && w.start - l.end < PAUSE
                    && w.end - l.start <= MAX_LINE_SECONDS =>
            {
                l.text.push(' ');
                l.text.push_str(&w.text);
                l.end = w.end;
            }
            _ => out.push(Line {
                id: format!("l{}", out.len()),
                speaker: speaker.clone(),
                start: w.start,
                end: w.end,
                text: w.text.clone(),
            }),
        }
    }
    out
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|t| t.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

fn overlaps(a: &Line, b: &Line, margin: f64) -> bool {
    a.start - margin <= b.end && b.start <= a.end + margin
}

/// Drop mic lines that are just the other side's voice leaking from the speakers into the mic.
pub fn removing_echo(mine: &[Line], others: &[Line]) -> Vec<Line> {
    mine.iter()
        .filter(|line| {
            let words = tokens(&line.text);
            if words.len() < 2 {
                let Some(word) = words.first() else {
                    return false;
                };
                return !others
                    .iter()
                    .any(|o| overlaps(line, o, 0.8) && tokens(&o.text).contains(word));
            }
            let theirs: std::collections::HashSet<String> = others
                .iter()
                .filter(|o| overlaps(line, o, 1.5))
                .flat_map(|o| tokens(&o.text))
                .collect();
            let shared = words.iter().filter(|w| theirs.contains(*w)).count();
            (shared as f64) / (words.len() as f64) < 0.6
        })
        .cloned()
        .collect()
}

/// Diarization clusters → stable ids in order of first appearance (`p{first}`, `p{first+1}`…).
fn number_speakers(
    assigned: Vec<(String, TimedWord)>,
    first: usize,
    person_name: &str,
) -> (Vec<Speaker>, Vec<(String, TimedWord)>) {
    let mut order: Vec<String> = Vec::new();
    let renamed = assigned
        .into_iter()
        .map(|(cluster, w)| {
            let i = order.iter().position(|c| *c == cluster).unwrap_or_else(|| {
                order.push(cluster.clone());
                order.len() - 1
            });
            (format!("p{}", first + i), w)
        })
        .collect();
    let speakers = (0..order.len())
        .map(|i| Speaker {
            id: format!("p{}", first + i),
            name: format!("{person_name} {}", first + i),
            color: first + i - 1,
            is_me: false,
        })
        .collect();
    (speakers, renamed)
}

fn finish(mut lines: Vec<Line>) -> Vec<Line> {
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    for (i, l) in lines.iter_mut().enumerate() {
        l.id = format!("l{i}");
    }
    lines
}

/// Two channels: the mic is "me", the system channel is split by its diarization `turns`.
/// Fewer than 5 system words = nothing came through the speakers, so mic only.
pub fn build(
    mine: &[TimedWord],
    others: &[TimedWord],
    turns: &[Turn],
    me_name: &str,
    person_name: &str,
) -> (Vec<Speaker>, Vec<Line>) {
    let me = Speaker { id: ME.into(), name: me_name.into(), color: 0, is_me: true };
    let mine_lines = lines(&mine.iter().map(|w| (ME.to_string(), w.clone())).collect::<Vec<_>>());
    if others.len() < 5 {
        let speakers = if mine_lines.is_empty() { Vec::new() } else { vec![me] };
        return (speakers, finish(mine_lines));
    }
    let fallback = turns.first().map(|t| t.speaker.clone()).unwrap_or_else(|| "0".into());
    let (mut speakers, theirs) = number_speakers(assign(others, turns, &fallback), 2, person_name);
    let other_lines = lines(&theirs);
    let mut all = removing_echo(&mine_lines, &other_lines);
    if !all.is_empty() {
        speakers.insert(0, me);
    }
    all.extend(other_lines);
    (speakers, finish(all))
}

/// One mic in a room with several voices: every voice is "Person N".
pub fn build_room(words: &[TimedWord], turns: &[Turn], person_name: &str) -> (Vec<Speaker>, Vec<Line>) {
    let fallback = turns.first().map(|t| t.speaker.clone()).unwrap_or_else(|| "0".into());
    let (speakers, assigned) = number_speakers(assign(words, turns, &fallback), 1, person_name);
    (speakers, finish(lines(&assigned)))
}

fn opt_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "null").map(str::to_string)
}

/// The model's JSON summary, also when wrapped in ```json fences or surrounded by prose.
pub fn parse_summary(reply: &str) -> Option<Summary> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let list = |key: &str| v.get(key).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let todos = list("todos")
        .iter()
        .filter_map(|t| {
            let (text, owner, due) = match t {
                serde_json::Value::String(s) => (Some(s.trim().to_string()), None, None),
                _ => (
                    t.get("text").and_then(opt_str),
                    t.get("owner").and_then(opt_str),
                    t.get("due").and_then(opt_str),
                ),
            };
            Some((text.filter(|s| !s.is_empty())?, owner, due))
        })
        .enumerate()
        .map(|(i, (text, owner, due))| Todo { id: format!("t{i}"), text, owner, due, done: false })
        .collect();
    Some(Summary {
        title: v.get("title").and_then(opt_str).unwrap_or_default(),
        summary: v.get("summary").and_then(opt_str)?,
        key_points: list("key_points").iter().filter_map(opt_str).collect(),
        todos,
    })
}

pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Transcript as `Name (m:ss): text` lines (summary prompt + Markdown).
pub fn transcript_text(note: &Note) -> String {
    note.lines
        .iter()
        .map(|l| {
            let name = note
                .speakers
                .iter()
                .find(|s| s.id == l.speaker)
                .map(|s| s.name.as_str())
                .unwrap_or(&l.speaker);
            format!("{name} ({}): {}", clock(l.start), l.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn markdown(note: &Note, lang: Lang) -> String {
    let mut md = format!("# {}\n\n", note.title);
    let date = chrono::DateTime::from_timestamp_millis(note.created)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    md.push_str(&format!("{date} · {}", clock(note.duration)));
    if let Some(app) = &note.app {
        md.push_str(&format!(" · {app}"));
    }
    md.push_str("\n\n");
    if !note.speakers.is_empty() {
        let names: Vec<&str> = note.speakers.iter().map(|s| s.name.as_str()).collect();
        md.push_str(&format!("{}: {}\n\n", tr(lang, "Uczestnicy", "Speakers"), names.join(", ")));
    }
    if let Some(summary) = &note.summary {
        md.push_str(&format!("## {}\n\n{summary}\n\n", tr(lang, "Podsumowanie", "Summary")));
    }
    if !note.key_points.is_empty() {
        md.push_str(&format!("## {}\n\n", tr(lang, "Kluczowe punkty", "Key points")));
        for p in &note.key_points {
            md.push_str(&format!("- {p}\n"));
        }
        md.push('\n');
    }
    if !note.todos.is_empty() {
        md.push_str(&format!("## {}\n\n", tr(lang, "Do zrobienia", "To do")));
        for t in &note.todos {
            let mut line = format!("- [{}] {}", if t.done { "x" } else { " " }, t.text);
            if let Some(o) = &t.owner {
                line.push_str(&format!(" — {o}"));
            }
            if let Some(d) = &t.due {
                line.push_str(&format!(" ({d})"));
            }
            md.push_str(&line);
            md.push('\n');
        }
        md.push('\n');
    }
    if !note.lines.is_empty() {
        md.push_str(&format!("## {}\n\n", tr(lang, "Transkrypcja", "Transcript")));
        for l in &note.lines {
            let name = note
                .speakers
                .iter()
                .find(|s| s.id == l.speaker)
                .map(|s| s.name.as_str())
                .unwrap_or(&l.speaker);
            md.push_str(&format!("**{name}** ({}) : {}\n\n", clock(l.start), l.text));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64) -> TimedWord {
        TimedWord { text: text.into(), start, end: start + 0.3 }
    }
    fn turn(s: &str, start: f64, end: f64) -> Turn {
        Turn { speaker: s.into(), start, end }
    }
    fn line(speaker: &str, start: f64, end: f64, text: &str) -> Line {
        Line { id: String::new(), speaker: speaker.into(), start, end, text: text.into() }
    }

    #[test]
    fn assign_uses_covering_then_nearest_turn() {
        let turns = [turn("0", 0.0, 2.0), turn("1", 3.0, 5.0)];
        let words = [w("a", 1.0), w("b", 3.5), w("c", 5.5), w("d", 20.0)];
        let got: Vec<String> = assign(&words, &turns, "x").into_iter().map(|(s, _)| s).collect();
        assert_eq!(got, ["0", "1", "1", "x"]);
    }

    #[test]
    fn lines_split_on_pause_speaker_and_length() {
        let words: Vec<(String, TimedWord)> = vec![
            ("me".into(), w("Cześć", 0.0)),
            ("me".into(), w("Kacper", 0.5)),
            ("me".into(), w("potem", 3.0)),
            ("p2".into(), w("hej", 3.4)),
        ];
        let l = lines(&words);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].text, "Cześć Kacper");
        assert_eq!(l[1].text, "potem");
        let long: Vec<(String, TimedWord)> = (0..100).map(|i| ("me".into(), w("x", i as f64))).collect();
        assert!(lines(&long).iter().all(|l| l.end - l.start <= MAX_LINE_SECONDS));
    }

    #[test]
    fn echo_lines_are_removed() {
        let others = [line("p2", 10.0, 13.0, "Wyślij mi raport do piątku proszę")];
        let mine = [
            line("me", 10.4, 13.2, "wyślij mi raport do piątku"),
            line("me", 11.0, 12.0, "dobra"),
            line("me", 10.5, 10.8, "proszę"),
            line("me", 20.0, 22.0, "wyślij mi raport"),
        ];
        let kept = removing_echo(&mine, &others);
        let texts: Vec<&str> = kept.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["dobra", "wyślij mi raport"]);
    }

    #[test]
    fn build_numbers_speakers_and_falls_back_to_mic() {
        let mine = [w("Cześć", 0.0), w("wszystkim", 0.4)];
        let others: Vec<TimedWord> = (0..6).map(|i| w("tak", 5.0 + i as f64 * 2.0)).collect();
        let turns = [turn("3", 4.0, 9.0), turn("1", 9.0, 20.0)];
        let (speakers, lines) = build(&mine, &others, &turns, "Ja", "Osoba");
        let ids: Vec<&str> = speakers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["me", "p2", "p3"]);
        assert_eq!(speakers[1].name, "Osoba 2");
        assert_eq!(lines[0].speaker, "me");
        assert!(lines.windows(2).all(|p| p[0].start <= p[1].start));
        assert_eq!(lines[1].id, "l1");

        let (speakers, lines) = build(&mine, &others[..2], &turns, "Ja", "Osoba");
        assert_eq!(speakers.len(), 1);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn build_room_names_every_voice() {
        let words = [w("a", 0.0), w("b", 5.0), w("c", 10.0)];
        let turns = [turn("7", 0.0, 1.0), turn("2", 4.0, 6.0), turn("7", 9.0, 11.0)];
        let (speakers, lines) = build_room(&words, &turns, "Person");
        assert_eq!(speakers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Person 1", "Person 2"]);
        assert_eq!(lines.iter().map(|l| l.speaker.as_str()).collect::<Vec<_>>(), ["p1", "p2", "p1"]);
    }

    #[test]
    fn summary_json_is_extracted() {
        let reply = "```json\n{\"title\":\"Sprint\",\"summary\":\"Omówiono plan.\",\"key_points\":[\"A\",\"\"],\
\"todos\":[{\"text\":\"Wysłać raport\",\"owner\":\"Kacper\",\"due\":null},{\"text\":\"\"},\"Zadzwonić\"]}\n```";
        let s = parse_summary(reply).unwrap();
        assert_eq!(s.title, "Sprint");
        assert_eq!(s.key_points, ["A"]);
        assert_eq!(s.todos.len(), 2);
        assert_eq!(s.todos[0].owner.as_deref(), Some("Kacper"));
        assert_eq!(s.todos[0].due, None);
        assert_eq!(s.todos[1].id, "t1");
        assert!(parse_summary("no json").is_none());
    }

    #[test]
    fn markdown_has_sections() {
        let note = Note {
            id: "n".into(),
            title: "Call".into(),
            title_edited: false,
            created: 0,
            duration: 75.0,
            app: Some("Discord".into()),
            status: Status::Ready,
            speakers: vec![Speaker { id: "me".into(), name: "Ja".into(), color: 0, is_me: true }],
            lines: vec![line("me", 75.0, 76.0, "hej")],
            summary: Some("S".into()),
            key_points: vec!["K".into()],
            todos: vec![Todo { id: "t0".into(), text: "T".into(), owner: None, due: None, done: true }],
            error: None,
        };
        let md = markdown(&note, Lang::Pl);
        for part in ["# Call", "1:15", "## Podsumowanie", "## Kluczowe punkty", "- [x] T", "**Ja** (1:15) : hej"] {
            assert!(md.contains(part), "missing {part}");
        }
        assert!(markdown(&note, Lang::En).contains("## Transcript"));
    }
}
