//! Local long-term memory: one SQLite file (`soul-memory.db` in the app data dir) with remembered
//! facts + voice notes, scenes and the app-launch history used for scene suggestions.
//!
//! One global connection behind a mutex; every call is a few short statements, so callers on
//! worker threads (agent, Tauri async commands) never wait long. Nothing here touches the UI thread.
//! Facts are injected into system prompts (keyword + recency, max 15) — never secrets.

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

const DB_FILE: &str = "soul-memory.db";
/// Facts injected into a system prompt.
pub const PROMPT_FACTS: usize = 15;
pub const NOTE_CATEGORY: &str = "notatka";

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: i64,
    pub text: String,
    pub category: String,
    /// `voice` | `auto` | `manual`
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: i64,
    pub use_count: i64,
}

/// Open (or create) the database in the app data dir. Safe to call once at startup.
pub fn init(app: &AppHandle) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("App data dir unavailable: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let conn = Connection::open(dir.join(DB_FILE)).map_err(|e| format!("Memory DB: {e}"))?;
    migrate(&conn).map_err(|e| format!("Memory DB schema: {e}"))?;
    let _ = DB.set(Mutex::new(conn));
    eprintln!("[soul][memory] ready facts={}", count().unwrap_or(0));
    Ok(())
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS facts (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             text TEXT NOT NULL,
             category TEXT NOT NULL DEFAULT 'inne',
             source TEXT NOT NULL DEFAULT 'manual',
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             last_used_at INTEGER NOT NULL DEFAULT 0,
             use_count INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS scenes (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL UNIQUE,
             triggers TEXT NOT NULL DEFAULT '[]',
             steps TEXT NOT NULL DEFAULT '[]',
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS app_launches (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             app TEXT NOT NULL,
             at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS dismissed (key TEXT PRIMARY KEY);",
    )
}

/// Run `f` on the shared connection.
pub(crate) fn with_db<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
    let db = DB.get().ok_or_else(|| "Memory is not available.".to_string())?;
    let conn = db.lock();
    f(&conn).map_err(|e| format!("Memory DB: {e}"))
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn row_fact(r: &rusqlite::Row) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: r.get(0)?,
        text: r.get(1)?,
        category: r.get(2)?,
        source: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        last_used_at: r.get(6)?,
        use_count: r.get(7)?,
    })
}

const FACT_COLS: &str =
    "id, text, category, source, created_at, updated_at, last_used_at, use_count";

fn all_facts(conn: &Connection) -> rusqlite::Result<Vec<Fact>> {
    let mut st = conn.prepare(&format!("SELECT {FACT_COLS} FROM facts ORDER BY updated_at DESC"))?;
    let rows = st.query_map([], row_fact)?;
    rows.collect()
}

pub fn count() -> Result<i64, String> {
    with_db(|c| c.query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0)))
}

/// Never store secrets (passwords, tokens, PINs) — same rule as clipboard history.
pub fn is_secret(text: &str) -> bool {
    let folded = fold(text);
    crate::island::clipboard::looks_like_secret(text)
        || ["haslo", "password", "pin do", "pin to", "kod pin", "token", "api key", "klucz api"]
            .iter()
            .any(|w| folded.contains(w))
}

/// Add a fact (deduplicated by its folded text). Returns the fact and whether it is new.
pub fn add_fact(text: &str, category: &str, source: &str) -> Result<(Fact, bool), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Pusty fakt.".into());
    }
    if is_secret(text) {
        return Err("Nie zapamiętuję haseł ani kluczy.".into());
    }
    let category = normalize_category(category);
    let key = fold(text);
    with_db(|c| {
        let now = now_ms();
        for f in all_facts(c)? {
            if fold(&f.text) == key {
                c.execute("UPDATE facts SET updated_at=?1 WHERE id=?2", params![now, f.id])?;
                return Ok((Fact { updated_at: now, ..f }, false));
            }
        }
        c.execute(
            "INSERT INTO facts (text, category, source, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![text, category, source, now],
        )?;
        let id = c.last_insert_rowid();
        let fact = c.query_row(
            &format!("SELECT {FACT_COLS} FROM facts WHERE id=?1"),
            [id],
            row_fact,
        )?;
        Ok((fact, true))
    })
    .inspect(|(f, new)| {
        eprintln!(
            "[soul][memory] add id={} category={} source={source} new={new} chars={}",
            f.id,
            f.category,
            f.text.chars().count()
        )
    })
}

