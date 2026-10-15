use super::apps::{
    collect_lnk_names, fuzzy_score, letter_icon_data_url, pick_best, search_suggestions,
    validate_http_url, windows_start_menu_dirs, AppEntry, AppKind, AppSuggestion,
};
use super::{clipboard, Platform};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use parking_lot::Mutex;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

/// `CREATE_NO_WINDOW`: console programs (powershell, taskkill, reg) get no console window.
/// Soul is a GUI-subsystem app, so without it every spawn flashes a black window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The only way this backend spawns a process: never shows a console window.
#[allow(clippy::disallowed_methods)] // the one sanctioned `Command::new`
pub(crate) fn hidden_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Hidden, non-interactive PowerShell running `script`.
fn powershell(script: &str) -> Command {
    let mut cmd = hidden_command("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    cmd
}

/// Open a file, `.lnk` shortcut, `shell:AppsFolder\…` app or URL like Explorer would
/// (`ShellExecuteW` "open") — no `cmd /c start`, so no console window.
pub(crate) fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // Shell verbs may need COM on the calling thread (Microsoft's ShellExecute guidance);
    // S_FALSE / RPC_E_CHANGED_MODE just mean it is already initialised.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let file = HSTRING::from(target);
    let result = unsafe { ShellExecuteW(None, w!("open"), &file, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // > 32 = success; otherwise an SE_ERR_* / Win32 error code.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute failed (code {code})"))
    }
}

pub struct WindowsPlatform {
    app_cache: Mutex<Option<(Instant, Vec<AppEntry>)>>,
}

impl WindowsPlatform {
    pub fn new() -> Self {
        Self {
            app_cache: Mutex::new(None),
        }
    }

    fn paste_via_ctrl_v() -> Result<(), String> {
        ctrl_chord(Key::Unicode('v'), "Paste")
    }

    fn indexed_apps(&self) -> Vec<AppEntry> {
        const TTL: Duration = Duration::from_secs(10 * 60);
        {
            let guard = self.app_cache.lock();
            if let Some((at, apps)) = guard.as_ref() {
                if at.elapsed() < TTL {
                    return apps.clone();
                }
            }
        }
        let apps = build_windows_app_index();
        *self.app_cache.lock() = Some((Instant::now(), apps.clone()));
        apps
    }

    fn launch_entry(entry: &AppEntry) -> Result<(), String> {
        match entry.kind {
            AppKind::Uwp => {
                let target = if entry.target.starts_with("shell:") {
                    entry.target.clone()
                } else {
                    format!("shell:AppsFolder\\{}", entry.target)
                };
                shell_open(&target).map_err(|e| format!("Failed to launch UWP '{}': {e}", entry.name))?;
            }
            AppKind::Shortcut | AppKind::Desktop => {
                shell_open(&entry.target).map_err(|e| format!("Failed to launch '{}': {e}", entry.name))?;
            }
        }
        Ok(())
    }

    fn find_process_window(query: &str) -> Result<(u32, String), String> {
        let script = r#"
$q = $env:SOUL_QUERY
Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle } |
  ForEach-Object { "{0}`t{1}`t{2}" -f $_.Id, $_.ProcessName, $_.MainWindowTitle }
"#;
        let output = powershell(script)
            .env("SOUL_QUERY", query)
            .output()
            .map_err(|e| format!("Failed to list windows: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Failed to list windows: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut best: Option<(u32, String, i32)> = None;
        for line in stdout.lines() {
            let mut parts = line.splitn(3, '\t');
            let Some(pid_s) = parts.next() else { continue };
            let Some(proc_name) = parts.next() else { continue };
            let title = parts.next().unwrap_or("");
            let Ok(pid) = pid_s.parse::<u32>() else { continue };
            let score = fuzzy_score(query, title)
                .max(fuzzy_score(query, proc_name));
            if score <= 0 {
                continue;
            }
            let label = if title.is_empty() {
                proc_name.to_string()
            } else {
                format!("{title} ({proc_name})")
            };
            match best {
                None => best = Some((pid, label, score)),
                Some((_, _, s)) if score > s => best = Some((pid, label, score)),
                _ => {}
            }
        }
        best.map(|(pid, label, _)| (pid, label))
            .ok_or_else(|| format!("No running window matched '{query}'."))
    }
}

/// Ctrl + `key` through SendInput (enigo).
fn ctrl_chord(key: Key, what: &str) -> Result<(), String> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| format!("Failed to init input simulation: {e}"))?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| format!("{what} failed (Ctrl press): {e}"))?;
    let pressed = enigo.key(key, Direction::Click);
    // Always release Ctrl, even when the key failed.
    let released = enigo.key(Key::Control, Direction::Release);
    pressed.map_err(|e| format!("{what} failed (key): {e}"))?;
    released.map_err(|e| format!("{what} failed (Ctrl release): {e}"))
}

