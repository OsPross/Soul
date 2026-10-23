//! Browser voice commands handled without a model (Vio API v2): reading the article aloud, find
//! on page, scroll to a text, the video / audio of a Vio tab, tab operations, history / bookmark
//! search, price watches and tab scenes — plus Soul's reactions to Vio's own buttons (Soul icon,
//! panel items, STOP).
//!
//! [`parse`] is pure and tolerant to STT slips (key words may be a letter or two off). [`route`]
//! adds the live conditions — Vio connected, the command announced in `hello`, media actually in
//! Vio — and returns `None` to leave the phrase to the pre-router / model as before („pauza” while
//! Spotify plays still pauses Spotify). It runs before `is_computer_task` and the pre-router.

use super::router::{fold_word, strip_filler, PreRoute};
use super::computer::vio::{self, Client};
use super::computer::vio_ctx::{BrowserCtx, Signal};
use crate::platform::ui::clip;
use crate::scenes;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// One browser voice command.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    ReadArticle,
    Find(String),
    /// `page` = the phrase names the page („przewiń stronę do X”): Vio need not be in front.
    ScrollTo { query: String, page: bool },
    /// `play` | `pause` | `seekBy` (value = seconds) | `faster` | `slower` | `louder` | `quieter`.
    Media { action: &'static str, value: Option<f64> },
    /// `mute` | `unmute` | `pin` | `unpin` | `reopen` | `back` | `forward`; `bare` = a one-word
    /// phrase („wstecz”, „dalej”) that only counts while Vio is in front.
    Tab { op: &'static str, bare: bool },
    History(String),
    Bookmark(String),
    Stop,
    WatchPrice,
    ListWatches,
    /// Empty = the watch of the current page.
    Unwatch(String),
    SaveScene(String),
    OpenScene(String),
    DeleteScene(String),
}

/// Courtesy words dropped before matching (they never carry the command).
const POLITE: &[&str] = &[
    "prosze", "mi", "sobie", "ta", "te", "tej", "tego", "ten", "tym", "teraz", "aktualna", "aktualnej", "obecna",
    "obecnej", "biezaca", "biezacej", "please", "the", "this", "current", "soul", "ej", "hej", "a", "i", "no", "moze",
];
const VIDEO: &[&str] = &["film", "filmik", "wideo", "video", "odtwarzanie", "nagranie", "muzyke", "to", "troche"];
const TAB: &[&str] = &["karte", "karta", "karcie", "karty", "zakladke", "tab"];
const PAGE: &[&str] = &["strone", "strona", "stronie", "strony", "page"];
const ARTICLE: &[&str] = &["artykul", "artykulu", "article"];
/// „przewiń do …” words that are a direction, not a text on the page.
const DIRECTIONS: &[&str] = &[
    "gory", "gore", "dolu", "dol", "konca", "poczatku", "przodu", "tylu", "top", "bottom", "end", "start", "nastepnej",
    "poprzedniej",
];

struct Word {
    /// As said (diacritics kept, punctuation trimmed): queries use these.
    orig: String,
    folded: String,
}

/// The words of the utterance without leading fillers („weź mi”, „czy możesz”) and courtesy words.
fn words(text: &str) -> Vec<Word> {
    let orig: Vec<String> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    let folded: Vec<String> = orig.iter().map(|w| fold_word(w)).collect();
    let (skip, _) = strip_filler(&folded);
    orig.into_iter()
        .zip(folded)
        .skip(skip)
        .filter(|(_, f)| !POLITE.contains(&f.as_str()))
        .map(|(orig, folded)| Word { orig, folded })
        .collect()
}

fn lev(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `w` is `t`, or an STT slip of it: one letter off for 5–8 letter words, two for longer ones
/// („przypomnij” is not „przypnij”).
fn near(w: &str, t: &str) -> bool {
    let n = t.chars().count();
    w == t || (n >= 5 && lev(w, t) <= if n >= 9 { 2 } else { 1 })
}

fn near_any(w: &str, set: &[&str]) -> bool {
    set.iter().any(|t| near(w, t))
}

/// Polish / English number word or digits.
fn number(w: &str) -> Option<f64> {
    w.parse::<f64>().ok().or_else(|| super::router::number_word(w).map(|n| n as f64))
}

/// „30 sekund”, „trzydzieści sekund”, „minutę”, „pół minuty”, „10 s” → seconds.
fn seconds(f: &[&str]) -> Option<f64> {
    let unit = |w: &str| {
        if w.starts_with("sek") || w.starts_with("second") || w == "s" {
            Some(1.0)
        } else if w.starts_with("minut") || w == "min" {
            Some(60.0)
        } else {
            None
        }
    };
    for (i, w) in f.iter().enumerate() {
        if *w == "pol" && f.get(i + 1).is_some_and(|u| u.starts_with("minut")) {
            return Some(30.0);
        }
        if let Some(n) = number(w) {
            // „dwadzieścia pięć sekund”: a second number word adds up.
            let (n, next) = match f.get(i + 1).and_then(|x| number(x)) {
                Some(m) if n >= 20.0 && m < 10.0 => (n + m, i + 2),
                _ => (n, i + 1),
            };
            if let Some(u) = f.get(next).and_then(|x| unit(x)) {
                return Some(n * u);
            }
        }
        if (w.starts_with("minute") || *w == "minuta") && !f[..i].iter().any(|x| number(x).is_some()) {
            return Some(60.0);
        }
    }
    None
}

/// Original words from position `i` on, joined (the query keeps the user's spelling).
fn tail(ws: &[Word], i: usize) -> String {
    ws.get(i..).map(|r| r.iter().map(|w| w.orig.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// Recognize a browser command (no I/O). `None` = not one.
pub fn parse(text: &str) -> Option<Cmd> {
    let ws = words(text);
    let f: Vec<&str> = ws.iter().map(|w| w.folded.as_str()).collect();
    if f.is_empty() || f.len() > 14 {
        return None;
    }
    let first = f[0];
    let rest = &f[1..];
    let has = |set: &[&str]| f.iter().any(|w| near_any(w, set));
    let only = |set: &[&str]| rest.iter().all(|w| set.contains(w));

    // Kill switch.
    if f.len() <= 3 && near_any(first, &["stop", "przestan", "starczy", "wystarczy", "dosc"]) && only(&["juz", "mowic", "gadac", "sie", "to"]) {
        return Some(Cmd::Stop);
    }
    if f == ["zatrzymaj", "sie"] {
        return Some(Cmd::Stop);
    }

    // Price watches (before „przestań …” and „obserwuj …” go anywhere else).
    let price_noun = |w: &&str| matches!(*w, "cene" | "ceny" | "cena" | "cen" | "price" | "prices");
    let unwatch = (first == "przestan" && rest.first().is_some_and(|w| near(w, "obserwowac")))
        || (near_any(first, &["usun", "skasuj"]) && rest.first().is_some_and(|w| near_any(w, &["obserwacje", "obserwowanie"])))
        || (first == "nie" && rest.first() == Some(&"obserwuj"))
        || (first == "stop" && rest.first() == Some(&"watching"));
    if unwatch {
        let after = f.iter().position(price_noun).map(|i| i + 1).unwrap_or(2);
        let q: Vec<&Word> = ws.iter().skip(after).filter(|w| !PAGE.contains(&w.folded.as_str()) && w.folded != "produktu").collect();
        return Some(Cmd::Unwatch(q.iter().map(|w| w.orig.as_str()).collect::<Vec<_>>().join(" ")));
    }
    if near_any(first, &["obserwuj", "sledz", "monitoruj", "pilnuj", "watch", "track"]) && f.iter().any(price_noun) {
        return Some(Cmd::WatchPrice);
    }
    if (near_any(first, &["pokaz", "wymien", "lista", "jakie", "list", "show"]) || first == "co")
        && has(&["obserwowane", "obserwowanych", "obserwuje", "watched", "watches"])
    {
        return Some(Cmd::ListWatches);
    }

    // Scenes of tabs: „zapisz scenę praca”, „zapisz karty jako scenę praca”, „otwórz / usuń scenę praca”.
    let scene_at = f.iter().position(|w| w.starts_with("scen") || *w == "scene");
    if let Some(si) = scene_at {
        let name = tail(&ws, si + 1);
        let verb = |set: &[&str]| near_any(first, set);
        let save = verb(&["zapisz", "zachowaj", "save"]) && (si == 1 || (f[1..si].iter().any(|w| near_any(w, TAB) || *w == "tabs") && f[si - 1] == "jako"));
        if !name.is_empty() && save {
            return Some(Cmd::SaveScene(name));
        }
        if !name.is_empty() && si == 1 && verb(&["otworz", "open"]) {
            return Some(Cmd::OpenScene(name));
        }
        if !name.is_empty() && si == 1 && verb(&["usun", "skasuj", "delete", "remove"]) {
            return Some(Cmd::DeleteScene(name));
        }
    }

    // Read the article aloud („streść tę stronę” stays with the agent: a model summary on request).
    if near_any(first, &["przeczytaj", "czytaj", "read"])
        && (has(ARTICLE) || (f.contains(&"glos") && has(PAGE)) || (first == "read" && f.contains(&"aloud")))
        && f.len() <= 6
    {
        return Some(Cmd::ReadArticle);
    }

    // Media: seek, pause / play, speed, volume.
    if let Some(s) = seconds(&f) {
        let seek_verb = near_any(first, &["przewin", "przeskocz", "skip", "cofnij", "przesun", "skocz"]) || f.windows(2).any(|p| p == ["do", "przodu"] || p == ["do", "tylu"]);
        if seek_verb && f.len() <= 8 {
            let back = has(&["tylu", "cofnij", "back", "backward", "wstecz"]);
            return Some(Cmd::Media { action: "seekBy", value: Some(if back { -s } else { s }) });
        }
    }
    let media = |action: &'static str| Some(Cmd::Media { action, value: None });
    if f.len() <= 3 && near_any(first, &["pauza", "zapauzuj", "spauzuj", "wstrzymaj", "pause"]) && only(VIDEO) {
        return media("pause");
    }
    if f.len() == 2 && first == "zatrzymaj" && VIDEO.contains(&f[1]) {
        return media("pause");
    }
    let play_verb = near_any(first, &["graj", "wznow", "odtworz", "odtwarzaj", "play", "resume"]);
    if (f.len() == 1 && play_verb) || (f.len() == 2 && (play_verb || matches!(first, "pusc" | "wlacz")) && (f[1] == "dalej" || VIDEO.contains(&f[1]))) {
        return media("play");
    }
    if f.len() <= 2 && only(VIDEO) {
        match first {
            "szybciej" | "faster" => return media("faster"),
            "wolniej" | "slower" => return media("slower"),
            "glosniej" | "louder" => return media("louder"),
            "ciszej" | "quieter" => return media("quieter"),
            w if near(w, "przyspiesz") => return media("faster"),
            w if near(w, "zwolnij") => return media("slower"),
            _ => {}
        }
    }

    // Tab operations.
    let tab = |op: &'static str| Some(Cmd::Tab { op, bare: false });
    let names_tab = has(TAB) || f.contains(&"tab");
    if names_tab && f.len() <= 5 {
        if near_any(first, &["wycisz", "mute"]) {
            return tab("mute");
        }
        if near_any(first, &["odcisz", "unmute"]) || (matches!(first, "wlacz" | "przywroc") && f.contains(&"dzwiek")) {
            return tab("unmute");
        }
        if near_any(first, &["przypnij", "pin"]) {
            return tab("pin");
        }
        if near_any(first, &["odepnij", "unpin"]) {
            return tab("unpin");
        }
        if near_any(first, &["przywroc", "otworz", "reopen"]) && has(&["zamknieta", "zamknietej", "ostatnia", "ostatnio", "ponownie", "closed", "powrotem"]) {
            return tab("reopen");
        }
    }
    let bare = |op: &'static str| Some(Cmd::Tab { op, bare: true });
    match f.as_slice() {
        ["wstecz"] => return bare("back"),
        ["dalej"] | ["naprzod"] | ["do", "przodu"] => return bare("forward"),
        ["go", "back"] => return tab("back"),
        ["go", "forward"] => return tab("forward"),
        _ => {}
    }
    if f.len() <= 5 && has(PAGE) {
        if near_any(first, &["cofnij", "wroc", "idz", "przejdz"]) && (has(&["poprzedniej", "poprzednia", "wstecz"]) || f.len() == 2) && !has(&["nastepnej", "nastepna"]) {
            return tab("back");
        }
        if near_any(first, &["idz", "przejdz"]) && has(&["przodu", "naprzod"]) {
            return tab("forward");
        }
    }

    // Find on the page / in the history, open a bookmark.
    let find_verb = near_any(first, &["znajdz", "szukaj", "wyszukaj", "poszukaj", "find", "search"]);
    if find_verb && f.len() >= 3 {
        let is = |w: &str, set: &[&str]| near_any(w, set);
        // „znajdź na stronie X”, „znajdź w historii X”
        if f.len() >= 4 && matches!(f[1], "na" | "w" | "on" | "in") {
            if is(f[2], &["stronie", "page", "tekscie"]) {
                return Some(Cmd::Find(tail(&ws, 3)));
            }
            if is(f[2], &["historii", "history"]) {
                return Some(Cmd::History(tail(&ws, 3)));
            }
        }
        // „znajdź X na stronie”, „znajdź X w historii”
        let n = f.len();
        if n >= 4 && matches!(f[n - 2], "na" | "w" | "on" | "in") {
            let q = ws[1..n - 2].iter().map(|w| w.orig.as_str()).collect::<Vec<_>>().join(" ");
            if is(f[n - 1], &["stronie", "page"]) {
                return Some(Cmd::Find(q));
            }
            if is(f[n - 1], &["historii", "history"]) {
                return Some(Cmd::History(q));
            }
        }
        // "search history for X"
        if f[1] == "history" {
            let from = if f.get(2) == Some(&"for") { 3 } else { 2 };
            return Some(Cmd::History(tail(&ws, from))).filter(|c| !matches!(c, Cmd::History(q) if q.is_empty()));
        }
    }
    if near_any(first, &["otworz", "pokaz", "open"]) && f.len() >= 3 && near_any(f[1], &["zakladke", "zakladki", "bookmark"]) && !matches!(f[2], "nowa" | "nowej" | "new") {
        return Some(Cmd::Bookmark(tail(&ws, 2)));
    }

    // „przewiń do sekcji Historia”, „przewiń stronę do X”, "scroll to comments".
    let page = f.len() >= 4 && near_any(f[1], PAGE);
    let at = if page { 2 } else { 1 };
    if near_any(first, &["przewin", "scroll", "skocz", "przejdz"]) && f.len() > at + 1 && matches!(f[at], "do" | "to") && !DIRECTIONS.contains(&f[at + 1]) && seconds(&f).is_none() {
        let from = if matches!(f[at + 1], "sekcji" | "czesci" | "fragmentu" | "akapitu" | "section") && f.len() > at + 2 { at + 2 } else { at + 1 };
        return Some(Cmd::ScrollTo { query: tail(&ws, from), page });
    }
    None
}

/// The Vio command a tab operation needs.
fn tab_cmd(op: &str) -> &'static str {
    match op {
        "mute" | "unmute" => "muteTab",
        "pin" | "unpin" => "pinTab",
        "back" => "goBack",
        "forward" => "goForward",
        _ => "reopenClosedTab",
    }
}

/// Playback rate / element volume steps Soul applies (Vio reports neither).
const RATE_STEP: f64 = 0.25;
const VOLUME_STEP: f64 = 0.2;

/// Whether `cmd` applies now: Vio connected, the command announced (`has`), media / focus
/// conditions.
pub fn applies(cmd: &Cmd, connected: bool, has: &dyn Fn(&str) -> bool, ctx: &BrowserCtx, other_media_playing: bool) -> bool {
    let focused = ctx.focused == Some(true);
    let active_media = ctx.media.as_ref().filter(|m| ctx.tab.as_ref() == Some(&m.tab));
    match cmd {
        Cmd::Stop => false,
        Cmd::OpenScene(_) | Cmd::DeleteScene(_) | Cmd::ListWatches | Cmd::Unwatch(_) => true,
        Cmd::SaveScene(_) | Cmd::WatchPrice => connected,
        Cmd::ReadArticle => has("getReadable"),
        Cmd::Find(q) => has("findInPage") && !q.is_empty(),
        Cmd::ScrollTo { query, page } => has("scrollTo") && (focused || *page) && !query.is_empty(),
        Cmd::History(q) => has("searchHistory") && !q.is_empty(),
        Cmd::Bookmark(q) => has("searchBookmarks") && !q.is_empty(),
        // „dalej” with music playing is still „next track”.
        Cmd::Tab { op, bare } => has(tab_cmd(op)) && (!bare || (focused && !(*op == "forward" && other_media_playing))),
        Cmd::Media { action, .. } => {
            // Vio reports media only once it played or paused. Seek / speed with Vio in front and
            // nothing known yet ask the page; play / pause keep the media keys (they reach Vio's
            // video too).
            let unknown = focused && ctx.media.is_none();
            has("mediaControl")
                && match *action {
                    "pause" => ctx.media.as_ref().is_some_and(|m| m.playing),
                    "play" => focused && active_media.is_some_and(|m| !m.playing),
                    "seekBy" | "faster" | "slower" => unknown || ctx.media_target().is_some(),
                    "louder" => {
                        focused && active_media.is_some_and(|m| m.playing && *ctx_volume(ctx, &m.tab) < 1.0 - 1e-6)
                    }
                    "quieter" => focused && active_media.is_some_and(|m| m.playing),
                    _ => ctx.media_target().is_some(),
                }
        }
    }
}

fn ctx_volume<'a>(ctx: &'a BrowserCtx, tab: &str) -> &'a f64 {
    ctx.volume.get(tab).unwrap_or(&1.0)
}

/// A browser command for this utterance that can run now (`None` = not one, or Vio can't).
pub fn route(text: &str) -> Option<PreRoute> {
    let cmd = parse(text)?;
    let c = vio::connected_client();
    let ctx = c.as_ref().map(|c| c.context()).unwrap_or_default();
    let other_media = crate::island::now_playing_brief().is_some_and(|(_, _, playing)| playing);
    let has = |name: &str| c.as_ref().is_some_and(|c| c.has(name));
    if !applies(&cmd, c.is_some(), &has, &ctx, other_media) {
        return None;
    }
    let (op, args) = match cmd {
        Cmd::OpenScene(name) => {
            return Some(PreRoute { tool: "run_scene".into(), args: json!({ "name": name }), reason: "browser:scene".into() })
        }
        Cmd::ReadArticle => ("read_article", json!({})),
        Cmd::Find(q) => ("find", json!({ "query": q })),
        Cmd::ScrollTo { query, .. } => ("scroll_to", json!({ "query": query })),
        Cmd::Media { action, value } => ("media", json!({ "action": action, "value": value })),
        Cmd::Tab { op, .. } => ("tab", json!({ "action": op })),
        Cmd::History(q) => ("history", json!({ "query": q })),
        Cmd::Bookmark(q) => ("bookmark", json!({ "query": q })),
        Cmd::WatchPrice => ("watch_price", json!({})),
        Cmd::ListWatches => ("list_watches", json!({})),
        Cmd::Unwatch(q) => ("unwatch", json!({ "query": q })),
        Cmd::SaveScene(name) => ("save_scene", json!({ "name": name })),
        Cmd::DeleteScene(name) => ("delete_scene", json!({ "name": name })),
        Cmd::Stop => return None,
    };
    let mut args = args;
    args["op"] = json!(op);
    Some(PreRoute { tool: TOOL.into(), args, reason: format!("browser:{op}") })
}

/// The tool name in routes, chips and the e2e trace.
pub const TOOL: &str = "browser";

/// No Live voice confirmation for these (reading aloud speaks by itself).
pub fn is_quiet(route: &PreRoute) -> bool {
    route.tool == TOOL && route.args["op"] == "read_article"
}

/// „stop” / „przestań” (the kill switch phrase).
pub fn is_stop(text: &str) -> bool {
    parse(text) == Some(Cmd::Stop)
}

fn client() -> Result<Arc<Client>, String> {
    vio::connected_client().ok_or_else(|| "Vio nie jest połączone.".to_string())
}

/// Active tab: from events, else `listTabs`. (tab id, title, url)
fn active(c: &Client) -> Result<(String, String, String), String> {
    let ctx = c.context();
    if let Some(t) = ctx.tab.filter(|_| ctx.live) {
        return Ok((t, ctx.title, ctx.url));
    }
    let (tab, id) = vio::active_tab(&c.call("listTabs", json!({}))?).ok_or("Vio nie ma otwartej karty.")?;
    let s = |k: &str| tab[k].as_str().unwrap_or_default().to_string();
    Ok((id, s("title"), s("url")))
}

fn polish(e: String) -> String {
    if vio::is_forbidden(&e) {
        vio::forbidden_message(crate::i18n::Lang::Pl, &e)
    } else if e == crate::platform::input::ABORTED {
        STOPPED.into()
    } else if e.contains("cannot go back") {
        "Nie ma dokąd wrócić.".into()
    } else if e.contains("cannot go forward") {
        "Nie ma dokąd iść dalej.".into()
    } else if e.contains("nothing to reopen") {
        "Nie ma zamkniętej karty do przywrócenia.".into()
    } else if e.contains("No video or audio") {
        "Nie widzę na tej stronie odtwarzacza, którym mogę sterować (filmów osadzonych z innej strony, np. YouTube w ramce, nie da się sterować).".into()
    } else {
        e
    }
}

fn arg(args: &Value, k: &str) -> String {
    args[k].as_str().unwrap_or_default().trim().to_string()
}

/// What a command stopped by STOP (or the kill switch) reports: not an error.
pub(super) const STOPPED: &str = "Zatrzymane.";

/// Run a `browser` route (worker thread).
pub fn run(app: &AppHandle, platform: &Arc<dyn crate::platform::Platform>, args: &Value) -> Result<String, String> {
    match run_op(app, platform, args) {
        Err(e) if e == STOPPED => Ok(e),
        r => r,
    }
}

fn run_op(app: &AppHandle, platform: &Arc<dyn crate::platform::Platform>, args: &Value) -> Result<String, String> {
    let op = arg(args, "op");
    match op.as_str() {
        "list_watches" => return list_watches(),
        "unwatch" => return unwatch(&arg(args, "query")),
        "delete_scene" => return delete_scene(app, &arg(args, "name")),
        _ => {}
    }
    let c = client()?;
    let (tab, title, url) = active(&c).map_err(polish)?;
    let q = arg(args, "query");
    match op.as_str() {
        "read_article" => {
            let text = vio::read_page(&c, &tab).map_err(polish)?;
            let head = text.lines().next().unwrap_or_default().to_string();
            read_aloud_later(app, text);
            Ok(format!("Czytam: {}", clip(&head, 80)))
        }
        "find" => {
            let v = c.call("findInPage", json!({ "tabId": tab, "text": clip(&q, 500) })).map_err(polish)?;
            Ok(match v["matches"].as_u64().unwrap_or(0) {
                0 => format!("Nie znalazłem \u{201e}{q}\u{201d} na stronie."),
                1 => format!("Jest jedno \u{201e}{q}\u{201d} — zaznaczyłem je."),
                n => format!("Znalazłem \u{201e}{q}\u{201d} {n} razy — pokazuję pierwsze."),
            })
        }
        "scroll_to" => match c.call("scrollTo", json!({ "tabId": tab, "text": q })) {
            Ok(_) => Ok(format!("Przewinąłem do \u{201e}{q}\u{201d}.")),
            Err(e) if e.contains("not found") => Err(format!("Nie znalazłem \u{201e}{q}\u{201d} na stronie.")),
            Err(e) => Err(polish(e)),
        },
        "media" => {
            let action = arg(args, "action");
            match media(&c, &action, args["value"].as_f64()) {
                // No player on the page: „graj” / „pauza” meant the music elsewhere.
                Err(e) if e.contains("odtwarzacza") && matches!(action.as_str(), "play" | "pause") => platform.media_control(&action),
                r => r,
            }
        }
        "tab" => {
            let action = arg(args, "action");
            vio::tab_op(&c, &tab, &action).map_err(polish)?;
            Ok(match action.as_str() {
                "mute" => "Karta wyciszona.",
                "unmute" => "Dźwięk karty włączony.",
                "pin" => "Karta przypięta.",
                "unpin" => "Karta odpięta.",
                "reopen" => "Przywróciłem zamkniętą kartę.",
                "back" => "Wracam.",
                _ => "Idę dalej.",
            }
            .into())
        }
        "history" | "bookmark" => {
            let bookmarks = op == "bookmark";
            let (list, _) = vio::search(&c, bookmarks, &q).map_err(polish)?;
            let Some(top) = list.first() else {
                return Ok(if bookmarks { format!("Nie mam zakładki \u{201e}{q}\u{201d}.") } else { format!("Nic w historii o \u{201e}{q}\u{201d}.") });
            };
            let top_url = top["url"].as_str().unwrap_or_default();
            vio::open_tab(&c, platform.as_ref(), top_url).map_err(polish)?;
            let name = top["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(top_url);
            let more = list.len() - 1;
            if more > 0 {
                let items: Vec<Value> = list
                    .iter()
                    .map(|e| json!({ "text": clip(e["title"].as_str().unwrap_or_default(), 120), "action": "open", "payload": e["url"] }))
                    .collect();
                show_panel(&c, if bookmarks { "Zakładki" } else { "Historia" }, items);
            }
            let where_ = if bookmarks { "z zakładek" } else { "z historii" };
            Ok(if more > 0 && c.has("panel") {
                format!("Otwieram \u{201e}{}\u{201d} {where_}; pozostałe wyniki ({more}) są w panelu Vio.", clip(name, 60))
            } else {
                format!("Otwieram \u{201e}{}\u{201d} {where_}.", clip(name, 60))
            })
        }
        "watch_price" => {
            let ctx = c.context();
            // The user's selected price is the most exact hint.
            let hint = Some(ctx.selection.clone()).filter(|s| !crate::price_watch::find_prices(s).is_empty());
            let w = crate::price_watch::add(&url, &title, hint.as_deref())?;
            let _ = app.emit("soul://price-watches", ());
            Ok(format!(
                "Obserwuję cenę: {}. Sprawdzam co kilka godzin i dam znać, gdy spadnie.{}",
                crate::price_watch::fmt_price(w.price, &w.currency),
                if hint.is_none() { " Jeśli to nie ta cena, zaznacz ją na stronie i powiedz to jeszcze raz." } else { "" }
            ))
        }
        "save_scene" => save_scene(app, &c, &arg(args, "name")),
        other => Err(format!("Nieznana komenda przeglądarki {other}.")),
    }
}

fn media(c: &Client, action: &str, value: Option<f64>) -> Result<String, String> {
    let ctx = c.context();
    let tab = ctx.media.as_ref().map(|m| m.tab.clone());
    let pct = |v: f64| format!("{:.0}%", v * 100.0);
    let (vio_action, v, done): (&str, Option<f64>, String) = match action {
        "faster" | "slower" => {
            let now = tab.as_ref().and_then(|t| ctx.rate.get(t)).copied().unwrap_or(1.0);
            let r = (now + if action == "faster" { RATE_STEP } else { -RATE_STEP }).clamp(0.25, 3.0);
            ("rate", Some(r), format!("Prędkość {}×.", format!("{r}").replace('.', ",")))
        }
        "louder" | "quieter" => {
            let now = tab.as_ref().map(|t| *ctx_volume(&ctx, t)).unwrap_or(1.0);
            let v = (now + if action == "louder" { VOLUME_STEP } else { -VOLUME_STEP }).clamp(0.0, 1.0);
            ("volume", Some(v), format!("Głośność filmu {}.", pct(v)))
        }
        "seekBy" => {
            let s = value.unwrap_or(10.0);
            let msg = if s < 0.0 { format!("Cofnąłem o {:.0} s.", -s) } else { format!("Przewinąłem o {s:.0} s.") };
            ("seekBy", Some(s), msg)
        }
        "pause" => ("pause", None, "Pauza.".into()),
        _ => ("play", None, "Gram.".into()),
    };
    vio::media(c, tab.as_deref(), vio_action, v).map_err(polish)?;
    if let (Some(t), Some(v)) = (tab, v) {
        c.update_context(|x| {
            match vio_action {
                "rate" => x.rate.insert(t, v),
                "volume" => x.volume.insert(t, v),
                _ => None,
            };
        });
    }
    Ok(done)
}

fn list_watches() -> Result<String, String> {
    let list = crate::price_watch::list()?;
    if let Some(c) = vio::connected_client().filter(|_| !list.is_empty()) {
        let items = list
            .iter()
            .map(|w| {
                let name = if w.title.is_empty() { clip(&w.url, 60) } else { clip(&w.title, 60) };
                json!({ "text": format!("{name} — {}", crate::price_watch::fmt_price(w.last_price, &w.currency)), "action": "unwatch", "payload": w.id })
            })
            .collect();
        show_panel(&c, "Obserwowane ceny (kliknij, aby usunąć)", items);
    }
    Ok(crate::price_watch::describe(&list))
}

fn unwatch(query: &str) -> Result<String, String> {
    let url = if query.is_empty() { vio::connected_client().and_then(|c| active(&c).ok()).map(|t| t.2) } else { None };
    if query.is_empty() && url.is_none() {
        return Err("Której ceny mam nie obserwować? Powiedz nazwę produktu.".into());
    }
    let gone = crate::price_watch::remove(query, url.as_deref())?;
    Ok(match gone.as_slice() {
        [] => "Nie obserwuję takiej ceny.".into(),
        [w] => format!("Już nie obserwuję: {}.", if w.title.is_empty() { clip(&w.url, 60) } else { clip(&w.title, 60) }),
        many => format!("Usunąłem {} obserwacji.", many.len()),
    })
}

/// Most tabs a scene stores.
const SCENE_TABS: usize = 12;

fn save_scene(app: &AppHandle, c: &Client, name: &str) -> Result<String, String> {
    let tabs = c.call("listTabs", json!({})).map_err(polish)?;
    let urls: Vec<String> = tabs
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["url"].as_str())
        .filter(|u| vio::normalize_url(u).is_ok())
        .take(SCENE_TABS)
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        return Err("Nie ma kart do zapisania.".into());
    }
    let steps = urls.iter().map(|u| scenes::SceneStep { kind: "open_url".into(), value: u.clone() }).collect();
    let scene = scenes::save(scenes::Scene { id: 0, name: name.to_string(), triggers: Vec::new(), steps, updated_at: 0 })?;
    let _ = app.emit("soul://scenes-changed", ());
    Ok(format!("Zapisałem scenę \u{201e}{}\u{201d} ({} kart). Powiedz \u{201e}otwórz scenę {}\u{201d}.", scene.name, urls.len(), scene.name))
}

fn delete_scene(app: &AppHandle, name: &str) -> Result<String, String> {
    let scene = scenes::find(name).ok_or_else(|| format!("Nie mam sceny \u{201e}{name}\u{201d}."))?;
    scenes::delete(scene.id)?;
    let _ = app.emit("soul://scenes-changed", ());
    Ok(format!("Usunąłem scenę \u{201e}{}\u{201d}.", scene.name))
}

fn show_panel(c: &Client, title: &str, items: Vec<Value>) {
    if !c.has("panel") {
        return;
    }
    let items: Vec<Value> = items.into_iter().take(30).collect();
    if let Err(e) = c.call("panel", json!({ "open": true, "content": { "title": title, "items": items } })) {
        eprintln!("[soul][agent] vio panel failed: {e}");
    }
}

/// Characters read aloud from an article in one go.
// shortcut: only the first ~3000 characters are read; reading on („czytaj dalej”) is not built yet.
const READ_MAX: usize = 3000;

/// Read the text aloud once Soul is quiet again (after the command released `busy`).
fn read_aloud_later(app: &AppHandle, text: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(state) = app.try_state::<crate::AppState>() else { return };
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end && (*state.busy.lock() || state.agent.speaking_turn.lock().is_some() || state.voice.is_speaking()) {
            std::thread::sleep(Duration::from_millis(100));
        }
        super::read_aloud(&app, &clip(&text, READ_MAX));
    });
}

