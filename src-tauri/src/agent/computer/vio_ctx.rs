//! Vio API v2 on Soul's side, without I/O (`vio.rs` feeds it): the capabilities from `hello`, the
//! browser context kept up to date by events (active tab, window focus, selection, focused field,
//! media), the user's signals from Vio (Soul icon, panel button, STOP) and the `setSoulState`
//! debounce. An old Vio (no `hello`) has no capabilities: every v2 feature is skipped.

use crate::platform::ui::clip;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Events Soul subscribes to (only the ones Vio announces in `hello`).
pub const WANTED_EVENTS: &[&str] = &[
    "tabActivated", "pageLoaded", "tabClosed", "windowFocus", "selectionChanged", "focusedField", "mediaState",
    "soulIconClicked", "panelAction", "stopRequested",
];

/// What `hello` / `capabilities` returned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caps {
    pub api_version: u64,
    pub vio_version: String,
    pub commands: Vec<String>,
    pub events: Vec<String>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

impl Caps {
    pub fn parse(v: &Value) -> Option<Self> {
        let commands = strings(&v["capabilities"]);
        if commands.is_empty() {
            return None;
        }
        Some(Self {
            api_version: v["apiVersion"].as_u64().unwrap_or(0),
            vio_version: v["vioVersion"].as_str().unwrap_or_default().to_string(),
            commands,
            events: strings(&v["events"]),
        })
    }

    pub fn has(&self, cmd: &str) -> bool {
        self.commands.iter().any(|c| c == cmd)
    }

    /// The events worth subscribing to that this Vio sends.
    pub fn subscribable(&self) -> Vec<&'static str> {
        WANTED_EVENTS.iter().copied().filter(|e| self.events.iter().any(|x| x == e)).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub tab: String,
    pub r#ref: String,
    pub kind: String,
    pub label: String,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub tab: String,
    pub playing: bool,
    pub muted: bool,
    pub current_time: f64,
    pub duration: f64,
    pub title: String,
}

/// The user did something in Vio that Soul must react to.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    IconClicked,
    Panel { action: String, payload: Value },
    Stop,
}

/// What Soul knows about Vio from events. Reset on every (re)connect.
#[derive(Debug, Clone, Default)]
pub struct BrowserCtx {
    /// Events are subscribed: the fields below are kept up to date.
    pub live: bool,
    pub tab: Option<String>,
    pub title: String,
    pub url: String,
    /// A normal Vio window has the focus (`None` = not told yet).
    pub focused: Option<bool>,
    pub selection: String,
    pub field: Option<Field>,
    pub media: Option<Media>,
    /// Playback rate / element volume (0-1) Soul set per tab (`mediaState` carries neither).
    pub rate: HashMap<String, f64>,
    pub volume: HashMap<String, f64>,
}

fn key(v: &Value) -> Option<String> {
    super::vio::tab_key(v)
}

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().trim().to_string()
}

/// How dictation / `type_text` should reach a Vio page, judged from the focused field.
#[derive(Debug, Clone, PartialEq)]
pub enum TextRoute {
    /// `insertText` into this tab: focus is in an editable field.
    Insert(String),
    /// A password field: never typed into, never via the clipboard.
    Forbidden,
    /// No event data: ask Vio (`listTabs` + `insertText`, clipboard fallback) as before.
    Unknown,
}

