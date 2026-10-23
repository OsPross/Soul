//! Dictation cleanup after Live transcription.
//!
//! The transcribe-live model runs in SMART mode (fillers/disfluencies removed server-side).
//! Here we add a cheap local pass for leftover hesitation tokens, and decide whether the text
//! contains a self-correction ("w czwartek, nie, w piątek") that needs a short text-only LLM pass.
//! Keeping the LLM out of the common path is what makes paste <1 s after release possible.
//!
//! `prepare_dictation` applies the user's snippets (spoken trigger -> placeholder mark, filled in by
//! `finish_dictation` after any LLM pass, so the snippet text is never rewritten), word replacements
//! and custom dictionary spellings.

use crate::config::{ReplacementRule, VoiceSnippet};
use std::collections::BTreeMap;

/// Pure hesitation sounds (Polish + English). Ambiguous words ("no", "jakby", "like") are left
/// to the model — removing them locally would damage real sentences.
const FILLERS: &[&str] = &[
    "yyy", "yy", "yyyy", "eee", "ee", "eeee", "eh", "ehm", "em", "emm", "hmm", "hm", "mmm",
    "mhm", "um", "umm", "uh", "uhm", "erm", "ymm", "yhm",
];

/// Phrases that usually introduce a self-correction. Matched on lowercase text with both
/// diacritic and ASCII-folded Polish forms.
const CORRECTION_MARKERS: &[&str] = &[
    ", nie, ",
    " nie nie ",
    "to znaczy",
    "znaczy się",
    "znaczy sie",
    "a nie",
    "przepraszam",
    "poprawka",
    "właściwie",
    "wlasciwie",
    "czy raczej",
    "albo nie",
    "nie, czekaj",
    "czekaj",
    "chwila,",
    "sorry",
    "i mean",
    "no wait",
    "scratch that",
    "actually",
    "or rather",
    "wait,",
];

fn is_filler(word: &str) -> bool {
    let core: String = word
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    !core.is_empty() && FILLERS.contains(&core.as_str())
}

/// Remove standalone hesitation tokens and tidy whitespace/punctuation. Never touches
/// diacritics or real words.
pub fn local_cleanup(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        if is_filler(w) {
            // Keep sentence punctuation that was glued to the filler ("yyy." / "eee,").
            let trailing: String = w
                .chars()
                .rev()
                .take_while(|c| matches!(c, '.' | '!' | '?'))
                .collect();
            if !trailing.is_empty() {
                if let Some(last) = words.last_mut() {
                    if !last.ends_with(['.', '!', '?']) {
                        let kept = last.trim_end_matches([',', ';', ':']).len();
                        last.truncate(kept);
                        last.push_str(&trailing.chars().rev().collect::<String>());
                    }
                }
            }
            continue;
        }
        words.push(w.to_string());
    }
    let mut out = words.join(" ");
    for (from, to) in [(" ,", ","), (" .", "."), (",,", ","), (" ?", "?"), (" !", "!")] {
        while out.contains(from) {
            out = out.replace(from, to);
        }
    }
    let out = out.trim().trim_start_matches([',', ';']).trim().to_string();
    capitalize_first(&out)
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => c.to_uppercase().collect::<String>() + chars.as_str(),
        _ => s.to_string(),
    }
}

/// True when the text likely contains a spoken self-correction.
pub fn needs_llm_cleanup(text: &str) -> bool {
    let lower = format!(" {} ", text.to_lowercase());
    CORRECTION_MARKERS.iter().any(|m| lower.contains(m))
}

/// Reject LLM output that isn't a cleanup of `input` (an answer, a translation, or empty).
pub fn plausible_cleanup(input: &str, output: &str) -> bool {
    let (i, o) = (input.chars().count(), output.trim().chars().count());
    if o == 0 {
        return false;
    }
    // Cleanup only shortens or lightly fixes; long outputs mean the model answered/expanded.
    o <= i + i / 5 + 10 && o * 4 >= i
}

