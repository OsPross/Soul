//! Price watcher: „obserwuj cenę tej strony” stores the page's URL and how to find its price; a
//! background thread re-fetches the page with Soul's own HTTP client every few hours, parses the
//! price from the HTML (shop metadata first, else the text next to the remembered words) and
//! notifies the user when it drops (Island toast + spoken notice). No AI is involved, ever — no
//! REST model call in the background — and Vio is not used for checking (no hidden tabs).
//!
//! Limits: pages that render the price with JavaScript only, or block non-browser requests, can't
//! be watched (the watch is refused right away with an explanation).

use crate::memory::{self, with_db};
use crate::platform::ui::clip;
use rusqlite::params;
use serde::Serialize;
use std::time::Duration;
use tauri::AppHandle;

/// Each watch is fetched at most this often.
const CHECK_EVERY: Duration = Duration::from_secs(3 * 3600);
/// The checker wakes up this often to see which watches are due.
const TICK: Duration = Duration::from_secs(10 * 60);
/// First look shortly after start (not during the startup rush).
const FIRST_DELAY: Duration = Duration::from_secs(120);
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HTML: usize = 3_000_000;
pub const MAX_WATCHES: usize = 30;
/// Stored in `anchor` when the price comes from shop metadata (JSON-LD / meta / itemprop).
const META: &str = "meta";
/// Words before the price that identify it in the page text.
const ANCHOR_WORDS: usize = 4;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    pub id: i64,
    pub url: String,
    pub title: String,
    /// Price when the watch was created.
    pub price: f64,
    pub currency: String,
    /// `meta` or `text:<folded words before the price>`.
    pub anchor: String,
    /// Price at the last check.
    pub last_price: f64,
    pub checked_at: i64,
    pub created_at: i64,
}

// ───────────────────────── parsing ─────────────────────────

const CURRENCIES: &[(&str, &str)] = &[
    ("zł", "zł"), ("zŁ", "zł"), ("zl", "zł"), ("pln", "zł"), ("€", "€"), ("eur", "€"), ("$", "$"), ("usd", "$"), ("£", "£"),
    ("gbp", "£"), ("kč", "Kč"), ("czk", "Kč"),
];

/// „1 299,99”, „1.299,99”, „1,299.99”, „19.99”, „49” → number. Spaces / NBSP between digit groups
/// are thousands separators; with both `,` and `.` the last one is the decimal point; a single
/// separator followed by exactly 3 digits is a thousands separator.
pub fn parse_amount(s: &str) -> Option<f64> {
    let s: String = s.trim().chars().filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\'')).collect();
    let s = s.trim_end_matches(['-', '–', '.', ',']);
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') || !s.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let last = s.rfind([',', '.']);
    let normalized = match last {
        None => s.to_string(),
        Some(i) => {
            let both = s.contains(',') && s.contains('.');
            let decimals = s.len() - i - 1;
            let single_kind = s.matches(s.as_bytes()[i] as char).count() == 1;
            if both || (single_kind && decimals != 3) {
                let (int, frac) = s.split_at(i);
                format!("{}.{}", int.replace([',', '.'], ""), &frac[1..])
            } else {
                s.replace([',', '.'], "")
            }
        }
    };
    normalized.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

/// A price found in text: amount, currency, byte offset of the number.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub amount: f64,
    pub currency: String,
    pub at: usize,
}

fn currency_at(lower: &str) -> Option<&'static str> {
    CURRENCIES.iter().find(|(m, _)| {
        lower.starts_with(m) && !lower[m.len()..].starts_with(|c: char| c.is_alphabetic())
    }).map(|(_, c)| *c)
}

/// Every number with a currency marker right before or after it („299,00 zł”, „€19.99”, „PLN 49”).
pub fn find_prices(text: &str) -> Vec<Found> {
    // ASCII lowercasing keeps byte offsets; „ZŁ” becomes „zŁ” (listed as a marker).
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let nbsp = |j: usize| lower[j..].starts_with('\u{a0}');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.' || b[i - 1] == b',')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_digit() || matches!(b[j], b',' | b'.' | b' ') || nbsp(j)) {
            // A space only between digit groups („1 299”).
            let w = if nbsp(j) { 2 } else { 1 };
            if (w == 2 || b[j] == b' ') && !(j + w < b.len() && b[j + w].is_ascii_digit() && b[j - 1].is_ascii_digit()) {
                break;
            }
            j += w;
        }
        let num = &lower[start..j];
        let after = lower[j..].trim_start_matches([' ', '\u{a0}']);
        let before = lower[..start].trim_end_matches([' ', '\u{a0}']);
        let cur = currency_at(after).or_else(|| {
            CURRENCIES.iter().find(|(m, _)| before.ends_with(m) && !before[..before.len() - m.len()].ends_with(|c: char| c.is_alphabetic())).map(|(_, c)| *c)
        });
        if let (Some(cur), Some(amount)) = (cur, parse_amount(num)) {
            out.push(Found { amount, currency: cur.to_string(), at: start });
        }
        i = j.max(i + 1);
    }
    out
}

