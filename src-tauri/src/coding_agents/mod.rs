//! Coding agents (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI…) → Soul.
//!
//! The agents run `soul --agent-hook` as a hook (`relay.rs`, a short-lived process that never
//! blocks the agent), which forwards the event over a same-user named pipe / Unix socket
//! (`pipe.rs`) to the running app: Island live sessions, approval / question cards (click or a
//! spoken „tak” / „nie”), the terminal jump (`session_window.rs`) and the weekly recap (`recap.rs`).
//! `installer.rs` writes the hooks into each agent's config (diff preview, dated backup).

pub mod installer;
pub mod pipe;
pub mod recap;
pub mod relay;
pub mod session_window;

/// Longest common subsequence line diff: `' '` kept, `'-'` removed, `'+'` added.
/// `None` when the table would be too big (> 1 M cells after trimming the common ends).
pub fn line_diff<'a>(a: &[&'a str], b: &[&'a str]) -> Option<Vec<(char, &'a str)>> {
    const MAX_CELLS: usize = 1_000_000;
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (m, n) = (am.len(), bm.len());
    if (m + 1).saturating_mul(n + 1) > MAX_CELLS {
        return None;
    }
    // lcs[i][j] = LCS length of am[i..] and bm[j..]
    let w = n + 1;
    let mut lcs = vec![0u32; (m + 1) * w];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            lcs[i * w + j] = if am[i] == bm[j] { lcs[(i + 1) * w + j + 1] + 1 } else { lcs[(i + 1) * w + j].max(lcs[i * w + j + 1]) };
        }
    }
    let mut out: Vec<(char, &str)> = a[..pre].iter().map(|l| (' ', *l)).collect();
    let (mut i, mut j) = (0, 0);
    while i < m || j < n {
        if i < m && j < n && am[i] == bm[j] {
            out.push((' ', am[i]));
            i += 1;
            j += 1;
        } else if i < m && (j == n || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            out.push(('-', am[i]));
            i += 1;
        } else {
            out.push(('+', bm[j]));
            j += 1;
        }
    }
    out.extend(a[a.len() - suf..].iter().map(|l| (' ', *l)));
    Some(out)
}

/// `(added, removed)` line counts between two texts (all-replaced when too big to diff).
pub fn line_counts(old: &str, new: &str) -> (u64, u64) {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    match line_diff(&a, &b) {
        Some(ops) => (
            ops.iter().filter(|(c, _)| *c == '+').count() as u64,
            ops.iter().filter(|(c, _)| *c == '-').count() as u64,
        ),
        None => (b.len() as u64, a.len() as u64),
    }
}

/// Unified diff with 3 lines of context (empty when nothing changed).
pub fn unified_diff(old: &str, new: &str, name: &str) -> String {
    const CONTEXT: usize = 3;
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let ops = line_diff(&a, &b)
        .unwrap_or_else(|| a.iter().map(|l| ('-', *l)).chain(b.iter().map(|l| ('+', *l))).collect());
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, (c, _))| *c != ' ').map(|(i, _)| i).collect();
    if changed.is_empty() {
        return String::new();
    }
    let mut out = format!("--- {name}\n+++ {name}\n");
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(CONTEXT);
        while k + 1 < changed.len() && changed[k + 1] <= changed[k] + 2 * CONTEXT + 1 {
            k += 1;
        }
        let end = (changed[k] + CONTEXT + 1).min(ops.len());
        let line_no = |upto: usize, skip: char| ops[..upto].iter().filter(|(c, _)| *c != skip).count();
        let count = |skip: char| ops[start..end].iter().filter(|(c, _)| *c != skip).count();
        out += &format!(
            "@@ -{},{} +{},{} @@\n",
            line_no(start, '+') + 1,
            count('+'),
            line_no(start, '-') + 1,
            count('-')
        );
        for (c, l) in &ops[start..end] {
            out += &format!("{c}{l}\n");
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcs_diff_and_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        assert_eq!(line_counts(old, new), (2, 1));
        let d = unified_diff(old, new, "x.txt");
        assert!(d.starts_with("--- x.txt\n+++ x.txt\n@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n"), "{d}");
        assert!(d.contains("@@ -8,3 +8,4 @@\n h\n i\n j\n+k\n"), "{d}");
        assert_eq!(unified_diff(old, old, "x"), "");
        assert_eq!(line_counts("", "x\ny"), (2, 0));
    }
}
