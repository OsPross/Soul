//! `close_app` target resolution, shared by the Windows and X11 backends.
//!
//! A spoken name („discord”, „zamknij steama”) is resolved against a process snapshot by
//! executable name — a known-apps alias map first, then fuzzy match on the exe stem and the
//! product name, and window titles only as a last resort. The match is then walked up to the
//! app's root process (`steamwebhelper` → `steam`, a Chrome renderer → the browser process), so
//! the backend can close / terminate the whole tree. Pure and unit-tested; backends only feed
//! the snapshot and execute the plan.

use super::apps::fuzzy_score;
use std::collections::{HashMap, HashSet};

/// One process from the backend's snapshot.
#[derive(Debug, Clone, Default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// Executable file name (`Discord.exe`, `steam`).
    pub exe: String,
    /// Product name from the version resource (Windows, windowed processes only).
    pub product: Option<String>,
    /// Title of a visible top-level window owned by this process.
    pub title: Option<String>,
}

/// Apps whose exe name differs from what people say, or that need special quitting.
struct KnownApp {
    /// Spoken names, ASCII-folded lowercase.
    aliases: &'static [&'static str],
    /// Main exe stem (lowercase, no `.exe`).
    exe: &'static str,
    /// Other exe stems of the same app (helpers), used only to climb to the main process.
    family: &'static [&'static str],
    display: &'static str,
    /// Closing the window only hides it to the tray: "close" means quit.
    tray: bool,
    /// The app's own quit command, opened like a URL.
    quit_url: Option<&'static str>,
}

const fn app(aliases: &'static [&'static str], exe: &'static str, display: &'static str) -> KnownApp {
    KnownApp {
        aliases,
        exe,
        family: &[],
        display,
        tray: false,
        quit_url: None,
    }
}

const fn tray(aliases: &'static [&'static str], exe: &'static str, display: &'static str) -> KnownApp {
    KnownApp {
        aliases,
        exe,
        family: &[],
        display,
        tray: true,
        quit_url: None,
    }
}

const KNOWN: &[KnownApp] = &[
    tray(&["discord", "dyskord", "dc"], "discord", "Discord"),
    KnownApp {
        aliases: &["steam", "steama", "steamie", "stim"],
        exe: "steam",
        family: &["steamwebhelper", "steamservice", "gameoverlayui"],
        display: "Steam",
        tray: true,
        quit_url: Some("steam://exit"),
    },
    tray(&["spotify", "spotifaj", "spotifaja", "spotifya"], "spotify", "Spotify"),
    KnownApp {
        aliases: &["fivem", "five m", "fajwem", "fajw em", "gta rp", "fivem client"],
        exe: "fivem",
        family: &[],
        display: "FiveM",
        tray: false,
        quit_url: None,
    },
    app(&["chrome", "google chrome", "chroma", "przegladarka chrome"], "chrome", "Chrome"),
    app(&["edge", "microsoft edge", "edga"], "msedge", "Microsoft Edge"),
    app(&["firefox", "firefoxa", "mozilla"], "firefox", "Firefox"),
    app(&["opera", "opera gx"], "opera", "Opera"),
    app(&["brave"], "brave", "Brave"),
    tray(&["teams", "microsoft teams", "teamsy"], "ms-teams", "Teams"),
    tray(&["telegram", "telegrama"], "telegram", "Telegram"),
    tray(&["slack", "slacka"], "slack", "Slack"),
    tray(&["epic", "epic games", "epic games launcher"], "epicgameslauncher", "Epic Games"),
    tray(&["battle net", "battlenet", "battle"], "battle.net", "Battle.net"),
    app(&["whatsapp", "whatsappa", "wacap"], "whatsapp", "WhatsApp"),
    app(&["vs code", "vscode", "visual studio code", "code"], "code", "VS Code"),
    app(&["obs", "obs studio"], "obs64", "OBS"),
    app(&["word", "worda", "microsoft word"], "winword", "Word"),
    app(&["excel", "excela", "microsoft excel"], "excel", "Excel"),
    app(&["powerpoint", "powerpointa", "microsoft powerpoint"], "powerpnt", "PowerPoint"),
    app(&["notatnik", "notepad"], "notepad", "Notatnik"),
    app(&["kalkulator", "kalkulatora", "calculator"], "calculatorapp", "Kalkulator"),
];