/// Up to `len` bytes of `s` from `start`, cut at character boundaries.
fn window(s: &str, start: usize, len: usize) -> &str {
    let mut a = start.min(s.len());
    while !s.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (a + len).min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    &s[a..b]
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    for q in ['"', '\''] {
        let key = format!("{name}={q}");
        if let Some(i) = lower.find(&key) {
            let rest = &tag[i + key.len()..];
            return rest.find(q).map(|e| rest[..e].trim().to_string());
        }
    }
    None
}

/// The price from shop metadata: `product:price:amount` / `og:price:amount` meta, `itemprop="price"`,
/// or JSON-LD `"price"` / `"lowPrice"`. Currency from the matching metadata when present.
pub fn structured_price(html: &str) -> Option<(f64, String)> {
    let lower = html.to_ascii_lowercase();
    let currency = |hint: Option<String>| -> String {
        let code = hint.or_else(|| {
            lower.find("pricecurrency").and_then(|i| {
                let rest = window(html, i, 120);
                rest.split(['"', '\'']).nth(2).map(str::to_string).or_else(|| attr(rest, "content"))
            })
        });
        code.and_then(|c| currency_at(&c.trim().to_lowercase()).map(str::to_string)).unwrap_or_default()
    };
    // <meta …> / <… itemprop="price" …>
    let mut i = 0;
    while let Some(off) = lower[i..].find('<') {
        let s = i + off;
        let Some(e) = lower[s..].find('>') else { break };
        let tag = &html[s..s + e + 1];
        let tl = tag.to_ascii_lowercase();
        if tl.contains("price:amount") || tl.contains("itemprop=\"price\"") || tl.contains("itemprop='price'") {
            if let Some(v) = attr(tag, "content").and_then(|c| parse_amount(&c)) {
                let cur = window(&lower, s, 600)
                    .find("price:currency")
                    .and_then(|k| attr(window(html, s + k, 200), "content"));
                return Some((v, currency(cur)));
            }
            if tl.contains("itemprop") {
                let inner = window(html, s + e + 1, 80);
                if let Some(v) = inner.split('<').next().and_then(|t| find_prices(t).first().map(|f| f.amount).or_else(|| parse_amount(t))) {
                    return Some((v, currency(None)));
                }
            }
        }
        i = s + e + 1;
        if i >= lower.len() {
            break;
        }
    }
    // JSON-LD / inline JSON.
    for key in ["\"price\"", "\"lowprice\""] {
        let mut from = 0;
        while let Some(k) = lower[from..].find(key) {
            let rest = lower[from + k + key.len()..].trim_start();
            if let Some(rest) = rest.strip_prefix(':') {
                let v: String = rest.trim_start().trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect();
                if let Some(v) = parse_amount(&v) {
                    return Some((v, currency(None)));
                }
            }
            from += k + key.len();
        }
    }
    None
}

/// Visible text of an HTML page (scripts / styles dropped, tags → spaces, common entities).
pub fn html_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let Some(off) = lower[i..].find('<') else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..i + off]);
        let s = i + off;
        let skip_to = ["<script", "<style", "<noscript", "<template"].iter().find(|t| lower[s..].starts_with(*t)).map(|t| format!("</{}", &t[1..]));
        i = match skip_to {
            Some(end) => lower[s..].find(&end).map(|e| s + e + end.len()).unwrap_or(html.len()),
            None => s,
        };
        match lower[i..].find('>') {
            Some(e) => i += e + 1,
            None => break,
        }
        out.push(' ');
    }
    let out = out
        .replace("&nbsp;", "\u{a0}")
        .replace("&#160;", "\u{a0}")
        .replace("&amp;", "&")
        .replace("&euro;", "€")
        .replace("&pound;", "£")
        .replace("&#8364;", "€")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fold(s: &str) -> Vec<String> {
    s.split_whitespace().map(crate::agent::fold_word).filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit())).collect()
}

