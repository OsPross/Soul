//! Spotify account: OAuth 2.0 Authorization Code + PKCE over a one-shot loopback listener.
//! Tokens live only in `<app data>/spotify-auth.json` — never in settings, events or logs.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use parking_lot::Mutex;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const REDIRECT_URI: &str = "http://127.0.0.1:43821/callback";
const LISTEN: &str = "127.0.0.1:43821";
const SCOPES: &str = "user-modify-playback-state user-read-playback-state user-read-currently-playing user-library-modify user-library-read playlist-read-private playlist-read-collaborative";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyAuthStore {
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: u64,
    pub display_name: Option<String>,
    /// "premium" | "free" | …
    pub product: Option<String>,
}

/// What the frontend sees (no tokens).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyStatus {
    pub connected: bool,
    pub client_id: String,
    pub display_name: Option<String>,
    pub is_premium: bool,
    pub installed: bool,
    pub running: bool,
}

static FILE: OnceLock<PathBuf> = OnceLock::new();
/// Serializes refresh + save so two calls never refresh the same token twice.
static STORE_LOCK: Mutex<()> = Mutex::new(());

pub fn init(app_data_dir: PathBuf) {
    let _ = FILE.set(app_data_dir.join("spotify-auth.json"));
}

pub fn load() -> SpotifyAuthStore {
    FILE.get()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(store: &SpotifyAuthStore) -> Result<(), String> {
    let path = FILE.get().ok_or("Spotify storage not ready.")?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(store).map_err(|e| e.to_string())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp).map_err(|e| format!("Could not save Spotify login: {e}"))?;
    f.write_all(&json).map_err(|e| format!("Could not save Spotify login: {e}"))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("Could not save Spotify login: {e}"))
}

pub fn is_connected() -> bool {
    !load().refresh_token.is_empty()
}

pub fn status() -> SpotifyStatus {
    let s = load();
    SpotifyStatus {
        connected: !s.refresh_token.is_empty(),
        is_premium: s.product.as_deref() == Some("premium"),
        client_id: s.client_id,
        display_name: s.display_name,
        installed: super::local::is_installed(),
        running: super::local::is_running(),
    }
}

/// Forget the tokens; the Client ID stays for the next „Połącz”.
pub fn disconnect() -> Result<(), String> {
    let _guard = STORE_LOCK.lock();
    let s = load();
    save(&SpotifyAuthStore { client_id: s.client_id, ..Default::default() })
}

/// `len` chars of the PKCE alphabet `[A-Za-z0-9-._~]` (RFC 7636 §4.1).
pub fn random_string(len: usize) -> Result<String, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let rng = SystemRandom::new();
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        rng.fill(&mut buf).map_err(|_| "No secure random source.".to_string())?;
        // 198 = 3 × 66: rejecting the rest keeps every character equally likely.
        for b in buf.iter().filter(|b| **b < 198) {
            if out.len() < len {
                out.push(ALPHABET[usize::from(*b) % ALPHABET.len()] as char);
            }
        }
    }
    Ok(out)
}

/// `BASE64URL(SHA256(verifier))` without padding (S256).
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()))
}

fn enc(s: &str) -> String {
    // form_urlencoded writes a space as '+'; a literal '+' is already %2B, so this is exact.
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={}&state={}&scope={}",
        enc(client_id),
        enc(REDIRECT_URI),
        enc(challenge),
        enc(state),
        enc(SCOPES)
    )
}

/// Refresh a minute early so a request never races the expiry.
pub fn token_expired(expires_at_ms: u64, now_ms: u64) -> bool {
    now_ms + 60_000 >= expires_at_ms
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn http() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse, String> {
    let resp = http()
        .post(TOKEN_URL)
        .form(form)
        .send()
        .map_err(|e| format!("Spotify login failed: {}", e.without_url()))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        // {"error":"invalid_grant","error_description":"…"} — no secrets in it.
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let why = v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("");
        return Err(format!("Spotify login failed (HTTP {}): {why}", status.as_u16()));
    }
    serde_json::from_str(&body).map_err(|_| "Spotify login: unexpected token response.".into())
}