/// Never closed, whatever the query says (shell, session and system processes).
const PROTECTED: &[&str] = &[
    "system", "idle", "registry", "smss", "csrss", "wininit", "winlogon", "services", "lsass",
    "svchost", "dwm", "explorer", "fontdrvhost", "sihost", "ctfmon", "conhost", "runtimebroker",
    "startmenuexperiencehost", "shellexperiencehost", "searchhost", "textinputhost", "taskhostw",
    "lockapp", "memory compression", "systemd", "init", "xorg", "xwayland", "gnome-shell",
    "plasmashell", "kwin_x11", "kwin_wayland", "xfwm4", "dbus-daemon", "pulseaudio", "pipewire",
    "soul",
];

/// What to close and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseTarget {
    /// Name shown to the user („Discord”).
    pub label: String,
    /// Root processes of the app (usually one).
    pub roots: Vec<u32>,
    /// Main exe stem, lowercase.
    pub exe: String,
    /// Goes to the tray on WM_CLOSE: skip the graceful window close.
    pub tray: bool,
    /// The app's own quit command (`steam://exit`).
    pub quit_url: Option<&'static str>,
}

/// Lowercase exe name without directory and `.exe`.
pub fn stem(exe: &str) -> String {
    let base = exe.rsplit(['\\', '/']).next().unwrap_or(exe).trim().to_lowercase();
    base.strip_suffix(".exe").map(str::to_string).unwrap_or(base)
}

/// Lowercase, Polish diacritics folded, punctuation → spaces, `.exe` dropped.
fn fold(s: &str) -> String {
    let folded: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    let words: Vec<&str> = folded.split_whitespace().filter(|w| *w != "exe").collect();
    words.join(" ")
}

fn is_protected(p: &Proc, own_pid: u32) -> bool {
    p.pid == own_pid || p.pid <= 4 || PROTECTED.contains(&stem(&p.exe).as_str())
}

fn known_app(query: &str) -> Option<&'static KnownApp> {
    let q = fold(query);
    let compact: String = q.chars().filter(|c| !c.is_whitespace()).collect();
    KNOWN.iter().find(|k| {
        k.exe == compact
            || k.aliases
                .iter()
                .any(|a| *a == q || a.replace(' ', "") == compact)
    })
}

/// Parent belongs to the same app as the child: same exe, a helper named after it
/// (`steam` ← `steamwebhelper`, `fivem` ← `fivem_b2802_gtaprocess`) or a known family member.
fn same_app(parent: &str, child: &str) -> bool {
    if parent == child || (parent.len() >= 3 && child.starts_with(parent)) {
        return true;
    }
    KNOWN.iter().any(|k| {
        let member = |s: &str| s == k.exe || k.family.contains(&s);
        member(parent) && member(child)
    })
}

/// Climb from `pid` to the topmost ancestor that is still the same app.
fn root_of(pid: u32, by_pid: &HashMap<u32, &Proc>, own_pid: u32) -> u32 {
    let mut cur = pid;
    for _ in 0..64 {
        let Some(p) = by_pid.get(&cur) else { break };
        let Some(parent) = by_pid.get(&p.ppid) else { break };
        if parent.pid == p.pid || is_protected(parent, own_pid) || !same_app(&stem(&parent.exe), &stem(&p.exe)) {
            break;
        }
        cur = parent.pid;
    }
    cur
}

fn display_name(p: &Proc) -> String {
    if let Some(product) = p.product.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return product.to_string();
    }
    let s = stem(&p.exe);
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