/// Terminals treat Ctrl+C as an interrupt — copy with Ctrl+Insert there instead.
fn foreground_is_console() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};
    let mut buf = [0u16; 128];
    // SAFETY: plain Win32 calls; `buf` outlives the call.
    let len = unsafe { GetClassNameW(GetForegroundWindow(), &mut buf) };
    let class = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
    matches!(
        class.as_str(),
        "ConsoleWindowClass" | "CASCADIA_HOSTING_WINDOW_CLASS" | "mintty" | "VirtualConsoleClass"
    )
}

/// Audio sessions Soul ducked: (session instance id, original level).
static DUCKED: Mutex<Vec<(String, f32)>> = Mutex::new(Vec::new());

/// Scale (or restore) every other process's WASAPI session volume on the default output.
pub(super) fn lower_current_thread_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: pseudo-handle of the calling thread; no memory is passed.
    if let Err(e) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) } {
        eprintln!("[soul][platform] lower thread priority failed: {e}");
    }
}

fn duck_sessions(factor: Option<f32>) -> Result<(), String> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    let own = std::process::id();
    let mut saved = DUCKED.lock();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("Audio devices unavailable: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| format!("No default output: {e}"))?;
        let manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Audio sessions unavailable: {e}"))?;
        let sessions = manager
            .GetSessionEnumerator()
            .map_err(|e| format!("Audio sessions unavailable: {e}"))?;
        let count = sessions.GetCount().unwrap_or(0);
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else { continue };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
            let pid = control2.GetProcessId().unwrap_or(0);
            // pid 0 = system sounds; never duck Soul's own reply.
            if pid == 0 || pid == own {
                continue;
            }
            let Ok(id_ptr) = control2.GetSessionInstanceIdentifier() else { continue };
            let id = id_ptr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id_ptr.0 as *const _));
            let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
            match factor {
                Some(f) => {
                    if saved.iter().any(|(s, _)| *s == id) {
                        continue;
                    }
                    let Ok(level) = volume.GetMasterVolume() else { continue };
                    if volume.SetMasterVolume(level * f, std::ptr::null()).is_ok() {
                        saved.push((id, level));
                    }
                }
                None => {
                    if let Some(pos) = saved.iter().position(|(s, _)| *s == id) {
                        let (_, level) = saved.remove(pos);
                        let _ = volume.SetMasterVolume(level, std::ptr::null());
                    }
                }
            }
        }
    }
    if factor.is_none() {
        saved.clear();
    }
    Ok(())
}

/// UI Automation: focused element is an Edit, or exposes a writable Value pattern.
fn focused_is_text_input() -> Option<bool> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_DocumentControlTypeId,
        UIA_EditControlTypeId, UIA_ValuePatternId,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let el = uia.GetFocusedElement().ok()?;
        let kind = el.CurrentControlType().ok()?;
        let read_only = el
            .GetCurrentPattern(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.cast::<IUIAutomationValuePattern>().ok())
            .and_then(|v| v.CurrentIsReadOnly().ok())
            .map(|b| b.as_bool());
        Some(match read_only {
            Some(ro) => !ro && (kind == UIA_EditControlTypeId || kind == UIA_DocumentControlTypeId),
            None => kind == UIA_EditControlTypeId,
        })
    }
}