impl BrowserCtx {
    /// One event. Returns a signal for the events that need an action.
    pub fn apply(&mut self, event: &str, data: &Value) -> Option<Signal> {
        let tab = key(&data["tabId"]);
        let same = |t: &Option<String>| tab.is_some() && *t == tab;
        match event {
            "tabActivated" => {
                self.tab = tab;
                self.title = text(data, "title");
                self.url = text(data, "url");
                self.selection.clear();
                self.field = None;
            }
            "pageLoaded" => {
                if self.tab.is_none() || same(&self.tab) {
                    self.tab = tab.clone();
                    self.title = text(data, "title");
                    self.url = text(data, "url");
                    self.selection.clear();
                    self.field = None;
                }
                if self.media.as_ref().is_some_and(|m| Some(&m.tab) == tab.as_ref()) {
                    self.media = None;
                }
            }
            "tabClosed" => {
                if same(&self.tab) {
                    self.tab = None;
                    self.title.clear();
                    self.url.clear();
                    self.selection.clear();
                    self.field = None;
                }
                if self.media.as_ref().is_some_and(|m| Some(&m.tab) == tab.as_ref()) {
                    self.media = None;
                }
                if let Some(t) = &tab {
                    self.rate.remove(t);
                    self.volume.remove(t);
                }
            }
            "windowFocus" => self.focused = data["focused"].as_bool(),
            "selectionChanged" => self.selection = clip(&text(data, "text"), 2000),
            "focusedField" => {
                self.field = match (data.is_object(), tab) {
                    (true, Some(tab)) => Some(Field {
                        tab,
                        r#ref: key(&data["ref"]).unwrap_or_default(),
                        kind: text(data, "type").to_lowercase(),
                        label: text(data, "label"),
                        editable: data["editable"].as_bool() == Some(true),
                    }),
                    _ => None,
                }
            }
            "mediaState" => {
                if let Some(tab) = tab {
                    self.media = Some(Media {
                        tab,
                        playing: data["playing"].as_bool() == Some(true),
                        muted: data["muted"].as_bool() == Some(true),
                        current_time: data["currentTime"].as_f64().unwrap_or(0.0),
                        duration: data["duration"].as_f64().unwrap_or(0.0),
                        title: text(data, "title"),
                    });
                }
            }
            "soulIconClicked" => return Some(Signal::IconClicked),
            "panelAction" => {
                return Some(Signal::Panel { action: text(data, "action"), payload: data["payload"].clone() });
            }
            "stopRequested" => return Some(Signal::Stop),
            _ => {}
        }
        None
    }

    pub fn text_route(&self) -> TextRoute {
        match &self.field {
            _ if !self.live => TextRoute::Unknown,
            Some(f) if f.kind == "password" => TextRoute::Forbidden,
            Some(f) if f.editable => TextRoute::Insert(f.tab.clone()),
            _ => TextRoute::Unknown,
        }
    }

    /// Media in a Vio tab that a voice command can act on: something plays, or the user is
    /// looking at the tab that has (paused) media.
    pub fn media_target(&self) -> Option<&Media> {
        self.media.as_ref().filter(|m| m.playing || (self.focused == Some(true) && self.tab.as_ref() == Some(&m.tab)))
    }

    /// One line for the agent's observation (inside `<screen>`: the selection is page data).
    pub fn observation(&self) -> Option<String> {
        if !self.live {
            return None;
        }
        let mut parts = Vec::new();
        match self.focused {
            Some(true) => parts.push("Vio window focused".to_string()),
            Some(false) => parts.push("Vio window not focused".to_string()),
            None => {}
        }
        if !self.selection.trim().is_empty() {
            parts.push(format!("selected text: \"{}\"", crate::platform::ui::sanitize(&clip(&self.selection, 300))));
        }
        if let Some(f) = &self.field {
            let pw = if f.kind == "password" { " password" } else { "" };
            parts.push(format!("focused field: {} \"{}\"{pw}", if f.kind.is_empty() { "text" } else { &f.kind }, clip(&f.label, 60)));
        }
        if let Some(m) = &self.media {
            let t = |s: f64| format!("{}:{:02}", (s.max(0.0) as u64) / 60, (s.max(0.0) as u64) % 60);
            parts.push(format!(
                "media: {} \"{}\" {}/{}{}",
                if m.playing { "playing" } else { "paused" },
                clip(&m.title, 60),
                t(m.current_time),
                t(m.duration),
                if m.muted { " muted" } else { "" }
            ));
        }
        (!parts.is_empty()).then(|| format!("BROWSER: {}", parts.join(" · ")))
    }
}