/// Resolve `query` against `procs`. `own_pid` (Soul) is never a target.
pub fn resolve(query: &str, procs: &[Proc], own_pid: u32) -> Result<CloseTarget, String> {
    let q = fold(query);
    if q.is_empty() {
        return Err("Nie podano nazwy aplikacji.".into());
    }
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let usable: Vec<&Proc> = procs.iter().filter(|p| !is_protected(p, own_pid)).collect();
    let roots_of = |matched: &[&Proc]| -> Vec<u32> {
        let mut roots: Vec<u32> = Vec::new();
        for p in matched {
            let r = root_of(p.pid, &by_pid, own_pid);
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        roots
    };

    // 1. Known app: by its main exe, else climb from one of its helpers.
    if let Some(k) = known_app(query) {
        let main: Vec<&Proc> = usable.iter().copied().filter(|p| stem(&p.exe) == k.exe).collect();
        let matched = if main.is_empty() {
            usable
                .iter()
                .copied()
                .filter(|p| k.family.contains(&stem(&p.exe).as_str()))
                .collect()
        } else {
            main
        };
        if matched.is_empty() {
            return Err(format!("{} nie jest uruchomiony.", k.display));
        }
        return Ok(CloseTarget {
            label: k.display.to_string(),
            roots: roots_of(&matched),
            exe: k.exe.to_string(),
            tray: k.tray,
            quit_url: k.quit_url,
        });
    }

    // 2. Fuzzy on exe stem / product name; 3. window title as the last resort.
    let best = |score: &dyn Fn(&Proc) -> i32| -> Vec<&Proc> {
        let scored: Vec<(i32, &Proc)> = usable
            .iter()
            .map(|p| (score(p), *p))
            .filter(|(s, _)| *s > 0)
            .collect();
        let top = scored.iter().map(|(s, _)| *s).max().unwrap_or(0);
        scored.into_iter().filter(|(s, _)| *s == top).map(|(_, p)| p).collect()
    };
    let mut matched = best(&|p: &Proc| {
        let by_exe = fuzzy_score(&q, &fold(&stem(&p.exe)));
        let by_product = p.product.as_deref().map(|s| fuzzy_score(&q, &fold(s))).unwrap_or(0);
        by_exe.max(by_product)
    });
    if matched.is_empty() {
        matched = best(&|p: &Proc| p.title.as_deref().map(|t| fuzzy_score(&q, &fold(t))).unwrap_or(0));
    }
    if matched.is_empty() {
        return Err(format!("Nie znaleziono uruchomionej aplikacji „{}”.", query.trim()));
    }
    let roots = roots_of(&matched);
    let root = by_pid[&roots[0]];
    let exe = stem(&root.exe);
    // A helper may resolve to a known app's main process (steamwebhelper → steam).
    let k = KNOWN.iter().find(|k| k.exe == exe);
    Ok(CloseTarget {
        label: k.map(|k| k.display.to_string()).unwrap_or_else(|| display_name(root)),
        roots,
        exe,
        tray: k.is_some_and(|k| k.tray),
        quit_url: k.and_then(|k| k.quit_url),
    })
}

/// `roots` and all their descendants, children before parents (termination order).
pub fn tree(procs: &[Proc], roots: &[u32]) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if p.pid != p.ppid {
            children.entry(p.ppid).or_default().push(p.pid);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    fn visit(pid: u32, children: &HashMap<u32, Vec<u32>>, seen: &mut HashSet<u32>, out: &mut Vec<u32>) {
        if !seen.insert(pid) {
            return;
        }
        for c in children.get(&pid).into_iter().flatten() {
            visit(*c, children, seen, out);
        }
        out.push(pid);
    }
    for r in roots {
        visit(*r, &children, &mut seen, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, exe: &str) -> Proc {
        Proc {
            pid,
            ppid,
            exe: exe.into(),
            ..Proc::default()
        }
    }

    fn titled(pid: u32, ppid: u32, exe: &str, title: &str) -> Proc {
        Proc {
            title: Some(title.into()),
            ..p(pid, ppid, exe)
        }
    }

    /// A desktop: explorer launched Discord, Steam, Spotify, FiveM and Chrome, each with helpers.
    fn desktop() -> Vec<Proc> {
        vec![
            p(4, 0, "System"),
            p(700, 600, "explorer.exe"),
            // Discord: root + renderer/gpu children, the window title is garbage-prone.
            titled(1000, 700, "Discord.exe", "#ogólny | DOJ | Realm Roleplay - Discord"),
            p(1001, 1000, "Discord.exe"),
            p(1002, 1000, "Discord.exe"),
            p(1003, 1001, "Discord.exe"),
            // Steam: steamwebhelper owns the visible window.
            p(2000, 700, "steam.exe"),
            titled(2001, 2000, "steamwebhelper.exe", "Steam"),
            p(2002, 2001, "steamwebhelper.exe"),
            p(2003, 2000, "steamservice.exe"),
            // Spotify
            titled(3000, 700, "Spotify.exe", "Daft Punk - One More Time"),
            p(3001, 3000, "Spotify.exe"),
            // FiveM: launcher + game + browser helpers
            p(4000, 700, "FiveM.exe"),
            titled(4001, 4000, "FiveM_b3095_GTAProcess.exe", "FiveM® by Cfx.re"),
            p(4002, 4000, "FiveM_ChromeBrowser"),
            p(4003, 4000, "FiveM_ROSLauncher"),
            // Chrome: browser + renderers (a YouTube tab title mentions Discord!)
            titled(5000, 700, "chrome.exe", "Discord tips - YouTube - Google Chrome"),
            p(5001, 5000, "chrome.exe"),
            p(5002, 5000, "chrome.exe"),
            // Soul itself
            titled(9000, 700, "soul.exe", "Soul"),
            Proc {
                product: Some("Visual Studio Code".into()),
                ..titled(6000, 700, "Code.exe", "main.rs - project")
            },
            p(6001, 6000, "Code.exe"),
        ]
    }

    fn resolve_ok(q: &str) -> CloseTarget {
        resolve(q, &desktop(), 9000).unwrap_or_else(|e| panic!("{q}: {e}"))
    }

    #[test]
    fn discord_resolves_to_root_by_exe_not_title() {
        for q in ["Discord", "discord", "dyskord", "Discord.exe", "DISCORD"] {
            let t = resolve_ok(q);
            assert_eq!(t.roots, vec![1000], "{q}");
            assert_eq!(t.label, "Discord");
            assert!(t.tray);
            assert_eq!(t.quit_url, None);
        }
    }

    #[test]
    fn steam_resolves_to_steam_exe_never_the_helper() {
        for q in ["Steam", "steama", "steam.exe"] {
            let t = resolve_ok(q);
            assert_eq!(t.roots, vec![2000], "{q}");
            assert_eq!(t.exe, "steam");
            assert_eq!(t.quit_url, Some("steam://exit"));
        }
        // Even naming the helper climbs to the root.
        assert_eq!(resolve_ok("steamwebhelper").roots, vec![2000]);
        // Only helpers running (main died): still lands on the topmost helper.
        let procs: Vec<Proc> = vec![p(700, 1, "explorer.exe"), p(2001, 77, "steamwebhelper.exe"), p(2002, 2001, "steamwebhelper.exe")];
        assert_eq!(resolve("steam", &procs, 1).unwrap().roots, vec![2001]);
    }

    #[test]
    fn spotify_fivem_chrome() {
        let s = resolve_ok("spotify");
        assert_eq!((s.roots.clone(), s.tray), (vec![3000], true));
        assert_eq!(resolve_ok("Spotifaj").roots, vec![3000]);

        let f = resolve_ok("FiveM");
        assert_eq!(f.roots, vec![4000]);
        assert_eq!(f.label, "FiveM");
        assert_eq!(resolve_ok("five m").roots, vec![4000]);
        // The game process climbs to the launcher.
        assert_eq!(resolve_ok("FiveM_b3095_GTAProcess").roots, vec![4000]);

        let c = resolve_ok("Chrome");
        assert_eq!(c.roots, vec![5000]);
        assert!(!c.tray);
        assert_eq!(resolve_ok("google chrome").roots, vec![5000]);
    }

    #[test]
    fn product_name_and_fuzzy_exe() {
        let t = resolve_ok("visual studio");
        assert_eq!(t.roots, vec![6000]);
        assert_eq!(t.label, "VS Code");
    }

    #[test]
    fn title_is_only_a_last_resort() {
        // "realm roleplay" is only in Discord's window title.
        assert_eq!(resolve_ok("realm roleplay").roots, vec![1000]);
        // "youtube" only in Chrome's title.
        assert_eq!(resolve_ok("youtube").roots, vec![5000]);
    }

    #[test]
    fn never_targets_protected_or_soul() {
        assert!(resolve("explorer", &desktop(), 9000).is_err());
        assert!(resolve("soul", &desktop(), 9000).is_err());
        assert!(resolve("system", &desktop(), 9000).is_err());
        let err = resolve("telegram", &desktop(), 9000).unwrap_err();
        assert!(err.contains("Telegram"), "{err}");
        assert!(resolve("nieistniejąca apka", &desktop(), 9000).is_err());
    }

    #[test]
    fn root_never_climbs_into_explorer() {
        // Discord's parent is explorer: root stays Discord.
        let procs = desktop();
        let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
        assert_eq!(root_of(1003, &by_pid, 9000), 1000);
        assert_eq!(root_of(2002, &by_pid, 9000), 2000);
        assert_eq!(root_of(4001, &by_pid, 9000), 4000);
    }

    #[test]
    fn tree_lists_children_before_parents() {
        let procs = desktop();
        let t = tree(&procs, &[1000]);
        assert_eq!(t.len(), 4);
        assert_eq!(*t.last().unwrap(), 1000);
        let pos = |pid| t.iter().position(|x| *x == pid).unwrap();
        assert!(pos(1003) < pos(1001));
        let steam = tree(&procs, &[2000]);
        assert_eq!(steam.len(), 4);
        assert!(!steam.contains(&700));
    }

    #[test]
    fn stems() {
        assert_eq!(stem(r"C:\Program Files\Steam\steam.exe"), "steam");
        assert_eq!(stem("/opt/google/chrome/chrome"), "chrome");
        assert_eq!(stem("Discord.EXE"), "discord");
    }
}