impl Platform for WindowsPlatform {
    fn text_input_focused(&self) -> Option<bool> {
        // UIA can stall on a hung target app — never wait for it longer than the budget.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(focused_is_text_input());
        });
        rx.recv_timeout(std::time::Duration::from_millis(150)).ok().flatten()
    }

    fn duck_other_audio(&self, factor: Option<f32>) -> Result<(), String> {
        duck_sessions(factor.map(|f| f.clamp(0.0, 1.0)))
    }

    fn set_do_not_disturb(&self, on: bool) -> Result<String, String> {
        // Toast banners on/off for the current user (there is no public Focus Assist API).
        let status = hidden_command("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\PushNotifications",
                "/v",
                "ToastEnabled",
                "/t",
                "REG_DWORD",
                "/d",
                if on { "0" } else { "1" },
                "/f",
            ])
            .status()
            .map_err(|e| format!("Do Not Disturb: {e}"))?;
        if status.success() {
            Ok(if on {
                "Powiadomienia wyciszone (Nie przeszkadzać)".into()
            } else {
                "Powiadomienia włączone".into()
            })
        } else {
            Err("Nie udało się przełączyć powiadomień Windows.".into())
        }
    }

    fn power_state(&self) -> super::PowerState {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        use windows::Win32::UI::Shell::{
            SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
            QUNS_RUNNING_D3D_FULL_SCREEN,
        };
        let mut status = SYSTEM_POWER_STATUS::default();
        // ACLineStatus: 0 = offline (battery), 1 = online, 255 = unknown.
        let on_battery = unsafe { GetSystemPowerStatus(&mut status) }
            .ok()
            .and(match status.ACLineStatus {
                0 => Some(true),
                1 => Some(false),
                _ => None,
            });
        let fullscreen = unsafe { SHQueryUserNotificationState() }.ok().map(|s| {
            s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE
        });
        super::PowerState {
            on_battery,
            fullscreen,
        }
    }

    fn type_text(&self, text: &str) -> Result<(), String> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| format!("Failed to init input simulation: {e}"))?;
        enigo
            .text(text)
            .map_err(|e| format!("Failed to type text: {e}"))
    }

    fn paste_text(&self, text: &str) -> Result<(), String> {
        clipboard::with_temporary_text(text, Self::paste_via_ctrl_v)
    }

    fn read_selection(&self) -> Result<Option<String>, String> {
        use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
        let mut cb =
            arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
        let saved = clipboard::save(&mut cb);
        // SAFETY: no arguments; reads a counter.
        let before = unsafe { GetClipboardSequenceNumber() };
        let key = if foreground_is_console() { Key::Insert } else { Key::Unicode('c') };
        ctrl_chord(key, "Copy")?;
        // An app with nothing selected leaves the clipboard alone.
        let t0 = Instant::now();
        let mut changed = false;
        while t0.elapsed() < Duration::from_millis(600) {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: as above.
            if unsafe { GetClipboardSequenceNumber() } != before {
                changed = true;
                break;
            }
        }
        if !changed {
            return Ok(None);
        }
        // The app may still be adding formats; the clipboard can be briefly locked.
        std::thread::sleep(Duration::from_millis(40));
        let mut text = None;
        for _ in 0..5 {
            match cb.get_text() {
                Ok(t) => {
                    text = Some(t);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(30)),
            }
        }
        clipboard::restore(&mut cb, saved);
        Ok(text.filter(|t| !t.trim().is_empty()))
    }

    fn active_window(&self) -> Option<u64> {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        // SAFETY: no arguments.
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.0.is_null()).then_some(hwnd.0 as usize as u64)
    }

    fn activate_window(&self, id: u64) -> Result<(), String> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};
        let hwnd = HWND(id as usize as *mut core::ffi::c_void);
        // SAFETY: the handle is only passed to user32, which validates it.
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return Err("The window is gone.".into());
            }
            if SetForegroundWindow(hwnd).as_bool() {
                Ok(())
            } else {
                Err("Could not bring the window back to the front.".into())
            }
        }
    }

    fn send_undo(&self) -> Result<(), String> {
        ctrl_chord(Key::Unicode('z'), "Undo")
    }

    fn get_volume(&self) -> Result<u8, String> {
        use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
        use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
        // SAFETY: COM calls on this worker thread; interfaces are released on drop.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("Audio devices unavailable: {e}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .map_err(|e| format!("No default output: {e}"))?;
            let volume: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("Volume unavailable: {e}"))?;
            let level = volume
                .GetMasterVolumeLevelScalar()
                .map_err(|e| format!("Volume unavailable: {e}"))?;
            Ok((level * 100.0).round().clamp(0.0, 100.0) as u8)
        }
    }

    fn capture_screen(&self) -> Result<Vec<u8>, String> {
        Ok(crate::capture::capture_monitor_under_cursor()?.png)
    }

    fn open_app(&self, name: &str) -> Result<String, String> {
        let apps = self.indexed_apps();
        let entry = pick_best(name, &apps)
            .ok_or_else(|| format!("No installed app matched '{name}'."))?;
        Self::launch_entry(entry)?;
        Ok(format!("Opened {}", entry.name))
    }

    fn search_apps(&self, query: &str, limit: usize) -> Vec<AppSuggestion> {
        let apps = self.indexed_apps();
        let mut suggestions = search_suggestions(query, &apps, limit);
        // Exact pinned-style lookup: if query matches one app name, ensure icon.
        if suggestions.is_empty() {
            if let Some(entry) = pick_best(query, &apps) {
                suggestions.push(AppSuggestion {
                    name: entry.name.clone(),
                    icon_data_url: letter_icon_data_url(&entry.name),
                });
            }
        }
        suggestions
    }

    fn close_app(&self, name: &str) -> Result<String, String> {
        let (pid, label) = Self::find_process_window(name)?;
        let status = hidden_command("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .status()
            .map_err(|e| format!("Failed to close '{label}': {e}"))?;
        if status.success() {
            Ok(format!("Closed {label}"))
        } else {
            Err(format!("Failed to close '{label}'."))
        }
    }

    fn focus_app(&self, name: &str) -> Result<String, String> {
        let (pid, label) = Self::find_process_window(name)?;
        let script = format!(
            r#"
$p = Get-Process -Id {pid} -ErrorAction Stop
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SoulWin {{
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);
}}
"@
[void][SoulWin]::ShowWindowAsync($p.MainWindowHandle, 9)
[void][SoulWin]::SetForegroundWindow($p.MainWindowHandle)
"#
        );
        let status = powershell(&script)
            .status()
            .map_err(|e| format!("Failed to focus '{label}': {e}"))?;
        if status.success() {
            Ok(format!("Focused {label}"))
        } else {
            Err(format!("Failed to focus '{label}'."))
        }
    }

    fn open_url(&self, url: &str) -> Result<String, String> {
        let url = validate_http_url(url)?;
        shell_open(&url).map_err(|e| format!("Failed to open URL: {e}"))?;
        Ok(format!("Opened {url}"))
    }

    fn set_volume(&self, percent: u8) -> Result<String, String> {
        let percent = percent.min(100);
        let level = percent as f32 / 100.0;
        let script = format!(
            r#"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioEndpointVolume {{
  int f(); int g(); int h(); int i();
  int SetMasterVolumeLevelScalar(float fLevel, Guid pguidEventContext);
  int j();
  int GetMasterVolumeLevelScalar(out float pfLevel);
}}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDevice {{
  int Activate(ref Guid id, int clsCtx, IntPtr p, [MarshalAs(UnmanagedType.IUnknown)] out object pp);
}}
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDeviceEnumerator {{
  int f();
  int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice pp);
}}
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumeratorComObject {{ }}
public class SoulAudio {{
  public static void SetVolume(float level) {{
    var enumerator = (IMMDeviceEnumerator)(new MMDeviceEnumeratorComObject());
    IMMDevice device;
    Marshal.ThrowExceptionForHR(enumerator.GetDefaultAudioEndpoint(0, 1, out device));
    Guid iid = typeof(IAudioEndpointVolume).GUID;
    object o;
    Marshal.ThrowExceptionForHR(device.Activate(ref iid, 23, IntPtr.Zero, out o));
    var vol = (IAudioEndpointVolume)o;
    Marshal.ThrowExceptionForHR(vol.SetMasterVolumeLevelScalar(level, Guid.Empty));
  }}
}}
"@
[SoulAudio]::SetVolume({level})
"#
        );
        let output = powershell(&script)
            .output()
            .map_err(|e| format!("Failed to set volume: {e}"))?;
        if output.status.success() {
            Ok(format!("Volume set to {percent}%"))
        } else {
            Err(format!(
                "Failed to set volume: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn media_control(&self, action: &str) -> Result<String, String> {
        // VK_MEDIA_PLAY_PAUSE=0xB3, VK_MEDIA_NEXT_TRACK=0xB0, VK_MEDIA_PREV_TRACK=0xB1
        let (vk, label) = match action.trim().to_ascii_lowercase().as_str() {
            "play" | "pause" | "play_pause" | "playpause" => (0xB3u32, "Play/Pause"),
            "next" => (0xB0, "Next track"),
            "prev" | "previous" => (0xB1, "Previous track"),
            other => {
                return Err(format!(
                    "Unknown media action '{other}'. Use play, pause, next, or prev."
                ));
            }
        };
        let script = format!(
            r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SoulMedia {{
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
}}
"@
[SoulMedia]::keybd_event({vk}, 0, 0, [UIntPtr]::Zero)
[SoulMedia]::keybd_event({vk}, 0, 2, [UIntPtr]::Zero)
"#
        );
        let status = powershell(&script)
            .status()
            .map_err(|e| format!("Media key failed: {e}"))?;
        if status.success() {
            Ok(label.into())
        } else {
            Err("Media key simulation failed.".into())
        }
    }

    fn install_input_hook(&self, sink: super::hook::HookSink) -> Result<(), String> {
        super::windows_hook::install(sink)
    }

    fn configure_input_hook(&self, config: super::hook::HookConfig) {
        super::windows_hook::configure(config);
    }
}

fn build_windows_app_index() -> Vec<AppEntry> {
    let mut apps = Vec::new();
    for dir in windows_start_menu_dirs() {
        collect_lnk_names(&dir, &mut apps);
    }

    // UWP / Store apps via Get-StartApps
    let output = powershell("Get-StartApps | ForEach-Object { \"$($_.Name)`t$($_.AppID)\" }")
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let mut parts = line.splitn(2, '\t');
                let Some(name) = parts.next() else { continue };
                let Some(app_id) = parts.next() else { continue };
                let name = name.trim();
                let app_id = app_id.trim();
                if name.is_empty() || app_id.is_empty() {
                    continue;
                }
                // Skip duplicates already covered by .lnk with same name.
                if apps.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
                    continue;
                }
                apps.push(AppEntry {
                    name: name.to_string(),
                    target: app_id.to_string(),
                    kind: AppKind::Uwp,
                });
            }
        }
    }

    eprintln!("[soul][platform] indexed {} Windows apps", apps.len());
    apps
}