/// The folded words right before byte `at` of `text`.
fn words_before(text: &str, at: usize) -> String {
    let w = fold(&text[..at]);
    w[w.len().saturating_sub(ANCHOR_WORDS)..].join(" ")
}

/// The price a watch looks for in this HTML.
pub fn extract(html: &str, anchor: &str) -> Option<(f64, String)> {
    if anchor == META {
        return structured_price(html);
    }
    let text = html_text(html);
    let want: Vec<String> = anchor.strip_prefix("text:").unwrap_or("").split_whitespace().map(str::to_string).collect();
    let prices = find_prices(&text);
    if want.is_empty() {
        return prices.first().map(|f| (f.amount, f.currency.clone()));
    }
    // The price whose preceding words share the most with the anchor (at least one).
    prices
        .iter()
        .map(|f| {
            let before = fold(&text[..f.at]);
            let near = &before[before.len().saturating_sub(ANCHOR_WORDS + 2)..];
            (want.iter().filter(|w| near.contains(w)).count(), f)
        })
        .filter(|(n, _)| *n > 0)
        .max_by_key(|(n, f)| (*n, usize::MAX - f.at))
        .map(|(_, f)| (f.amount, f.currency.clone()))
}

/// How to watch this page: (price, currency, anchor). `hint` = the user's selected text (an
/// explicit price), else shop metadata, else the first price in the fetched text.
pub fn plan(html: &str, hint: Option<&str>) -> Result<(f64, String, String), String> {
    let text = html_text(html);
    let structured = structured_price(html);
    let want = hint.and_then(|h| find_prices(h).into_iter().next().or_else(|| parse_amount(h).map(|amount| Found { amount, currency: String::new(), at: 0 })));
    let same = |a: f64, b: f64| (a - b).abs() < 0.005;
    if let Some(w) = want {
        if let Some(f) = find_prices(&text).into_iter().find(|f| same(f.amount, w.amount)) {
            return Ok((f.amount, f.currency, format!("text:{}", words_before(&text, f.at))));
        }
        if let Some((p, c)) = structured.filter(|(p, _)| same(*p, w.amount)) {
            return Ok((p, c, META.into()));
        }
        return Err(format!(
            "Zaznaczona cena ({}) nie występuje w kodzie strony — strona pewnie ładuje ją skryptem, więc nie mogę jej obserwować.",
            fmt_price(w.amount, &w.currency)
        ));
    }
    if let Some((p, c)) = structured {
        return Ok((p, c, META.into()));
    }
    if let Some(f) = find_prices(&text).into_iter().next() {
        return Ok((f.amount, f.currency, format!("text:{}", words_before(&text, f.at))));
    }
    Err("Nie widzę ceny w kodzie tej strony (może ładuje się skryptem albo strona blokuje roboty) — nie mogę jej obserwować.".into())
}

pub fn fmt_price(v: f64, currency: &str) -> String {
    let s = if (v - v.round()).abs() < 0.005 { format!("{v:.0}") } else { format!("{v:.2}").replace('.', ",") };
    if currency.is_empty() { s } else { format!("{s} {currency}") }
}

/// A drop worth telling the user about (≥ 1 grosz / cent).
pub fn dropped(last: f64, now: f64) -> bool {
    now + 0.005 < last
}

// ───────────────────────── fetching ─────────────────────────

/// The page's HTML with Soul's HTTP client (http(s) only, ≤ 3 MB, 15 s).
pub fn fetch(url: &str) -> Result<String, String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("Tylko strony http(s).".into());
    }
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
            .build()
            .unwrap_or_default()
    });
    let r = client
        .get(url)
        .header("Accept-Language", "pl-PL,pl;q=0.9,en;q=0.8")
        .send()
        .map_err(|e| format!("Nie mogę pobrać strony: {}", clip(&e.to_string(), 120)))?;
    let status = r.status().as_u16();
    if status >= 400 {
        return Err(format!("Strona odpowiada błędem {status} (może blokuje roboty) — nie mogę jej obserwować."));
    }
    let body = r.text().map_err(|e| format!("Nie mogę odczytać strony: {}", clip(&e.to_string(), 120)))?;
    let mut end = body.len().min(MAX_HTML);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    Ok(body[..end].to_string())
}

// ───────────────────────── storage ─────────────────────────

