//! App discovery helpers + fuzzy matching shared by platform backends.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    /// Launch target: .lnk/.desktop path, executable, or UWP AppID (`shell:AppsFolder\…`).
    pub target: String,
    pub kind: AppKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSuggestion {
    pub name: String,
    /// Letter-avatar SVG data URL (real icons can replace later).
    pub icon_data_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Shortcut,
    #[allow(dead_code)] // set on Linux .desktop entries
    Desktop,
    Uwp,
}

/// Score how well `query` matches `candidate` (higher is better). Returns 0 if no match.
pub fn fuzzy_score(query: &str, candidate: &str) -> i32 {
    let q = normalize_name(query);
    let c = normalize_name(candidate);
    if q.is_empty() || c.is_empty() {
        return 0;
    }
    if c == q {
        return 1000;
    }
    if c.starts_with(&q) {
        return 850;
    }
    if c.contains(&q) {
        return 700;
    }
    // All query tokens present as substrings.
    let tokens: Vec<&str> = q.split_whitespace().filter(|t| !t.is_empty()).collect();
    if !tokens.is_empty() && tokens.iter().all(|t| c.contains(t)) {
        return 500 + (tokens.len() as i32 * 20);
    }
    // Compact match: "vs code" ↔ "vscode"
    let q_compact: String = q.chars().filter(|ch| !ch.is_whitespace()).collect();
    let c_compact: String = c.chars().filter(|ch| !ch.is_whitespace()).collect();
    if !q_compact.is_empty() && c_compact.contains(&q_compact) {
        return 400;
    }
    0
}

pub fn normalize_name(s: &str) -> String {
    s.chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn pick_best<'a>(query: &str, entries: &'a [AppEntry]) -> Option<&'a AppEntry> {
    let mut best: Option<(&AppEntry, i32)> = None;
    for e in entries {
        let score = fuzzy_score(query, &e.name);
        if score <= 0 {
            continue;
        }
        match best {
            None => best = Some((e, score)),
            Some((_, s)) if score > s => best = Some((e, score)),
            Some((prev, s)) if score == s && e.name.len() < prev.name.len() => {
                best = Some((e, score));
            }
            _ => {}
        }
    }
    best.map(|(e, _)| e)
}

/// Ranked autocomplete suggestions for Island Apps tab.
pub fn search_suggestions(query: &str, entries: &[AppEntry], limit: usize) -> Vec<AppSuggestion> {
    let q = query.trim();
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(&AppEntry, i32)> = entries
        .iter()
        .filter_map(|e| {
            let score = fuzzy_score(q, &e.name);
            (score > 0).then_some((e, score))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.name.len().cmp(&b.0.name.len()))
            .then_with(|| a.0.name.to_ascii_lowercase().cmp(&b.0.name.to_ascii_lowercase()))
    });
    // Deduplicate by normalized name.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (e, _) in scored {
        let key = normalize_name(&e.name);
        if !seen.insert(key) {
            continue;
        }
        out.push(AppSuggestion {
            name: e.name.clone(),
            icon_data_url: letter_icon_data_url(&e.name),
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

pub fn letter_icon_data_url(name: &str) -> String {
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .unwrap_or('?')
        .to_ascii_uppercase();
    let hue = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
        % 360;
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='64' height='64' viewBox='0 0 64 64'>\
<rect width='64' height='64' rx='14' fill='hsl({hue} 42% 38%)'/>\
<text x='32' y='42' text-anchor='middle' font-family='Segoe UI,system-ui,sans-serif' font-size='28' font-weight='600' fill='white'>{letter}</text>\
</svg>"
    );
    format!(
        "data:image/svg+xml;utf8,{}",
        urlencoding_minimal(&svg)
    )
}

fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn collect_lnk_names(dir: &Path, out: &mut Vec<AppEntry>) {
    let walk = match std::fs::read_dir(dir) {
        Ok(w) => w,
        Err(_) => return,
    };
    for entry in walk.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lnk_names(&path, out);
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
        {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            out.push(AppEntry {
                name,
                target: path.to_string_lossy().to_string(),
                kind: AppKind::Shortcut,
            });
        }
    }
}