/// Byte ranges of the word runs (alphanumeric chars) in `s`.
fn word_spans(s: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                spans.push((st, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        spans.push((st, s.len()));
    }
    spans
}

/// Words of `s` (split on anything that is not alphanumeric).
pub fn tokens(s: &str) -> Vec<String> {
    word_spans(s).into_iter().map(|(a, b)| s[a..b].to_string()).collect()
}

/// Replace whole-word occurrences of `phrase` in `text`: case-insensitive, and any spaces /
/// hyphens / underscores between its words match ("cloud code" finds "Cloud-Code").
pub fn replacing(phrase: &str, replacement: &str, text: &str) -> String {
    let want: Vec<String> = tokens(phrase).iter().map(|t| t.to_lowercase()).collect();
    if want.is_empty() {
        return text.to_string();
    }
    let spans = word_spans(text);
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i + want.len() <= spans.len() {
        let run = &spans[i..i + want.len()];
        let words = run.iter().zip(&want).all(|(&(a, b), w)| text[a..b].to_lowercase() == *w);
        let joined = run.windows(2).all(|p| {
            text[p[0].1..p[1].0].chars().all(|c| c.is_whitespace() || c == '-' || c == '_')
        });
        if words && joined {
            out.push_str(&text[copied..run[0].0]);
            out.push_str(replacement);
            copied = run[want.len() - 1].1;
            i += want.len();
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// A dictionary entry that is safe to force onto the transcript (no symbols, not a single letter).
pub fn is_plain_word(word: &str) -> bool {
    word.chars().filter(|c| c.is_alphanumeric()).count() >= 2
        && word.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'' | '’'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDictation {
    /// Transcript with snippet marks (`⟦1⟧`…), replacements and dictionary spellings applied.
    pub text: String,
    /// Mark -> full snippet text.
    pub snippets: BTreeMap<String, String>,
}

pub fn prepare_dictation(
    text: &str,
    words: &[String],
    replacements: &[ReplacementRule],
    snippets: &[VoiceSnippet],
) -> PreparedDictation {
    let mut out = text.to_string();
    let mut marks = BTreeMap::new();
    for snippet in snippets.iter().filter(|s| !s.trigger.trim().is_empty()) {
        let mark = format!("⟦{}⟧", marks.len() + 1);
        let next = replacing(&snippet.trigger, &mark, &out);
        if next != out {
            out = next;
            marks.insert(mark, snippet.text.clone());
        }
    }
    for r in replacements.iter().filter(|r| !r.from.trim().is_empty()) {
        out = replacing(&r.from, &r.to, &out);
    }
    for word in words.iter().map(|w| w.trim()).filter(|w| is_plain_word(w)) {
        out = replacing(word, word, &out);
    }
    PreparedDictation { text: out, snippets: marks }
}

/// True iff every snippet mark survived (an LLM pass that dropped one is rejected).
pub fn keeps_marks(text: &str, snippets: &BTreeMap<String, String>) -> bool {
    snippets.keys().all(|m| text.contains(m.as_str()))
}

/// Put the snippet texts in place of their marks.
pub fn finish_dictation(text: &str, snippets: &BTreeMap<String, String>) -> String {
    snippets
        .iter()
        .fold(text.to_string(), |out, (mark, full)| out.replace(mark.as_str(), full))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(from: &str, to: &str) -> ReplacementRule {
        ReplacementRule { from: from.into(), to: to.into() }
    }

    fn snippet(trigger: &str, text: &str) -> VoiceSnippet {
        VoiceSnippet { trigger: trigger.into(), text: text.into() }
    }

    #[test]
    fn tokens_split_on_symbols_keep_diacritics() {
        assert_eq!(tokens("Zażółć, gęślą-jaźń!"), ["Zażółć", "gęślą", "jaźń"]);
        assert!(tokens(" -- ").is_empty());
    }

    #[test]
    fn replacing_whole_words_any_case_and_separator() {
        assert_eq!(replacing("cloud code", "Claude Code", "otwórz cloud code teraz"), "otwórz Claude Code teraz");
        assert_eq!(replacing("cloud code", "Claude Code", "Cloud-Code i CLOUD  code."), "Claude Code i Claude Code.");
        assert_eq!(replacing("art", "ART", "partir w czwartek, art."), "partir w czwartek, ART.");
        assert_eq!(replacing("łódź", "Łódź", "jadę do ŁÓDŹ"), "jadę do Łódź");
        assert_eq!(replacing("cloud code", "X", "cloud. code"), "cloud. code");
        assert_eq!(replacing("", "X", "abc"), "abc");
    }

    #[test]
    fn plain_words() {
        assert!(is_plain_word("Kubernetes"));
        assert!(is_plain_word("O’Neill"));
        assert!(is_plain_word("Claude Code"));
        assert!(!is_plain_word("a"));
        assert!(!is_plain_word("C++"));
    }

    #[test]
    fn prepare_and_finish_dictation() {
        let words = vec!["Supabase".to_string(), "x".to_string()];
        let rules = vec![rule("cloud code", "Claude Code")];
        let snippets = vec![
            snippet("mój podpis", "Pozdrawiam,
Oskar
— cloud code"),
            snippet("  ", "ignored"),
            snippet("adres domowy", "ul. Długa 1"),
        ];
        let p = prepare_dictation("Używam supabase i cloud code. Mój podpis", &words, &rules, &snippets);
        assert_eq!(p.text, "Używam Supabase i Claude Code. ⟦1⟧");
        assert_eq!(p.snippets.len(), 1);
        assert!(keeps_marks(&p.text, &p.snippets));
        assert!(!keeps_marks("Używam Supabase.", &p.snippets));
        // The snippet text itself is never rewritten by replacements.
        assert_eq!(
            finish_dictation(&p.text, &p.snippets),
            "Używam Supabase i Claude Code. Pozdrawiam,
Oskar
— cloud code"
        );
    }

    #[test]
    fn strips_fillers_keeps_diacritics() {
        assert_eq!(
            local_cleanup("yyy zadzwoń jutro eee do Łukasza."),
            "Zadzwoń jutro do Łukasza."
        );
        assert_eq!(local_cleanup("No to idziemy, eee."), "No to idziemy.");
    }

    #[test]
    fn detects_corrections() {
        assert!(needs_llm_cleanup("Spotkanie w czwartek, nie, w piątek o dziesiątej."));
        assert!(needs_llm_cleanup("Kup mleko, to znaczy kefir."));
        assert!(needs_llm_cleanup("Meet at three, I mean four."));
        assert!(!needs_llm_cleanup("Dzień dobry, dziękuję za wiadomość."));
    }

    #[test]
    fn rejects_answers() {
        assert!(plausible_cleanup("Spotkanie w czwartek, nie, w piątek.", "Spotkanie w piątek."));
        assert!(!plausible_cleanup(
            "Jaka jest stolica Francji?",
            "Stolicą Francji jest Paryż, miasto położone nad Sekwaną w północnej części kraju."
        ));
    }
}