pub fn update_fact(id: i64, text: &str, category: &str) -> Result<Fact, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Pusty fakt.".into());
    }
    if is_secret(text) {
        return Err("Nie zapamiętuję haseł ani kluczy.".into());
    }
    let category = normalize_category(category);
    with_db(|c| {
        c.execute(
            "UPDATE facts SET text=?1, category=?2, updated_at=?3 WHERE id=?4",
            params![text, category, now_ms(), id],
        )?;
        c.query_row(&format!("SELECT {FACT_COLS} FROM facts WHERE id=?1"), [id], row_fact)
    })
}

pub fn delete_fact(id: i64) -> Result<(), String> {
    with_db(|c| c.execute("DELETE FROM facts WHERE id=?1", [id]).map(|_| ()))?;
    eprintln!("[soul][memory] delete id={id}");
    Ok(())
}

pub fn clear_all() -> Result<(), String> {
    with_db(|c| c.execute("DELETE FROM facts", []).map(|_| ()))?;
    eprintln!("[soul][memory] cleared");
    Ok(())
}

pub fn list(category: Option<&str>) -> Result<Vec<Fact>, String> {
    let facts = with_db(all_facts)?;
    Ok(match category.map(str::trim).filter(|c| !c.is_empty()) {
        Some(cat) => facts.into_iter().filter(|f| f.category == cat).collect(),
        None => facts,
    })
}

/// Keyword search (empty query → most recent). Marks the hits as used.
pub fn search(query: &str, limit: usize) -> Result<Vec<Fact>, String> {
    let facts = with_db(all_facts)?;
    let hits: Vec<Fact> = if tokens(query).is_empty() {
        facts.into_iter().take(limit).collect()
    } else {
        rank(&facts, Some(query), now_ms())
            .into_iter()
            .filter(|(score, _)| *score >= 1.0)
            .take(limit)
            .map(|(_, f)| f)
            .collect()
    };
    touch(&hits);
    Ok(hits)
}

/// Delete facts that match every keyword of `query`. Returns what was removed.
pub fn forget(query: &str) -> Result<Vec<Fact>, String> {
    let want = tokens(query);
    if want.is_empty() {
        return Ok(Vec::new());
    }
    let facts = with_db(all_facts)?;
    let doomed: Vec<Fact> = facts
        .into_iter()
        .filter(|f| keyword_hits(&want, &f.text) == want.len())
        .collect();
    for f in &doomed {
        delete_fact(f.id)?;
    }
    eprintln!("[soul][memory] forget removed={}", doomed.len());
    Ok(doomed)
}

fn touch(facts: &[Fact]) {
    if facts.is_empty() {
        return;
    }
    let now = now_ms();
    let _ = with_db(|c| {
        for f in facts {
            c.execute(
                "UPDATE facts SET last_used_at=?1, use_count=use_count+1 WHERE id=?2",
                params![now, f.id],
            )?;
        }
        Ok(())
    });
}

/// Most relevant facts for a prompt: keyword hits first, then the freshest ones (max 15).
pub fn relevant(query: Option<&str>, limit: usize) -> Vec<Fact> {
    let Ok(facts) = with_db(all_facts) else {
        return Vec::new();
    };
    rank(&facts, query, now_ms())
        .into_iter()
        .take(limit)
        .map(|(_, f)| f)
        .collect()
}

/// System-prompt block with remembered facts ("" when memory is empty / unavailable).
/// `None` (the warm Live session) = the 15 freshest facts in a stable order, so the prompt only
/// changes when memory changes (a changed prompt reconnects the Live session).
pub fn prompt_facts(query: Option<&str>) -> String {
    if DB.get().is_none() {
        return String::new();
    }
    match query {
        Some(q) => format_prompt(&relevant(Some(q), PROMPT_FACTS)),
        None => {
            let Ok(facts) = with_db(all_facts) else {
                return String::new();
            };
            format_prompt(&stable_top(facts, PROMPT_FACTS))
        }
    }
}

/// Freshest `limit` facts (by last edit, never by use), listed oldest-first by id.
fn stable_top(mut facts: Vec<Fact>, limit: usize) -> Vec<Fact> {
    facts.sort_by_key(|f| std::cmp::Reverse(f.updated_at));
    facts.truncate(limit);
    facts.sort_by_key(|f| f.id);
    facts
}

fn format_prompt(facts: &[Fact]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        " Known facts about the user from Soul's local memory (use them when relevant; never read the whole list aloud):",
    );
    for f in facts {
        out.push_str(&format!("\n- {} [{}]", f.text, f.category));
    }
    out
}