/// Opt-in check that Soul's spawns never show a console window (the "włącz Spotify" flash).
///   cargo test --manifest-path src-tauri/Cargo.toml --release --lib console_probe -- --ignored --nocapture --test-threads=1
/// The test detaches from its console first (`FreeConsole`), like the GUI-subsystem release app —
/// otherwise children would silently share the test's console and nothing could flash. Side
/// effects: opens + closes Character Map and Calculator, opens one https://example.com tab and sends
/// play/pause twice.
#[cfg(test)]
mod console_probe {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, IsWindowVisible};

    const CONSOLE_CLASSES: [&str; 2] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"];

    fn visible_consoles() -> HashSet<isize> {
        unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
            let out = &mut *(lp.0 as *mut HashSet<isize>);
            let mut buf = [0u16; 64];
            let n = GetClassNameW(hwnd, &mut buf) as usize;
            let class = String::from_utf16_lossy(&buf[..n]);
            if IsWindowVisible(hwnd).as_bool() && CONSOLE_CLASSES.contains(&class.as_str()) {
                out.insert(hwnd.0 as isize);
            }
            true.into()
        }
        let mut set = HashSet::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut set as *mut _ as isize));
        }
        set
    }

    /// Run `f` while polling every 10 ms for console windows that weren't there before.
    fn new_consoles_during(label: &str, settle_ms: u64, f: impl FnOnce()) -> usize {
        let baseline = visible_consoles();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let watcher = {
            let (stop, seen, baseline) = (stop.clone(), seen.clone(), baseline.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    for h in visible_consoles().difference(&baseline) {
                        seen.lock().insert(*h);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        f();
        std::thread::sleep(Duration::from_millis(settle_ms));
        stop.store(true, Ordering::SeqCst);
        watcher.join().unwrap();
        let n = seen.lock().len();
        eprintln!("[console_probe] {label:<40} new console windows: {n}");
        n
    }

    #[test]
    #[ignore]
    fn no_console_windows() {
        unsafe {
            let _ = windows::Win32::System::Console::FreeConsole();
        }
        let p = WindowsPlatform::new();

        // Negative control: a bare spawn of a console program MUST be detected, or this probe proves nothing.
        #[allow(clippy::disallowed_methods)]
        let control = new_consoles_during("control: bare `cmd /c ping` (old bug)", 300, || {
            let _ = Command::new("cmd").args(["/C", "ping -n 3 127.0.0.1 >NUL"]).spawn();
        });
        assert!(control > 0, "detector failed: the unflagged control spawn showed no console window");
        std::thread::sleep(Duration::from_millis(2500)); // let the control window close

        let mut total = 0;
        total += new_consoles_during("app index (Get-StartApps powershell)", 300, || {
            let _ = build_windows_app_index();
        });
        total += new_consoles_during("open_app .lnk/exe path (Character Map)", 1500, || {
            WindowsPlatform::launch_entry(&AppEntry {
                name: "Character Map".into(),
                target: r"C:\Windows\System32\charmap.exe".into(),
                kind: AppKind::Shortcut,
            })
            .unwrap();
        });
        total += new_consoles_during("focus_app (powershell + Add-Type)", 300, || {
            let _ = p.focus_app("charmap");
        });
        total += new_consoles_during("close_app (powershell + taskkill)", 500, || {
            p.close_app("charmap").unwrap();
        });
        total += new_consoles_during("open_app UWP path (Calculator)", 2000, || {
            WindowsPlatform::launch_entry(&AppEntry {
                name: "Calculator".into(),
                target: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
                kind: AppKind::Uwp,
            })
            .unwrap();
        });
        total += new_consoles_during("close Calculator (hidden taskkill)", 500, || {
            let _ = hidden_command("taskkill").args(["/IM", "CalculatorApp.exe", "/F"]).status();
        });
        total += new_consoles_during("open_url / youtube / web_search", 800, || {
            p.open_url("https://example.com").unwrap();
        });
        total += new_consoles_during("media_control play/pause ×2", 300, || {
            p.media_control("play_pause").unwrap();
            std::thread::sleep(Duration::from_millis(600));
            p.media_control("play_pause").unwrap();
        });
        total += new_consoles_during("set_volume path (powershell)", 300, || {
            assert!(powershell("exit 0").status().unwrap().success());
        });
        total += new_consoles_during("DND path (hidden reg query)", 300, || {
            let _ = hidden_command("reg").args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\PushNotifications"]).output();
        });
        assert_eq!(total, 0, "console windows appeared");
    }
}