/// Soul's Island / overlay state → the Vio icon state (`None` = keep the current one).
pub fn soul_state_for(overlay: &str) -> Option<&'static str> {
    Some(match overlay {
        "listening" => "listening",
        "thinking" | "processing" | "retrying" => "thinking",
        "speaking" => "speaking",
        "done" | "error" | "idle" => "idle",
        _ => return None,
    })
}

/// Minimum time between two `setSoulState` calls; the latest state always goes out.
pub const STATE_GAP: Duration = Duration::from_millis(250);

/// Debounce for `setSoulState`: polled on a timer; sends only a changed state, at most once per
/// [`STATE_GAP`] — a burst of states ends with the last one.
#[derive(Debug, Default)]
pub struct StateGate {
    sent: Option<(String, String)>,
    at: Option<Instant>,
}

impl StateGate {
    /// Whether `want` (state, tooltip) should be sent now.
    pub fn take(&mut self, want: &(String, String), now: Instant) -> bool {
        if self.sent.as_ref() == Some(want) || self.at.is_some_and(|at| now.saturating_duration_since(at) < STATE_GAP) {
            return false;
        }
        self.sent = Some(want.clone());
        self.at = Some(now);
        true
    }

    /// A new connection: Vio starts at idle.
    pub fn reset(&mut self) {
        self.sent = Some(("idle".into(), String::new()));
        self.at = None;
    }
}