/// Score = keyword hits (×10) + recency (decays over ~30 days) + a little for frequent use.
fn rank(facts: &[Fact], query: Option<&str>, now: i64) -> Vec<(f64, Fact)> {
    let want = query.map(tokens).unwrap_or_default();
    let mut scored: Vec<(f64, Fact)> = facts
        .iter()
        .map(|f| {
            let hits = keyword_hits(&want, &f.text) as f64;
            let last = f.updated_at.max(f.last_used_at);
            let age_days = ((now - last).max(0) as f64) / 86_400_000.0;
            let recency = (-age_days / 30.0).exp();
            let usage = (f.use_count.min(10) as f64) / 20.0;
            (hits * 10.0 + recency + usage, f.clone())
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

const STOP: &[&str] = &[
    "jest", "jestem", "oraz", "albo", "ktory", "ktora", "ktore", "moja", "moje", "moj", "mojej",
    "mnie", "mam", "sie", "dla", "the", "and", "that", "this", "with", "jak", "czy", "nie", "tak",
    "ten", "tego", "jaka", "jaki", "jakie", "pamietasz", "wiesz", "about",
];

fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .split_whitespace()
        .map(fold)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .collect();
    out.dedup();
    out
}

/// Polish is heavily inflected: compare 4-letter stems ("muzyce" ~ "muzyka", "serwerze" ~ "serwer").
fn stem(w: &str) -> String {
    w.chars().take(4).collect()
}

fn keyword_hits(want: &[String], text: &str) -> usize {
    if want.is_empty() {
        return 0;
    }
    let have: Vec<String> = text.split_whitespace().map(fold).map(|w| stem(&w)).collect();
    want.iter().filter(|w| have.contains(&stem(w))).count()
}

fn fold(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .map(|c| match c {
            'ą' | 'Ą' => 'a',
            'ć' | 'Ć' => 'c',
            'ę' | 'Ę' => 'e',
            'ł' | 'Ł' => 'l',
            'ń' | 'Ń' => 'n',
            'ó' | 'Ó' => 'o',
            'ś' | 'Ś' => 's',
            'ź' | 'Ź' | 'ż' | 'Ż' => 'z',
            other => other.to_ascii_lowercase(),
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub const CATEGORIES: &[&str] = &["gry", "muzyka", "praca", "osobiste", "preferencje", NOTE_CATEGORY, "inne"];

fn normalize_category(c: &str) -> String {
    let f = fold(c);
    match f.as_str() {
        "games" | "gaming" | "gra" => "gry".into(),
        "music" => "muzyka".into(),
        "work" => "praca".into(),
        "personal" => "osobiste".into(),
        "preferences" | "preferencja" => "preferencje".into(),
        "note" | "notes" | "notatki" => NOTE_CATEGORY.into(),
        _ if CATEGORIES.contains(&f.as_str()) => f,
        _ => "inne".into(),
    }
}

/// Cheap category guess for facts saved without one.
pub fn guess_category(text: &str) -> &'static str {
    let f = fold(text);
    let has = |ws: &[&str]| ws.iter().any(|w| f.contains(w));
    if has(&["gram", "serwer", "gra ", "gry", "steam", "fivem", "minecraft", "discord"]) {
        "gry"
    } else if has(&["playlist", "muzyk", "spotify", "piosenk", "zespol", "album"]) {
        "muzyka"
    } else if has(&["pracuj", "praca", "pracy", "firma", "biuro", "projekt", "szef"]) {
        "praca"
    } else if has(&["lubie", "wole", "ulubion", "nie lubie", "prefer"]) {
        "preferencje"
    } else if has(&["imie", "nazywam", "mieszkam", "urodzin", "rodzin", "mama", "tata", "dziewczyn", "chlopak", "zona", "maz"]) {
        "osobiste"
    } else {
        "inne"
    }
}

/// Durable personal facts stated in passing ("gram na serwerze X", "moja playlista to Y").
/// Returns the fact to store (the user's own words, diacritics kept) and its category.
pub fn extract_fact(text: &str) -> Option<(String, &'static str)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 || words.len() > 25 {
        return None;
    }
    let folded: Vec<String> = words.iter().map(|w| fold(w)).collect();
    let tail = |from: usize| -> String {
        words[from.min(words.len())..]
            .join(" ")
            .trim_end_matches(['.', '!', '?', ','])
            .trim()
            .to_string()
    };
    // Find a pattern start anywhere in the first few words (after "Soul," / "wiesz," …).
    for i in 0..folded.len().min(6) {
        let w = |k: usize| folded.get(i + k).map(String::as_str).unwrap_or("");
        let fact = match (w(0), w(1), w(2)) {
            ("gram", "na", "serwerze") | ("gram", "na", "serwerach") => Some((tail(i), "gry")),
            ("gram", "w", _) if !w(2).is_empty() => Some((tail(i), "gry")),
            ("mam", "na", "imie") => Some((tail(i), "osobiste")),
            ("nazywam", "sie", _) if !w(2).is_empty() => Some((tail(i), "osobiste")),
            ("mieszkam", "w", _) if !w(2).is_empty() => Some((tail(i), "osobiste")),
            ("pracuje", "w", _) | ("pracuje", "jako", _) if !w(2).is_empty() => {
                Some((tail(i), "praca"))
            }
            ("my", "name", "is") => Some((tail(i), "osobiste")),
            ("i", "play", "on") => Some((tail(i), "gry")),
            (m, _, _) if matches!(m, "moja" | "moj" | "moje" | "my") => {
                // "moja (ulubiona) playlista to …", "mój serwer to …", "my playlist is …"
                let rest = &folded[i..];
                rest.iter()
                    .position(|x| x == "to" || x == "is" || x == "nazywa")
                    .filter(|&p| p >= 2 && p <= 4 && p + 1 < rest.len())
                    .map(|_| {
                        let fact = tail(i);
                        let cat = guess_category(&fact);
                        (fact, if cat == "inne" { "preferencje" } else { cat })
                    })
            }
            _ => None,
        };
        if let Some((fact, cat)) = fact {
            if fact.split_whitespace().count() >= 3 && !is_secret(&fact) {
                let mut c = fact.chars();
                let fact = match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => fact,
                };
                return Some((fact, cat));
            }
        }
    }
    None
}

// ───────────────────────── app launches (scene suggestions) ─────────────────────────

pub fn record_launch(app_name: &str) {
    let name = app_name.trim().to_string();
    if name.is_empty() {
        return;
    }
    let now = now_ms();
    let _ = with_db(|c| {
        c.execute("INSERT INTO app_launches (app, at) VALUES (?1, ?2)", params![name, now])?;
        // Keep 60 days of history.
        c.execute("DELETE FROM app_launches WHERE at < ?1", [now - 60 * 86_400_000])?;
        Ok(())
    });
}

pub fn launches_since(since_ms: i64) -> Vec<(String, i64)> {
    with_db(|c| {
        let mut st = c.prepare("SELECT app, at FROM app_launches WHERE at >= ?1 ORDER BY at")?;
        let rows = st.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    })
    .unwrap_or_default()
}

pub fn is_dismissed(key: &str) -> bool {
    with_db(|c| {
        c.query_row("SELECT 1 FROM dismissed WHERE key=?1", [key], |_| Ok(()))
            .optional()
            .map(|r| r.is_some())
    })
    .unwrap_or(true)
}

pub fn dismiss(key: &str) {
    let _ = with_db(|c| c.execute("INSERT OR IGNORE INTO dismissed (key) VALUES (?1)", [key]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: i64, text: &str, age_days: i64, now: i64) -> Fact {
        let t = now - age_days * 86_400_000;
        Fact {
            id,
            text: text.into(),
            category: guess_category(text).into(),
            source: "voice".into(),
            created_at: t,
            updated_at: t,
            last_used_at: 0,
            use_count: 0,
        }
    }

    #[test]
    fn extracts_durable_polish_facts() {
        assert_eq!(
            extract_fact("gram na serwerze CityRP od wczoraj"),
            Some(("Gram na serwerze CityRP od wczoraj".into(), "gry"))
        );
        let (f, c) = extract_fact("Soul, moja playlista to Chill Vibes").unwrap();
        assert_eq!(f, "Moja playlista to Chill Vibes");
        assert_eq!(c, "muzyka");
        assert!(extract_fact("otwórz Spotify").is_none());
        assert!(extract_fact("moje hasło to kot123").is_none());
    }

    #[test]
    fn ranking_prefers_keywords_then_recency() {
        let now = 1_000_000_000_000;
        let facts = vec![
            fact(1, "Moja playlista to Chill Vibes", 40, now),
            fact(2, "Gram na serwerze CityRP", 1, now),
            fact(3, "Mieszkam w Krakowie", 0, now),
        ];
        let ranked = rank(&facts, Some("jaka jest moja playlista"), now);
        assert_eq!(ranked[0].1.id, 1);
        let ranked = rank(&facts, None, now);
        assert_eq!(ranked[0].1.id, 3);
        // Inflection: "serwerze" ~ "serwer".
        assert_eq!(keyword_hits(&tokens("na jakim serwerze gram"), "Gram na serwerze CityRP"), 2);
    }

    #[test]
    fn prompt_block_is_bounded_and_labelled() {
        let now = 1_000_000_000_000;
        let facts: Vec<Fact> = (0..30).map(|i| fact(i, &format!("Fakt numer {i}"), i, now)).collect();
        let top: Vec<Fact> = rank(&facts, None, now).into_iter().take(PROMPT_FACTS).map(|(_, f)| f).collect();
        let block = format_prompt(&top);
        assert_eq!(block.matches("\n- ").count(), PROMPT_FACTS);
        assert!(block.contains("Known facts about the user"));
    }

    /// Schema + every statement against a real (in-memory) SQLite: facts, dedupe, search,
    /// forget, scenes upsert, launches. Only this test touches the global connection.
    #[test]
    fn sqlite_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert!(DB.set(Mutex::new(conn)).is_ok());

        let (a, new) = add_fact("Gram na serwerze CityRP", "gry", "voice").unwrap();
        assert!(new);
        let (b, new) = add_fact("gram na serwerze cityrp.", "inne", "auto").unwrap();
        assert!(!new);
        assert_eq!(a.id, b.id);
        add_fact("Moja playlista to Chill Vibes", "muzyka", "voice").unwrap();
        add_fact("Kupić mleko", NOTE_CATEGORY, "voice").unwrap();
        assert!(add_fact("Moje hasło to kot123", "inne", "voice").is_err());

        let hits = search("na jakim serwerze gram", 5).unwrap();
        assert_eq!(hits[0].text, "Gram na serwerze CityRP");
        assert_eq!(list(Some(NOTE_CATEGORY)).unwrap().len(), 1);
        assert!(prompt_facts(None).contains("Chill Vibes"));
        assert!(prompt_facts(Some("playlista")).contains("Chill Vibes"));

        let edited = update_fact(a.id, "Gram na serwerze FiveM CityRP", "gry").unwrap();
        assert_eq!(edited.text, "Gram na serwerze FiveM CityRP");
        assert_eq!(forget("playlista").unwrap().len(), 1);
        assert_eq!(count().unwrap(), 2);

        let scene = crate::scenes::save(crate::scenes::Scene {
            id: 0,
            name: "granie".into(),
            triggers: vec!["gramy w FiveM".into()],
            steps: vec![crate::scenes::SceneStep { kind: "open_app".into(), value: "Discord".into() }],
            updated_at: 0,
        })
        .unwrap();
        assert!(scene.id > 0);
        // Same name again = update, not a duplicate.
        crate::scenes::save(crate::scenes::Scene { id: 0, ..scene.clone() }).unwrap();
        assert_eq!(crate::scenes::list().unwrap().len(), 1);
        assert_eq!(crate::scenes::match_trigger("Gramy w FiveM!").map(|s| s.name), Some("granie".into()));
        assert_eq!(crate::scenes::find("scenę granie").map(|s| s.id), Some(scene.id));
        crate::scenes::delete(scene.id).unwrap();
        assert!(crate::scenes::list().unwrap().is_empty());

        record_launch("Discord");
        assert_eq!(launches_since(0).len(), 1);
        assert!(!is_dismissed("k"));
        dismiss("k");
        assert!(is_dismissed("k"));

        clear_all().unwrap();
        assert_eq!(count().unwrap(), 0);
    }

    #[test]
    fn live_prompt_order_ignores_usage() {
        let now = 1_000_000_000_000;
        let mut a = fact(1, "Mieszkam w Krakowie", 2, now);
        let b = fact(2, "Gram na serwerze CityRP", 1, now);
        let before = format_prompt(&stable_top(vec![a.clone(), b.clone()], 15));
        a.use_count = 9;
        a.last_used_at = now;
        let after = format_prompt(&stable_top(vec![b, a], 15));
        assert_eq!(before, after);
    }

    #[test]
    fn secrets_are_refused() {
        assert!(is_secret("Moje hasło do banku to Kot123!"));
        assert!(!is_secret("Gram na serwerze CityRP"));
    }

    #[test]
    fn categories_normalize() {
        assert_eq!(normalize_category("Music"), "muzyka");
        assert_eq!(normalize_category("gry"), "gry");
        assert_eq!(normalize_category("cokolwiek"), "inne");
    }
}
