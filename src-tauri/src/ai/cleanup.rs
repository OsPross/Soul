//! Dictation cleanup after Live transcription.
//!
//! The transcribe-live model runs in SMART mode (fillers/disfluencies removed server-side).
//! Here we add a cheap local pass for leftover hesitation tokens, and decide whether the text
//! contains a self-correction ("w czwartek, nie, w piątek") that needs a short text-only LLM pass.
//! Keeping the LLM out of the common path is what makes paste <1 s after release possible.

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

#[cfg(test)]
mod tests {
    use super::*;

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