fn row(r: &rusqlite::Row) -> rusqlite::Result<Watch> {
    Ok(Watch {
        id: r.get(0)?,
        url: r.get(1)?,
        title: r.get(2)?,
        price: r.get(3)?,
        currency: r.get(4)?,
        anchor: r.get(5)?,
        last_price: r.get(6)?,
        checked_at: r.get(7)?,
        created_at: r.get(8)?,
    })
}

pub fn list() -> Result<Vec<Watch>, String> {
    with_db(|c| {
        let mut st = c.prepare(
            "SELECT id, url, title, price, currency, anchor, last_price, checked_at, created_at FROM price_watches ORDER BY created_at",
        )?;
        let rows = st.query_map([], row)?;
        rows.collect()
    })
}

/// Start watching `url` (replaces an older watch of the same page). Blocking: fetches the page.
pub fn add(url: &str, title: &str, hint: Option<&str>) -> Result<Watch, String> {
    if list()?.len() >= MAX_WATCHES && !list()?.iter().any(|w| w.url == url) {
        return Err(format!("Obserwuję już {MAX_WATCHES} cen — usuń którąś („przestań obserwować cenę …”)."));
    }
    let html = fetch(url)?;
    let (price, currency, anchor) = plan(&html, hint)?;
    let now = memory::now_ms();
    let title = clip(title.trim(), 120);
    with_db(|c| {
        c.execute(
            "INSERT INTO price_watches (url, title, price, currency, anchor, last_price, checked_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?3, ?6, ?6)
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, price=excluded.price, currency=excluded.currency,
               anchor=excluded.anchor, last_price=excluded.last_price, checked_at=excluded.checked_at",
            params![url, title, price, currency, anchor, now],
        )
    })?;
    eprintln!("[soul][price] watch added price={price} anchor={}", if anchor == META { "meta" } else { "text" });
    list()?.into_iter().find(|w| w.url == url).ok_or_else(|| "Nie udało się zapisać obserwacji.".into())
}

/// Remove watches whose title or URL contains `query` (empty = the one for `url`).
pub fn remove(query: &str, url: Option<&str>) -> Result<Vec<Watch>, String> {
    let q = query.trim().to_lowercase();
    let gone: Vec<Watch> = list()?
        .into_iter()
        .filter(|w| match (q.is_empty(), url) {
            (true, Some(u)) => w.url == u,
            (true, None) => false,
            _ => w.title.to_lowercase().contains(&q) || w.url.to_lowercase().contains(&q),
        })
        .collect();
    for w in &gone {
        with_db(|c| c.execute("DELETE FROM price_watches WHERE id=?1", [w.id]))?;
    }
    Ok(gone)
}

pub fn remove_id(id: i64) -> Result<(), String> {
    with_db(|c| c.execute("DELETE FROM price_watches WHERE id=?1", [id]).map(|_| ()))
}

fn store_check(id: i64, price: Option<f64>) {
    let now = memory::now_ms();
    let _ = with_db(|c| match price {
        Some(p) => c.execute("UPDATE price_watches SET last_price=?1, checked_at=?2 WHERE id=?3", params![p, now, id]),
        None => c.execute("UPDATE price_watches SET checked_at=?1 WHERE id=?2", params![now, id]),
    });
}