/// Reconnect delay after a failed connect: doubles up to 30 s.
pub fn next_backoff(cur: Duration) -> Duration {
    (cur * 2).clamp(Duration::from_secs(2), Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hello() -> Value {
        json!({ "apiVersion": 2, "vioVersion": "0.1.0",
            "capabilities": ["hello", "listTabs", "getReadable", "mediaControl", "setSoulState"],
            "events": ["tabActivated", "focusedField", "stopRequested", "futureEvent"] })
    }

    #[test]
    fn capabilities_gate_features() {
        let c = Caps::parse(&hello()).unwrap();
        assert_eq!((c.api_version, c.vio_version.as_str()), (2, "0.1.0"));
        assert!(c.has("getReadable") && c.has("setSoulState"));
        assert!(!c.has("screenshot") && !c.has("highlight"), "not announced → not used");
        assert_eq!(c.subscribable(), vec!["tabActivated", "focusedField", "stopRequested"], "unknown events are never subscribed");
        // v1 Vio: `hello` fails ("unknown command") or answers without a list → no v2 features.
        assert!(Caps::parse(&json!({})).is_none());
        assert!(Caps::parse(&json!({ "capabilities": [] })).is_none());
    }

    #[test]
    fn events_build_the_context() {
        let mut c = BrowserCtx { live: true, ..Default::default() };
        assert_eq!(c.observation(), None);
        c.apply("windowFocus", &json!({ "focused": true }));
        c.apply("tabActivated", &json!({ "tabId": "k3", "url": "https://shop.pl/p", "title": "Buty" }));
        assert_eq!((c.tab.as_deref(), c.title.as_str()), (Some("k3"), "Buty"));
        c.apply("selectionChanged", &json!({ "tabId": "k3", "text": "Buty \"Run\" 299 zł" }));
        c.apply("focusedField", &json!({ "tabId": "k3", "ref": "a1-1", "type": "email", "label": "E-mail", "editable": true }));
        c.apply("mediaState", &json!({ "tabId": "k3", "playing": true, "muted": false, "currentTime": 65.4, "duration": 200.0, "title": "Reklama" }));
        let obs = c.observation().unwrap();
        assert!(obs.starts_with("BROWSER: Vio window focused"), "{obs}");
        assert!(obs.contains("selected text: \"Buty 'Run' 299 zł\""), "quotes neutralized: {obs}");
        assert!(obs.contains("focused field: email \"E-mail\""), "{obs}");
        assert!(obs.contains("media: playing \"Reklama\" 1:05/3:20"), "{obs}");
        assert_eq!(c.text_route(), TextRoute::Insert("k3".into()));
        // Blur, then a password field.
        c.apply("focusedField", &Value::Null);
        assert_eq!(c.text_route(), TextRoute::Unknown);
        c.apply("focusedField", &json!({ "tabId": "k3", "ref": "a1-2", "type": "password", "label": "Hasło", "editable": false }));
        assert_eq!(c.text_route(), TextRoute::Forbidden);
        assert!(c.observation().unwrap().contains("\"Hasło\" password"));
        // Navigation clears page state, keeps media of other tabs.
        c.apply("pageLoaded", &json!({ "tabId": "k3", "url": "https://shop.pl/q", "title": "Inne" }));
        assert!(c.field.is_none() && c.selection.is_empty() && c.media.is_none());
        assert_eq!(c.url, "https://shop.pl/q");
        // A background tab loading doesn't change the active one.
        c.apply("pageLoaded", &json!({ "tabId": "zz", "url": "https://x.pl", "title": "X" }));
        assert_eq!(c.tab.as_deref(), Some("k3"));
        c.apply("mediaState", &json!({ "tabId": "zz", "playing": false, "title": "Film" }));
        assert!(c.media_target().is_none(), "paused media in a background tab is not a target");
        c.apply("tabActivated", &json!({ "tabId": "zz", "url": "https://x.pl", "title": "X" }));
        assert_eq!(c.media_target().unwrap().title, "Film", "paused media in the tab the user looks at");
        c.apply("tabClosed", &json!({ "tabId": "zz" }));
        assert!(c.tab.is_none() && c.media.is_none());
        c.apply("windowFocus", &json!({ "focused": false }));
        assert_eq!(c.focused, Some(false));
    }

    #[test]
    fn signals_and_old_vio() {
        let mut c = BrowserCtx::default();
        assert_eq!(c.apply("soulIconClicked", &json!({})), Some(Signal::IconClicked));
        assert_eq!(c.apply("stopRequested", &json!({})), Some(Signal::Stop));
        assert_eq!(
            c.apply("panelAction", &json!({ "action": "open", "payload": "https://a.pl" })),
            Some(Signal::Panel { action: "open".into(), payload: json!("https://a.pl") })
        );
        assert_eq!(c.apply("unknownEvent", &json!({})), None);
        // Not subscribed (old Vio): no context, the old dictation path.
        c.apply("focusedField", &json!({ "tabId": "k", "ref": "r", "type": "text", "editable": true }));
        assert_eq!(c.text_route(), TextRoute::Unknown);
        assert_eq!(c.observation(), None);
    }

    #[test]
    fn soul_state_is_debounced() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let st = |s: &str| (s.to_string(), String::new());
        let mut g = StateGate::default();
        g.reset();
        assert!(!g.take(&st("idle"), ms(0)), "Vio starts idle");
        assert!(g.take(&st("listening"), ms(0)));
        // A burst within the gap: nothing until the gap passes, then only the latest.
        assert!(!g.take(&st("thinking"), ms(50)));
        assert!(!g.take(&st("speaking"), ms(120)));
        assert!(g.take(&st("speaking"), ms(260)));
        assert!(!g.take(&st("speaking"), ms(900)), "same state is not resent");
        assert!(g.take(&("acting".into(), "Klikam".into()), ms(900)));
        assert!(g.take(&("acting".into(), "Wpisuję".into()), ms(1200)), "a new tooltip is a change");
        assert_eq!(soul_state_for("processing"), Some("thinking"));
        assert_eq!(soul_state_for("done"), Some("idle"));
        assert_eq!(soul_state_for("confirm"), None);
    }

    #[test]
    fn backoff_doubles_to_30s() {
        let mut d = Duration::from_secs(2);
        let mut seen = vec![];
        for _ in 0..6 {
            d = next_backoff(d);
            seen.push(d.as_secs());
        }
        assert_eq!(seen, vec![4, 8, 16, 30, 30, 30]);
    }
}