/// STOP: the running agent task, the voice, waiting Vio calls and a pending confirmation all end
/// now. Returns whether anything was running.
pub fn stop_all(app: &AppHandle, why: &str) -> bool {
    let task = super::computer::killswitch::abort(why);
    let state = app.state::<crate::AppState>();
    let speaking = state.voice.is_speaking() || state.agent.speaking_turn.lock().is_some();
    super::barge_in(app);
    if let Some(c) = vio::global() {
        c.cancel_all();
    }
    let confirm = super::confirm::soul_pending(app);
    if confirm {
        let _ = super::confirm::answer(app, false);
    }
    vio::soul_state("idle", "");
    eprintln!("[soul][agent] stop ({why}) task={task} speaking={speaking} confirm={confirm}");
    task || speaking || confirm
}

/// Vio's Soul icon, panel buttons and STOP.
pub fn on_signal(app: &AppHandle, s: Signal) {
    match s {
        Signal::Stop => {
            stop_all(app, "STOP w Vio");
            crate::schedule_hide(app, 0);
        }
        Signal::IconClicked => crate::wake::listen_hands_free(app, "vio-icon"),
        Signal::Panel { action, payload } => match action.as_str() {
            "open" => {
                if let (Some(url), Some(c)) = (payload.as_str(), vio::connected_client()) {
                    let platform = Arc::clone(&app.state::<crate::AppState>().platform);
                    if let Err(e) = vio::open_tab(&c, platform.as_ref(), url) {
                        eprintln!("[soul][agent] vio panel open failed: {e}");
                    }
                }
            }
            "unwatch" => {
                if let Some(id) = payload.as_i64() {
                    let _ = crate::price_watch::remove_id(id);
                    let _ = app.emit("soul://price-watches", ());
                    let _ = list_watches();
                }
            }
            "listen" => crate::wake::listen_hands_free(app, "vio-panel"),
            "stop" => {
                stop_all(app, "panel Vio");
            }
            other => eprintln!("[soul][agent] vio panel action {other:?} ignored"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::computer::vio_ctx::{Field, Media};

    fn p(t: &str) -> Option<Cmd> {
        parse(t)
    }

    #[test]
    fn phrases_and_stt_slips() {
        use Cmd::*;
        let cases: Vec<(&str, Cmd)> = vec![
            ("Przeczytaj artykuł", ReadArticle),
            ("przeczytaj mi ten artykuł", ReadArticle),
            ("Przeczytaj na głos tę stronę", ReadArticle),
            ("przeczytaj artykól", ReadArticle),
            ("Znajdź na stronie cena dostawy", Find("cena dostawy".into())),
            ("znajdz na tej stronie Mirosławice", Find("Mirosławice".into())),
            ("Znajdź latarnię na stronie", Find("latarnię".into())),
            ("Znajdż na stronie kontakt", Find("kontakt".into())),
            ("szukaj na stronię kontakt", Find("kontakt".into())),
            ("Przewiń do sekcji Historia", ScrollTo { query: "Historia".into(), page: false }),
            ("przewiń do komentarzy", ScrollTo { query: "komentarzy".into(), page: false }),
            ("Przewiń stronę do wypożyczalni", ScrollTo { query: "wypożyczalni".into(), page: true }),
            ("Pauza", Media { action: "pause", value: None }),
            ("pałza", Media { action: "pause", value: None }),
            ("Graj", Media { action: "play", value: None }),
            ("wznów", Media { action: "play", value: None }),
            ("Wznów film", Media { action: "play", value: None }),
            ("przewiń o 30 sekund", Media { action: "seekBy", value: Some(30.0) }),
            ("Przewiń o trzydzieści sekund", Media { action: "seekBy", value: Some(30.0) }),
            ("przewiń do tyłu o 10 sekund", Media { action: "seekBy", value: Some(-10.0) }),
            ("cofnij o dwadzieścia pięć sekund", Media { action: "seekBy", value: Some(-25.0) }),
            ("przewiń o minutę", Media { action: "seekBy", value: Some(60.0) }),
            ("przewiń o pół minuty", Media { action: "seekBy", value: Some(30.0) }),
            ("Szybciej", Media { action: "faster", value: None }),
            ("wolniej", Media { action: "slower", value: None }),
            ("przyśpiesz film", Media { action: "faster", value: None }),
            ("głośniej", Media { action: "louder", value: None }),
            ("Ciszej", Media { action: "quieter", value: None }),
            ("Wycisz kartę", Tab { op: "mute", bare: false }),
            ("wycisz tą kartę", Tab { op: "mute", bare: false }),
            ("wyciś kartę", Tab { op: "mute", bare: false }),
            ("Przypnij kartę", Tab { op: "pin", bare: false }),
            ("przypnie kartę", Tab { op: "pin", bare: false }),
            ("odepnij kartę", Tab { op: "unpin", bare: false }),
            ("Przywróć zamkniętą kartę", Tab { op: "reopen", bare: false }),
            ("otwórz ponownie zamkniętą kartę", Tab { op: "reopen", bare: false }),
            ("Wstecz", Tab { op: "back", bare: true }),
            ("wróć do poprzedniej strony", Tab { op: "back", bare: false }),
            ("cofnij stronę", Tab { op: "back", bare: false }),
            ("Dalej", Tab { op: "forward", bare: true }),
            ("Znajdź w historii przepis na bigos", History("przepis na bigos".into())),
            ("znajdz w histori bigos", History("bigos".into())),
            ("Otwórz zakładkę bank", Bookmark("bank".into())),
            ("otwórz zakładkę Allegro", Bookmark("Allegro".into())),
            ("Stop", Stop),
            ("przestań", Stop),
            ("Stop, Soul!", Stop),
            ("Obserwuj cenę tej strony", WatchPrice),
            ("obserwuj cene tego produktu", WatchPrice),
            ("Pokaż obserwowane ceny", ListWatches),
            ("jakie ceny obserwujesz", ListWatches),
            ("Przestań obserwować cenę butów", Unwatch("butów".into())),
            ("przestań obserwować cenę tej strony", Unwatch(String::new())),
            ("Zapisz scenę praca", SaveScene("praca".into())),
            ("zapisz karty jako scenę zakupy", SaveScene("zakupy".into())),
            ("Otwórz scenę praca", OpenScene("praca".into())),
            ("usuń scenę praca", DeleteScene("praca".into())),
        ];
        for (text, want) in cases {
            assert_eq!(p(text), Some(want), "{text}");
        }
    }

    #[test]
    fn not_browser_commands() {
        for t in [
            "Pauza w pracy była długa i nudna",
            "graj w gry",
            "przewiń w dół",
            "przewiń do góry",
            "przewiń do końca",
            "otwórz nową zakładkę",
            "streść tę stronę",
            "mów głośniej",
            "wycisz",
            "dalej idziemy na obiad",
            "znajdź mi przepis na bigos",
            "zapisz plik",
            "przestań mi przerywać kiedy mówię",
            "otwórz Spotify",
            "Następna piosenka",
            "przypomnij mi o karcie",
        ] {
            assert_eq!(p(t), None, "{t}");
        }
    }

    fn ctx() -> BrowserCtx {
        BrowserCtx { live: true, tab: Some("t1".into()), focused: Some(true), ..Default::default() }
    }

    #[test]
    fn media_only_when_vio_has_it() {
        let pause = Cmd::Media { action: "pause", value: None };
        let play = Cmd::Media { action: "play", value: None };
        let louder = Cmd::Media { action: "louder", value: None };
        let mut c = ctx();
        let m = |playing| Media { tab: "t1".into(), playing, muted: false, current_time: 0.0, duration: 10.0, title: "x".into() };
        c.media = Some(m(true));
        // Vio off or v1 (no capabilities): never.
        assert!(!applies(&pause, false, &|_| false, &c, false));
        assert!(!applies(&pause, true, &|_| false, &c, false));
        let media_rule = |cmd: &Cmd, c: &BrowserCtx| applies(cmd, true, &|_| true, c, false);
        assert!(media_rule(&pause, &c) && !media_rule(&play, &c));
        assert!(!media_rule(&louder, &c), "page at full volume (the default) → system volume");
        c.volume.insert("t1".into(), 0.6);
        assert!(media_rule(&louder, &c), "after „ciszej” the page volume goes back up");
        c.media = Some(m(false));
        assert!(!media_rule(&pause, &c) && media_rule(&play, &c));
        c.focused = Some(false);
        assert!(!media_rule(&play, &c), "a paused video in an unfocused Vio isn't what „graj” means");
        c.media = None;
        let seek = Cmd::Media { action: "seekBy", value: Some(10.0) };
        assert!(!media_rule(&seek, &c) && !media_rule(&play, &c), "Vio not in front, nothing known");
        c.focused = Some(true);
        assert!(media_rule(&seek, &c), "fresh page in front: ask Vio");
        assert!(!media_rule(&play, &c) && !media_rule(&pause, &c), "play / pause stay media keys until Vio reports media");
        assert!(!media_rule(&louder, &c), "volume keeps the system meaning");
        // Dictation path data.
        c.field = Some(Field { tab: "t1".into(), r#ref: "r".into(), kind: "text".into(), label: "x".into(), editable: true });
        assert!(c.text_route() != crate::agent::computer::vio_ctx::TextRoute::Unknown);
    }

    #[test]
    fn capability_and_focus_gates() {
        let c = BrowserCtx::default();
        let none = |_: &str| false;
        assert!(applies(&Cmd::ListWatches, false, &none, &c, false));
        assert!(applies(&Cmd::OpenScene("praca".into()), false, &none, &c, false));
        assert!(!applies(&Cmd::SaveScene("praca".into()), false, &none, &c, false), "saving tabs needs Vio");
        assert!(applies(&Cmd::SaveScene("praca".into()), true, &none, &c, false), "listTabs is v1");
        assert!(!applies(&Cmd::Stop, true, &|_| true, &c, false), "stop goes through stop_all, not a route");
        let only = |want: &'static str| move |c: &str| c == want;
        let find = Cmd::Find("x".into());
        assert!(applies(&find, true, &only("findInPage"), &c, false));
        assert!(!applies(&find, true, &only("scrollTo"), &c, false), "old Vio without findInPage");
        // One-word „wstecz” / „dalej” only while Vio is in front; „dalej” with music = next track.
        let back = Cmd::Tab { op: "back", bare: true };
        let fwd = Cmd::Tab { op: "forward", bare: true };
        assert!(!applies(&back, true, &|_| true, &c, false));
        let f = ctx();
        assert!(applies(&back, true, &|_| true, &f, false));
        assert!(applies(&fwd, true, &|_| true, &f, false));
        assert!(!applies(&fwd, true, &|_| true, &f, true));
        assert!(applies(&Cmd::Tab { op: "mute", bare: false }, true, &only("muteTab"), &c, false));
        assert!(!applies(&Cmd::ScrollTo { query: "x".into(), page: false }, true, &|_| true, &c, false), "scroll_to needs Vio in front");
        assert!(applies(&Cmd::ScrollTo { query: "x".into(), page: true }, true, &|_| true, &c, false), "…unless it names the page");
    }
}