/// One spoken / Island line per watch.
pub fn describe(list: &[Watch]) -> String {
    if list.is_empty() {
        return "Nie obserwuję żadnych cen.".into();
    }
    list.iter()
        .map(|w| {
            let name = if w.title.is_empty() { clip(&w.url, 60) } else { clip(&w.title, 60) };
            format!("{name}: {}", fmt_price(w.last_price, &w.currency))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// ───────────────────────── background check ─────────────────────────

/// Check every due watch once; returns the drops (watch, new price).
fn check_due(now_ms: i64) -> Vec<(Watch, f64)> {
    let mut drops = Vec::new();
    for w in list().unwrap_or_default() {
        if now_ms - w.checked_at < CHECK_EVERY.as_millis() as i64 {
            continue;
        }
        let price = fetch(&w.url).ok().and_then(|html| extract(&html, &w.anchor)).map(|(p, _)| p);
        match price {
            Some(p) => {
                eprintln!("[soul][price] checked id={} {} → {}", w.id, w.last_price, p);
                if dropped(w.last_price, p) {
                    drops.push((w.clone(), p));
                }
            }
            None => eprintln!("[soul][price] checked id={}: price not found this time", w.id),
        }
        store_check(w.id, price);
    }
    drops
}

/// Start the checker thread (app startup).
pub fn start(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("soul-price-watch".into()).spawn(move || {
        std::thread::sleep(FIRST_DELAY);
        loop {
            for (w, now) in check_due(memory::now_ms()) {
                let name = if w.title.is_empty() { clip(&w.url, 60) } else { clip(&w.title, 60) };
                let text = format!("Cena spadła: {name} — {} → {}", fmt_price(w.last_price, &w.currency), fmt_price(now, &w.currency));
                eprintln!("[soul][price] drop id={}", w.id);
                crate::island::toast(&app, "info", &text, None);
                crate::agent::speak_notice(&app, &text);
            }
            std::thread::sleep(TICK);
        }
    });
    if let Err(e) = spawned {
        eprintln!("[soul][price] checker thread failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_in_polish_and_english_formats() {
        for (s, v) in [
            ("1 299,99", 1299.99),
            ("1\u{a0}299,99", 1299.99),
            ("1.299,99", 1299.99),
            ("1,299.99", 1299.99),
            ("19.99", 19.99),
            ("19,9", 19.9),
            ("49", 49.0),
            ("1.299", 1299.0),
            ("2 499,-", 2499.0),
            ("12 345 678", 12345678.0),
        ] {
            assert_eq!(parse_amount(s), Some(v), "{s}");
        }
        for bad in ["", "abc", "0", ",50", "-5"] {
            assert_eq!(parse_amount(bad), None, "{bad}");
        }
    }

    #[test]
    fn prices_need_a_currency() {
        let f = find_prices("Darmowa dostawa od 99 zł! Teraz tylko 1 299,00 zł zamiast 1 499 zł. Model 2024, 3 lata gwarancji. €19.99 or PLN 49");
        let got: Vec<(f64, &str)> = f.iter().map(|f| (f.amount, f.currency.as_str())).collect();
        assert_eq!(got, vec![(99.0, "zł"), (1299.0, "zł"), (1499.0, "zł"), (19.99, "€"), (49.0, "zł")]);
        assert!(find_prices("Rok 2024, 15 sztuk, zlot fanów").is_empty(), "„zlot” is not „zł”");
    }

    #[test]
    fn structured_price_wins() {
        let html = r#"<html><head><meta property="product:price:amount" content="249.90"><meta property="product:price:currency" content="PLN"></head>
            <body><p>Dostawa od 99 zł</p><span class="p">249,90 zł</span></body></html>"#;
        assert_eq!(structured_price(html), Some((249.9, "zł".into())));
        let ld = r#"<script type="application/ld+json">{"@type":"Product","offers":{"price": "1299.00","priceCurrency":"PLN"}}</script>"#;
        assert_eq!(structured_price(ld), Some((1299.0, "zł".into())));
        let micro = r#"<span itemprop="price" content="59.99">59,99 zł</span>"#;
        assert_eq!(structured_price(micro).map(|p| p.0), Some(59.99));
        assert_eq!(structured_price("<p>Cena: 10 zł</p>"), None);
        assert_eq!(plan(html, None).unwrap(), (249.9, "zł".into(), META.into()));
    }

    #[test]
    fn text_anchor_finds_the_same_price_after_a_change() {
        let page = |p: &str| format!("<html><body><nav>Darmowa dostawa od 99 zł</nav><script>var x='5 zł';</script><h1>Buty Run</h1><div>Cena teraz: <b>{p} zł</b></div><footer>Rabat 10 zł</footer></body></html>");
        let html = page("349,00");
        // The user selected the price: anchored on the words before it.
        let (price, cur, anchor) = plan(&html, Some("349,00 zł")).unwrap();
        assert_eq!((price, cur.as_str()), (349.0, "zł"));
        assert!(anchor.starts_with("text:") && anchor.contains("cena teraz"), "{anchor}");
        assert_eq!(extract(&page("299,99"), &anchor), Some((299.99, "zł".into())));
        assert!(dropped(349.0, 299.99) && !dropped(349.0, 349.0) && !dropped(299.99, 349.0));
        // Without a hint: the first price in the visible text (scripts are not text).
        assert_eq!(plan(&html, None).unwrap().0, 99.0);
        assert!(!html_text(&html).contains("var x"));
        // A selected price the fetched page doesn't have (rendered by JS): refused.
        assert!(plan(&html, Some("123 zł")).unwrap_err().contains("skryptem"));
        assert!(plan("<p>nic</p>", None).is_err());
        assert_eq!(fmt_price(1299.0, "zł"), "1299 zł");
        assert_eq!(fmt_price(19.9, "€"), "19,90 €");
    }
}