/// A fresh access token, refreshed (and saved) when it expires within a minute.
pub fn valid_token() -> Result<String, String> {
    let _guard = STORE_LOCK.lock();
    let mut s = load();
    if s.refresh_token.is_empty() {
        return Err(super::NOT_CONNECTED.into());
    }
    if !token_expired(s.expires_at_ms, now_ms()) {
        return Ok(s.access_token);
    }
    let t = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &s.refresh_token),
        ("client_id", &s.client_id),
    ])?;
    s.access_token = t.access_token;
    if let Some(r) = t.refresh_token {
        s.refresh_token = r;
    }
    s.expires_at_ms = now_ms() + t.expires_in * 1000;
    save(&s)?;
    Ok(s.access_token)
}

/// A started login: the bound loopback listener and the PKCE secrets.
pub struct Pending {
    listener: TcpListener,
    client_id: String,
    verifier: String,
    state: String,
    pub url: String,
}

/// Bind the loopback port and build the authorize URL. Fast; call before opening the browser.
pub fn begin(client_id: &str) -> Result<Pending, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() || !client_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Wklej poprawny Spotify Client ID (z developer.spotify.com/dashboard).".into());
    }
    let listener = TcpListener::bind(LISTEN)
        .map_err(|_| "Logowanie do Spotify już trwa albo port 43821 jest zajęty.".to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let verifier = random_string(64)?;
    let state = random_string(24)?;
    let url = authorize_url(client_id, &code_challenge(&verifier), &state);
    Ok(Pending { listener, client_id: client_id.into(), verifier, state, url })
}

fn respond(stream: &mut TcpStream, status: &str, text: &str) {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Soul</title><body style=\"font:16px system-ui;background:#111;color:#eee;display:grid;place-items:center;height:90vh\"><p>{text}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
}

/// Wait (≤ 180 s) for the browser redirect, exchange the code, fetch the profile, save. Blocking.
pub fn finish(p: Pending) -> Result<SpotifyStatus, String> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let code = loop {
        if Instant::now() >= deadline {
            return Err("Spotify: czas na zalogowanie minął.".into());
        }
        let mut stream = match p.listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(format!("Spotify login: {e}")),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/callback") {
            respond(&mut stream, "404 Not Found", "");
            continue;
        }
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{path}")) else { continue };
        let param = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
        if param("state").as_deref() != Some(p.state.as_str()) {
            respond(&mut stream, "400 Bad Request", "Nieprawidłowe żądanie logowania.");
            return Err("Spotify login: state mismatch.".into());
        }
        match (param("code"), param("error")) {
            (Some(code), _) => {
                respond(&mut stream, "200 OK", "Połączono ze Spotify! Możesz zamknąć tę kartę.");
                break code;
            }
            (None, err) => {
                respond(&mut stream, "200 OK", "Nie połączono ze Spotify. Możesz zamknąć tę kartę.");
                return Err(format!("Spotify login cancelled: {}", err.unwrap_or_default()));
            }
        }
    };
    drop(p.listener);

    let t = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", &p.client_id),
        ("code_verifier", &p.verifier),
    ])?;
    let me: serde_json::Value = http()
        .get("https://api.spotify.com/v1/me")
        .bearer_auth(&t.access_token)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("Spotify profile: {}", e.without_url()))?;
    let store = SpotifyAuthStore {
        client_id: p.client_id,
        access_token: t.access_token,
        refresh_token: t.refresh_token.unwrap_or_default(),
        expires_at_ms: now_ms() + t.expires_in * 1000,
        display_name: me["display_name"].as_str().map(str::to_string),
        product: me["product"].as_str().map(str::to_string),
    };
    {
        let _guard = STORE_LOCK.lock();
        save(&store)?;
    }
    eprintln!("[soul][spotify] connected ({})", store.product.as_deref().unwrap_or("?"));
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = random_string(64).unwrap();
        assert_eq!(v.len(), 64);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
        assert_ne!(v, random_string(64).unwrap());
    }

    #[test]
    fn authorize_url_is_encoded() {
        let url = authorize_url("abc123", "chal-_", "st8");
        assert!(url.starts_with("https://accounts.spotify.com/authorize?client_id=abc123&response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A43821%2Fcallback"));
        assert!(url.contains("code_challenge_method=S256&code_challenge=chal-_&state=st8"));
        assert!(url.contains("scope=user-modify-playback-state%20user-read-playback-state"));
        assert!(!url.contains('+'));
    }

    #[test]
    fn expiry_has_a_minute_margin() {
        assert!(!token_expired(200_000, 100_000));
        assert!(token_expired(200_000, 140_000));
        assert!(token_expired(0, 1));
    }
}