pub fn windows_start_menu_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        dirs.push(
            PathBuf::from(program_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Ok(appdata) = std::env::var("AppData") {
        dirs.push(
            PathBuf::from(appdata)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    dirs
}

#[cfg(target_os = "linux")]
pub fn linux_desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs
}

/// Parse Name= and Exec= from a .desktop file (first group only).
#[cfg(target_os = "linux")]
pub fn parse_desktop_file(path: &Path) -> Option<AppEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut no_display = false;
    let mut in_desktop = false;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            if name.is_none() {
                name = Some(v.trim().to_string());
            }
        } else if let Some(v) = line.strip_prefix("Exec=") {
            if exec.is_none() {
                exec = Some(v.trim().to_string());
            }
        } else if line == "NoDisplay=true" || line == "Hidden=true" {
            no_display = true;
        }
    }

    if no_display {
        return None;
    }
    let name = name.filter(|n| !n.is_empty())?;
    let exec = exec.filter(|e| !e.is_empty())?;
    let exec_clean = strip_desktop_exec_field_codes(&exec);
    if exec_clean.is_empty() {
        return None;
    }
    Some(AppEntry {
        name,
        target: exec_clean,
        kind: AppKind::Desktop,
    })
}

/// Remove .desktop Exec field codes (`%u`, `%f`, …) and quotes; do not invoke a shell.
#[cfg(target_os = "linux")]
pub fn strip_desktop_exec_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    let mut in_quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                out.push(ch);
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_quote = Some(ch),
            '%' => {
                // Skip field code (optionally followed by one letter).
                if chars.peek().is_some() {
                    let _ = chars.next();
                }
            }
            _ => out.push(ch),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(target_os = "linux")]
pub fn collect_desktop_apps(out: &mut Vec<AppEntry>) {
    for dir in linux_desktop_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop_file(&path) {
                out.push(app);
            }
        }
    }
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn search_url(query: &str, site: Option<&str>) -> Result<String, String> {
    let q = percent_encode(query.trim());
    if q.is_empty() {
        return Err("Empty search query.".into());
    }
    let site = site.unwrap_or("google").trim().to_ascii_lowercase();
    let url = match site.as_str() {
        "" | "google" => format!("https://www.google.com/search?q={q}"),
        "youtube" | "yt" => youtube_search_url(query)?,
        "bing" => format!("https://www.bing.com/search?q={q}"),
        "duckduckgo" | "ddg" => format!("https://duckduckgo.com/?q={q}"),
        "github" => format!("https://github.com/search?q={q}"),
        "wiki" | "wikipedia" => format!("https://en.wikipedia.org/wiki/Special:Search?search={q}"),
        other => {
            // Treat unknown site as a Google site: filter.
            format!("https://www.google.com/search?q=site%3A{other}+{q}")
        }
    };
    Ok(url)
}

pub fn youtube_search_url(query: &str) -> Result<String, String> {
    let q = percent_encode(query.trim());
    if q.is_empty() {
        return Err("Empty YouTube search query.".into());
    }
    Ok(format!(
        "https://www.youtube.com/results?search_query={q}"
    ))
}

/// Best-effort: scrape the first `watch?v=` / `"videoId"` from YouTube results HTML.
/// Falls back to the search results URL if nothing is found.
pub fn youtube_first_result_url(query: &str) -> Result<String, String> {
    let search = youtube_search_url(query)?;
    let html = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .user_agent("Mozilla/5.0 (compatible; SoulAssistant/0.1)")
        .build()
        .and_then(|c| c.get(&search).send())
    {
        Ok(resp) => resp.text().unwrap_or_default(),
        Err(e) => {
            eprintln!("[soul][youtube_play] fetch failed: {e} — opening search");
            return Ok(search);
        }
    };

    if let Some(id) = extract_youtube_video_id(&html) {
        return Ok(format!("https://www.youtube.com/watch?v={id}"));
    }
    eprintln!("[soul][youtube_play] no video id in results — opening search");
    Ok(search)
}

fn extract_youtube_video_id(html: &str) -> Option<String> {
    fn take_id(after: &str) -> Option<String> {
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        (id.len() == 11).then_some(id)
    }

    // Prefer structured JSON blob keys YouTube embeds in the page.
    for marker in [r#""videoId":""#, r#""videoId": ""#] {
        let mut rest = html;
        while let Some(idx) = rest.find(marker) {
            let after = &rest[idx + marker.len()..];
            if let Some(id) = take_id(after) {
                return Some(id);
            }
            rest = &after[1.min(after.len())..];
        }
    }
    // Fallback: /watch?v=XXXXXXXXXXX
    let mut rest = html;
    while let Some(idx) = rest.find("/watch?v=") {
        let after = &rest[idx + "/watch?v=".len()..];
        if let Some(id) = take_id(after) {
            return Some(id);
        }
        rest = &after[1.min(after.len())..];
    }
    None
}

pub fn validate_http_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("Only http:// and https:// URLs are allowed.".into());
    }
    if url.chars().any(|c| c.is_control() || c == '`' || c == '\n' || c == '\r') {
        return Err("URL contains invalid characters.".into());
    }
    Ok(url.to_string())
}
