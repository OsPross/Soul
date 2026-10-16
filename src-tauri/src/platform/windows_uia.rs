//! Windows backend of the computer agent: UI Automation perception + pattern actions, SendInput
//! mouse/keyboard (Unicode typing), monitors, kill-switch probe, capture exclusion.
//!
//! All UIA work runs on one dedicated worker thread (MTA). COM interface pointers never leave it:
//! the worker keeps the last snapshot's `IUIAutomationElement`s, so `click(id)` acts on exactly the
//! element the model saw. Callers wait with a timeout; a worker stuck in a hung app is abandoned
//! and a new one starts on the next request. UIA's own connection/transaction timeouts bound every
//! cross-process call as well.

use super::coords::{self, Monitor, Point};
use super::input::{InputStep, Key, MouseButton};
use super::ui::{
    self, Caps, ElementOp, QuickState, Rect, Role, ScrollDir, SnapshotOptions, Source, UiElement, UiSnapshot,
    WindowInfo,
};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use windows::core::{Interface, BSTR, VARIANT};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::UI::Accessibility::*;

/// Whole snapshot (tree walk) budget.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(4);
/// One pattern operation / quick state read.
const OP_TIMEOUT: Duration = Duration::from_millis(3500);
/// UIA cross-process call limits (IUIAutomation2).
const UIA_CONNECTION_TIMEOUT_MS: u32 = 2000;
const UIA_TRANSACTION_TIMEOUT_MS: u32 = 2500;

static GEN: AtomicU64 = AtomicU64::new(0);

/// Run the calling thread per-monitor-DPI-aware (physical pixels everywhere). Idempotent.
pub(super) fn ensure_dpi_aware_thread() {
    use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    // SAFETY: plain call; returns the previous context (ignored).
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

// ───────────────────────────── worker ─────────────────────────────

type Job = Box<dyn FnOnce(&mut Ctx) + Send>;

struct Ctx {
    uia: Option<IUIAutomation>,
    cache: Option<IUIAutomationCacheRequest>,
    condition: Option<IUIAutomationCondition>,
    gen: u64,
    /// Raw elements of the last snapshot (backend order).
    elements: Vec<IUIAutomationElement>,
    /// id - 1 → index into `elements`.
    ids: Vec<usize>,
}

static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

fn worker() -> Sender<Job> {
    let mut slot = WORKER.lock();
    if let Some(tx) = slot.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new().name("soul-uia".into()).spawn(move || {
        ensure_dpi_aware_thread();
        // SAFETY: COM init for this thread; balanced implicitly at thread exit.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut ctx = Ctx {
            uia: None,
            cache: None,
            condition: None,
            gen: 0,
            elements: Vec::new(),
            ids: Vec::new(),
        };
        while let Ok(job) = rx.recv() {
            job(&mut ctx);
        }
    });
    if let Err(e) = spawned {
        eprintln!("[soul][platform] uia worker spawn failed: {e}");
    }
    *slot = Some(tx.clone());
    tx
}

/// Run `f` on the UIA worker, waiting at most `timeout`.
fn run<T: Send + 'static>(
    timeout: Duration,
    what: &'static str,
    f: impl FnOnce(&mut Ctx) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (rtx, rrx) = mpsc::channel();
    let job: Job = Box::new(move |ctx| {
        let _ = rtx.send(f(ctx));
    });
    if worker().send(job).is_err() {
        *WORKER.lock() = None;
        return Err("UI Automation worker is gone.".into());
    }
    match rrx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => {
            // The app under automation hangs: abandon this worker (it exits after its job).
            eprintln!("[soul][platform] uia {what} timed out after {} ms — new worker", timeout.as_millis());
            *WORKER.lock() = None;
            Err(format!("UI Automation timed out ({what}) — the app is not responding."))
        }
        Err(RecvTimeoutError::Disconnected) => {
            *WORKER.lock() = None;
            Err("UI Automation worker stopped.".into())
        }
    }
}

impl Ctx {
    fn uia(&mut self) -> Result<IUIAutomation, String> {
        if let Some(u) = &self.uia {
            return Ok(u.clone());
        }
        // SAFETY: COM calls on the worker thread (MTA initialised in `worker`).
        unsafe {
            let uia: IUIAutomation = windows::Win32::System::Com::CoCreateInstance(
                &CUIAutomation8,
                None,
                windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
            )
            .or_else(|_| {
                windows::Win32::System::Com::CoCreateInstance(
                    &CUIAutomation,
                    None,
                    windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                )
            })
            .map_err(|e| format!("UI Automation unavailable: {e}"))?;
            if let Ok(u2) = uia.cast::<IUIAutomation2>() {
                let _ = u2.SetConnectionTimeout(UIA_CONNECTION_TIMEOUT_MS);
                let _ = u2.SetTransactionTimeout(UIA_TRANSACTION_TIMEOUT_MS);
            }
            self.uia = Some(uia.clone());
            Ok(uia)
        }
    }

    fn cache(&mut self) -> Result<IUIAutomationCacheRequest, String> {
        if let Some(c) = &self.cache {
            return Ok(c.clone());
        }
        let uia = self.uia()?;
        // SAFETY: COM calls on the worker thread.
        unsafe {
            let c = uia.CreateCacheRequest().map_err(|e| e.message())?;
            for p in CACHED_PROPERTIES {
                c.AddProperty(*p).map_err(|e| e.message())?;
            }
            c.SetTreeScope(TreeScope_Element).map_err(|e| e.message())?;
            c.SetAutomationElementMode(AutomationElementMode_Full).map_err(|e| e.message())?;
            self.cache = Some(c.clone());
            Ok(c)
        }
    }

    /// (control view) ∧ ¬offscreen ∧ (interactive control type ∨ invokable ∨ scrollable).
    fn condition(&mut self) -> Result<IUIAutomationCondition, String> {
        if let Some(c) = &self.condition {
            return Ok(c.clone());
        }
        let uia = self.uia()?;
        // SAFETY: COM calls on the worker thread; VARIANTs outlive the calls.
        unsafe {
            let mut any: Option<IUIAutomationCondition> = None;
            let mut or = |c: IUIAutomationCondition| -> Result<(), String> {
                any = Some(match any.take() {
                    None => c,
                    Some(prev) => uia.CreateOrCondition(&prev, &c).map_err(|e| e.message())?,
                });
                Ok(())
            };
            for t in INCLUDED_TYPES {
                or(uia
                    .CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(t.0))
                    .map_err(|e| e.message())?)?;
            }
            for p in [UIA_IsInvokePatternAvailablePropertyId, UIA_IsScrollPatternAvailablePropertyId] {
                or(uia.CreatePropertyCondition(p, &VARIANT::from(true)).map_err(|e| e.message())?)?;
            }
            let wanted = any.ok_or("no condition")?;
            let onscreen = uia
                .CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false))
                .map_err(|e| e.message())?;
            let view = uia.ControlViewCondition().map_err(|e| e.message())?;
            let c = uia
                .CreateAndCondition(&uia.CreateAndCondition(&view, &onscreen).map_err(|e| e.message())?, &wanted)
                .map_err(|e| e.message())?;
            self.condition = Some(c.clone());
            Ok(c)
        }
    }

    fn element(&self, gen: u64, id: u32) -> Result<IUIAutomationElement, String> {
        if gen != self.gen {
            return Err(ui::STALE.into());
        }
        let idx = id
            .checked_sub(1)
            .and_then(|i| self.ids.get(i as usize))
            .ok_or_else(|| format!("No element {id} in the current view."))?;
        self.elements.get(*idx).cloned().ok_or_else(|| ui::STALE.into())
    }
}


const CACHED_PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_AutomationIdPropertyId,
    UIA_IsPasswordPropertyId,
    UIA_FrameworkIdPropertyId,
    UIA_ValueValuePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsScrollPatternAvailablePropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_ScrollVerticalScrollPercentPropertyId,
    UIA_RangeValueValuePropertyId,
];

const INCLUDED_TYPES: &[UIA_CONTROLTYPE_ID] = &[
    UIA_ButtonControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId,
    UIA_EditControlTypeId,
    UIA_HyperlinkControlTypeId,
    UIA_ListItemControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_SliderControlTypeId,
    UIA_SpinnerControlTypeId,
    UIA_TabItemControlTypeId,
    UIA_TreeItemControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_DataItemControlTypeId,
    UIA_HeaderItemControlTypeId,
    UIA_DocumentControlTypeId,
    UIA_TextControlTypeId,
];

fn role_of(t: UIA_CONTROLTYPE_ID) -> Role {
    match t {
        x if x == UIA_ButtonControlTypeId || x == UIA_SplitButtonControlTypeId => Role::Button,
        x if x == UIA_EditControlTypeId => Role::Edit,
        x if x == UIA_DocumentControlTypeId => Role::Document,
        x if x == UIA_HyperlinkControlTypeId => Role::Link,
        x if x == UIA_CheckBoxControlTypeId => Role::CheckBox,
        x if x == UIA_RadioButtonControlTypeId => Role::Radio,
        x if x == UIA_ComboBoxControlTypeId => Role::ComboBox,
        x if x == UIA_ListItemControlTypeId => Role::ListItem,
        x if x == UIA_MenuItemControlTypeId => Role::MenuItem,
        x if x == UIA_TabItemControlTypeId => Role::TabItem,
        x if x == UIA_TreeItemControlTypeId => Role::TreeItem,
        x if x == UIA_SliderControlTypeId => Role::Slider,
        x if x == UIA_SpinnerControlTypeId => Role::Spinner,
        x if x == UIA_DataItemControlTypeId => Role::DataItem,
        x if x == UIA_HeaderItemControlTypeId || x == UIA_HeaderControlTypeId => Role::Header,
        x if x == UIA_ListControlTypeId || x == UIA_TreeControlTypeId || x == UIA_DataGridControlTypeId => Role::List,
        x if x == UIA_TextControlTypeId => Role::Text,
        x if x == UIA_PaneControlTypeId || x == UIA_WindowControlTypeId || x == UIA_GroupControlTypeId => Role::Pane,
        _ => Role::Other,
    }
}

// ───────────────────────────── VARIANT helpers ─────────────────────────────

fn v_bool(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<bool> {
    // SAFETY: cached property read on the worker thread.
    let v = unsafe { el.GetCachedPropertyValue(p) }.ok()?;
    bool::try_from(&v).ok()
}

fn v_i32(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<i32> {
    // SAFETY: as above.
    let v = unsafe { el.GetCachedPropertyValue(p) }.ok()?;
    i32::try_from(&v).ok()
}

fn v_f64(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<f64> {
    // SAFETY: as above.
    let v = unsafe { el.GetCachedPropertyValue(p) }.ok()?;
    f64::try_from(&v).ok()
}

fn v_string(el: &IUIAutomationElement, p: UIA_PROPERTY_ID) -> Option<String> {
    // SAFETY: as above.
    let v = unsafe { el.GetCachedPropertyValue(p) }.ok()?;
    BSTR::try_from(&v).ok().map(|b| b.to_string())
}

fn bstr(r: windows::core::Result<BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

fn rect_of(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// Raw element from the cache (ids assigned later by `ui::finalize`).
fn read_element(el: &IUIAutomationElement) -> Option<UiElement> {
    // SAFETY: cached property reads on the worker thread.
    unsafe {
        let kind = el.CachedControlType().ok()?;
        let rect = rect_of(el.CachedBoundingRectangle().ok()?);
        let mut e = UiElement::new(role_of(kind), bstr(el.CachedName()), rect);
        e.enabled = el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true);
        e.focused = el.CachedHasKeyboardFocus().map(|b| b.as_bool()).unwrap_or(false);
        e.automation_id = bstr(el.CachedAutomationId());
        e.password = el.CachedIsPassword().map(|b| b.as_bool()).unwrap_or(false);
        let framework = bstr(el.CachedFrameworkId());
        e.web = framework.eq_ignore_ascii_case("Chrome");
        let has = |p| v_bool(el, p).unwrap_or(false);
        let value_pattern = has(UIA_IsValuePatternAvailablePropertyId);
        e.caps = Caps {
            invoke: has(UIA_IsInvokePatternAvailablePropertyId),
            set_value: value_pattern && !v_bool(el, UIA_ValueIsReadOnlyPropertyId).unwrap_or(true),
            toggle: has(UIA_IsTogglePatternAvailablePropertyId),
            select: has(UIA_IsSelectionItemPatternAvailablePropertyId),
            expand: has(UIA_IsExpandCollapsePatternAvailablePropertyId),
            scroll: has(UIA_IsScrollPatternAvailablePropertyId),
        };
        if !e.password {
            if value_pattern {
                e.value = v_string(el, UIA_ValueValuePropertyId);
            } else if e.role == Role::Slider || e.role == Role::Spinner {
                e.value = v_f64(el, UIA_RangeValueValuePropertyId).map(|v| format!("{v}"));
            }
        }
        if e.caps.toggle {
            e.checked = v_i32(el, UIA_ToggleToggleStatePropertyId).map(|s| s == ToggleState_On.0);
        }
        if e.caps.expand {
            e.expanded = v_i32(el, UIA_ExpandCollapseExpandCollapseStatePropertyId).and_then(|s| match s {
                s if s == ExpandCollapseState_Expanded.0 || s == ExpandCollapseState_PartiallyExpanded.0 => Some(true),
                s if s == ExpandCollapseState_Collapsed.0 => Some(false),
                _ => None, // leaf
            });
        }
        if e.caps.select {
            e.selected = v_bool(el, UIA_SelectionItemIsSelectedPropertyId);
        }
        if e.caps.scroll {
            e.scroll_pct = v_f64(el, UIA_ScrollVerticalScrollPercentPropertyId).filter(|p| *p >= 0.0);
        }
        Some(e)
    }
}

// ───────────────────────────── windows ─────────────────────────────

fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut core::ffi::c_void)
}

fn window_title(h: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut buf = [0u16; 512];
    // SAFETY: buffer + length passed together.
    let n = unsafe { GetWindowTextW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn window_class(h: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 128];
    // SAFETY: as above.
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn window_pid(h: HWND) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    // SAFETY: out pointer to a local.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Visible frame (DWM extended frame bounds: no invisible resize borders).
fn window_rect(h: HWND) -> Option<Rect> {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut r = RECT::default();
    // SAFETY: out struct of the documented size.
    let ok = unsafe {
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32)
    }
    .is_ok()
        || unsafe { GetWindowRect(h, &mut r) }.is_ok();
    let rect = rect_of(r);
    (ok && !rect.is_empty()).then_some(rect)
}

fn is_cloaked(h: HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    let mut cloaked = 0u32;
    // SAFETY: out u32.
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4) }.is_ok() && cloaked != 0
}

/// A real app window: visible, not minimized, not cloaked, titled, not the desktop / taskbar.
fn is_app_window(h: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongW, IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
    // SAFETY: user32 validates the handle.
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() || is_cloaked(h) {
            return false;
        }
        if GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
    }
    let class = window_class(h);
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return false;
    }
    !window_title(h).trim().is_empty() && window_rect(h).is_some()
}

/// Top-level windows in z-order (topmost first).
fn z_order() -> Vec<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<HWND>);
        out.push(h);
        true.into()
    }
    let mut out: Vec<HWND> = Vec::new();
    // SAFETY: `out` outlives EnumWindows, which calls `cb` synchronously.
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The window the agent works on: the foreground window, or — when one of Soul's own windows
/// has focus (Island click) — the topmost real app window below it.
fn target_window(skip_own: bool) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let own = std::process::id();
    // SAFETY: no arguments.
    let fg = unsafe { GetForegroundWindow() };
    if !fg.0.is_null() && (!skip_own || window_pid(fg) != own) && window_rect(fg).is_some() {
        return Some(fg);
    }
    z_order().into_iter().find(|h| window_pid(*h) != own && is_app_window(*h))
}

fn monitor_rect_of(h: HWND) -> Rect {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: sized out struct.
    unsafe {
        let m = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(m, &mut mi);
    }
    rect_of(mi.rcMonitor)
}

fn window_info(h: HWND) -> WindowInfo {
    let rect = window_rect(h).unwrap_or_default();
    let monitor = monitor_rect_of(h);
    let pid = window_pid(h);
    let process = super::windows::process_path(pid)
        .map(|p| p.rsplit(['\\', '/']).next().unwrap_or_default().to_string())
        .unwrap_or_default();
    WindowInfo {
        id: h.0 as usize as u64,
        title: window_title(h),
        process,
        pid,
        rect,
        monitor,
        fullscreen: covers_monitor(&rect, &monitor) && !has_caption(h),
    }
}

fn covers_monitor(rect: &Rect, monitor: &Rect) -> bool {
    !monitor.is_empty() && rect.x <= monitor.x && rect.y <= monitor.y && rect.right() >= monitor.right() && rect.bottom() >= monitor.bottom()
}

/// A maximized app window on a monitor without a taskbar also covers the whole monitor; games
/// and F11 / presentation fullscreen have no caption.
fn has_caption(h: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongW, GWL_STYLE, WS_CAPTION};
    // SAFETY: user32 validates the handle.
    let style = unsafe { GetWindowLongW(h, GWL_STYLE) } as u32;
    style & WS_CAPTION.0 == WS_CAPTION.0
}

/// Harness: the top-level window whose title contains `needle`.
#[cfg(test)]
pub(super) fn find_window(needle: &str) -> Option<u64> {
    let needle = needle.to_lowercase();
    z_order().into_iter().find(|h| is_app_window(*h) && window_title(*h).to_lowercase().contains(&needle)).map(|h| h.0 as usize as u64)
}

/// Popups of the same process shown over the window (menus, combo drop-downs, flyouts).
fn popups_of(main: HWND, pid: u32) -> Vec<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongW, IsWindowVisible, GWL_STYLE, WS_POPUP};
    z_order()
        .into_iter()
        .filter(|h| *h != main && window_pid(*h) == pid)
        .filter(|h| {
            // SAFETY: user32 validates the handle.
            let (visible, style) = unsafe { (IsWindowVisible(*h).as_bool(), GetWindowLongW(*h, GWL_STYLE) as u32) };
            let class = window_class(*h);
            visible
                && !is_cloaked(*h)
                && (class == "#32768" || class == "ComboLBox" || style & WS_POPUP.0 != 0)
                && window_rect(*h).is_some_and(|r| r.w < 4000 && r.h < 4000)
        })
        .take(3)
        .collect()
}

// ───────────────────────────── snapshot ─────────────────────────────

pub(super) fn snapshot(opts: SnapshotOptions) -> Result<UiSnapshot, String> {
    let first = snapshot_once(opts)?;
    // Chromium / Electron build the page's accessibility tree only after the first UIA client
    // asks: a browser window without its page document gets one more look.
    let chromium = target_window(opts.skip_own_windows).is_some_and(|h| window_class(h).starts_with("Chrome_WidgetWin"));
    if chromium && !first.elements.iter().any(|e| e.role == Role::Document) {
        std::thread::sleep(Duration::from_millis(450));
        return snapshot_once(opts);
    }
    Ok(first)
}

fn snapshot_once(opts: SnapshotOptions) -> Result<UiSnapshot, String> {
    let t0 = Instant::now();
    let target = target_window(opts.skip_own_windows).ok_or("No app window is in focus.")?;
    let info = window_info(target);
    let popups = popups_of(target, info.pid);
    let main_id = info.id;
    let popup_ids: Vec<u64> = popups.iter().map(|h| h.0 as usize as u64).collect();
    let mut filter = info.rect;
    for p in &popups {
        if let Some(r) = window_rect(*p) {
            // Union of the window and its popups (a menu may hang outside the frame).
            let l = filter.x.min(r.x);
            let t = filter.y.min(r.y);
            let rr = filter.right().max(r.right());
            let b = filter.bottom().max(r.bottom());
            filter = Rect::new(l, t, rr - l, b - t);
        }
    }
    if let Some(m) = Some(info.monitor).filter(|m| !m.is_empty()) {
        filter = filter.intersect(&m).unwrap_or(filter);
    }
    let max = opts.max_elements;
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let (elements, truncated) = run(SNAPSHOT_TIMEOUT, "snapshot", move |ctx| {
        let uia = ctx.uia()?;
        let cache = ctx.cache()?;
        let cond = ctx.condition()?;
        let mut raw_ui: Vec<UiElement> = Vec::new();
        let mut raw_com: Vec<IUIAutomationElement> = Vec::new();
        for (i, wid) in std::iter::once(main_id).chain(popup_ids.iter().copied()).enumerate() {
            // SAFETY: COM calls on the worker thread.
            let root = match unsafe { uia.ElementFromHandleBuildCache(hwnd(wid), &cache) } {
                Ok(r) => r,
                Err(e) if i == 0 => return Err(format!("UI Automation can't read this window: {}", e.message())),
                Err(_) => continue,
            };
            // SAFETY: as above.
            let found = unsafe { root.FindAllBuildCache(TreeScope_Subtree, &cond, &cache) };
            let Ok(arr) = found else { continue };
            // SAFETY: as above.
            let n = unsafe { arr.Length() }.unwrap_or(0);
            for k in 0..n.min(4000) {
                // SAFETY: index < Length.
                let Ok(el) = (unsafe { arr.GetElement(k) }) else { continue };
                if let Some(e) = read_element(&el) {
                    raw_ui.push(e);
                    raw_com.push(el);
                }
            }
        }
        // The focused element may sit outside the condition (custom control): add it.
        // SAFETY: as above.
        if let Ok(f) = unsafe { uia.GetFocusedElementBuildCache(&cache) } {
            if let Some(mut e) = read_element(&f) {
                if !raw_ui.iter().any(|r| r.focused) && filter.intersect(&e.rect).is_some() {
                    e.focused = true;
                    raw_ui.push(e);
                    raw_com.push(f);
                }
            }
        }
        let (kept, truncated) = ui::finalize(raw_ui, &filter, max);
        ctx.gen = gen;
        ctx.ids = kept.iter().map(|e| e.backend_index).collect();
        ctx.elements = raw_com;
        Ok((kept, truncated))
    })?;
    Ok(UiSnapshot {
        gen,
        window: info,
        elements,
        source: Source::Uia,
        took_ms: t0.elapsed().as_millis() as u64,
        truncated,
    })
}

// ───────────────────────────── element operations ─────────────────────────────

pub(super) fn element_op(gen: u64, id: u32, op: ElementOp) -> Result<(), String> {
    run(OP_TIMEOUT, "element op", move |ctx| {
        let el = ctx.element(gen, id)?;
        // SAFETY: COM calls on the worker thread; the element came from this worker.
        unsafe {
            let err = |e: windows::core::Error| e.message().to_string();
            match op {
                ElementOp::Invoke => el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId).map_err(err)?.Invoke().map_err(err),
                ElementOp::Toggle => el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId).map_err(err)?.Toggle().map_err(err),
                ElementOp::Select => el
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
                    .map_err(err)?
                    .Select()
                    .map_err(err),
                ElementOp::Expand => el
                    .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
                    .map_err(err)?
                    .Expand()
                    .map_err(err),
                ElementOp::Collapse => el
                    .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
                    .map_err(err)?
                    .Collapse()
                    .map_err(err),
                ElementOp::Focus => el.SetFocus().map_err(err),
                ElementOp::SetValue(text) => {
                    if el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false) {
                        return Err("Refusing to write into a password field.".into());
                    }
                    el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                        .map_err(err)?
                        .SetValue(&BSTR::from(text.as_str()))
                        .map_err(err)
                }
                ElementOp::Scroll { dir, amount } => {
                    let p = el.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId).map_err(err)?;
                    let (inc, dec) = if amount >= 5 {
                        (ScrollAmount_LargeIncrement, ScrollAmount_LargeDecrement)
                    } else {
                        (ScrollAmount_SmallIncrement, ScrollAmount_SmallDecrement)
                    };
                    let times = if amount >= 5 { (amount / 5).max(1) } else { amount.max(1) * 3 };
                    for _ in 0..times {
                        let r = match dir {
                            ScrollDir::Down => p.Scroll(ScrollAmount_NoAmount, inc),
                            ScrollDir::Up => p.Scroll(ScrollAmount_NoAmount, dec),
                            ScrollDir::Right => p.Scroll(inc, ScrollAmount_NoAmount),
                            ScrollDir::Left => p.Scroll(dec, ScrollAmount_NoAmount),
                        };
                        r.map_err(err)?;
                    }
                    Ok(())
                }
            }
        }
    })
}

pub(super) fn quick_state(gen: u64, target: Option<u32>) -> Result<QuickState, String> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: no arguments.
    let fg = unsafe { GetForegroundWindow() };
    let window_id = fg.0 as usize as u64;
    let title = window_title(fg);
    let (focused, target_state) = run(OP_TIMEOUT, "quick state", move |ctx| {
        let uia = ctx.uia()?;
        // SAFETY: COM calls on the worker thread.
        unsafe {
            let focused = uia
                .GetFocusedElement()
                .map(|f| {
                    format!(
                        "{} {}",
                        role_of(f.CurrentControlType().unwrap_or_default()).label(),
                        bstr(f.CurrentName())
                    )
                })
                .unwrap_or_default();
            let mut t = String::new();
            if let Some(id) = target {
                if let Ok(el) = ctx.element(gen, id) {
                    let cur = |p| el.GetCurrentPropertyValue(p).ok();
                    if !el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false) {
                        if let Some(v) = cur(UIA_ValueValuePropertyId) {
                            t.push_str(&BSTR::try_from(&v).map(|b| b.to_string()).unwrap_or_default());
                        }
                    }
                    for p in [UIA_ToggleToggleStatePropertyId, UIA_ExpandCollapseExpandCollapseStatePropertyId] {
                        if let Some(v) = cur(p) {
                            t.push_str(&format!("|{}", i32::try_from(&v).unwrap_or(-1)));
                        }
                    }
                    if let Some(v) = cur(UIA_ScrollVerticalScrollPercentPropertyId) {
                        t.push_str(&format!("|{:.1}", f64::try_from(&v).unwrap_or(-1.0)));
                    }
                }
            }
            Ok((focused, t))
        }
    })?;
    Ok(QuickState {
        window_id,
        title,
        focused,
        target: target_state,
    })
}

pub(super) fn foreground_window() -> Option<WindowInfo> {
    target_window(true).map(window_info)
}

// ───────────────────────────── input ─────────────────────────────

fn vk(k: Key) -> (u16, bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let (v, ext) = match k {
        Key::Ctrl => (VK_CONTROL, false),
        Key::Shift => (VK_SHIFT, false),
        Key::Alt => (VK_MENU, false),
        Key::Win => (VK_LWIN, true),
        Key::Enter => (VK_RETURN, false),
        Key::Tab => (VK_TAB, false),
        Key::Escape => (VK_ESCAPE, false),
        Key::Backspace => (VK_BACK, false),
        Key::Delete => (VK_DELETE, true),
        Key::Insert => (VK_INSERT, true),
        Key::Home => (VK_HOME, true),
        Key::End => (VK_END, true),
        Key::PageUp => (VK_PRIOR, true),
        Key::PageDown => (VK_NEXT, true),
        Key::Up => (VK_UP, true),
        Key::Down => (VK_DOWN, true),
        Key::Left => (VK_LEFT, true),
        Key::Right => (VK_RIGHT, true),
        Key::Space => (VK_SPACE, false),
        Key::Menu => (VK_APPS, true),
        Key::F(n) => (VIRTUAL_KEY(VK_F1.0 + (n.clamp(1, 24) as u16 - 1)), false),
        Key::Char(c) => (VIRTUAL_KEY(c.to_ascii_uppercase() as u16), false),
    };
    (v.0, ext)
}

fn virtual_screen() -> Rect {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
    // SAFETY: plain metrics.
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn send(inputs: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT]) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT};
    // SAFETY: slice of fully initialised INPUTs with the right struct size.
    let n = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() {
        Ok(())
    } else {
        // UIPI: input to an elevated window is blocked for a non-elevated Soul.
        Err("Windows blocked the synthetic input (is the target app running as administrator?).".into())
    }
}

fn mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(super) fn send_step(step: &InputStep) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    ensure_dpi_aware_thread();
    match *step {
        InputStep::MoveTo(p) => {
            let vs = match coords::virtual_bounds(&monitors()) {
                r if r.is_empty() => virtual_screen(),
                r => r,
            };
            let (ax, ay) = coords::to_absolute(p, vs);
            send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, ax, ay, 0)])?;
            let mut now = POINT::default();
            // SAFETY: out point.
            if unsafe { GetCursorPos(&mut now) }.is_ok() && ((now.x - p.x).abs() > 1 || (now.y - p.y).abs() > 1) {
                // Rounding / unusual layouts: put it exactly where it belongs.
                // SAFETY: plain call.
                unsafe { SetCursorPos(p.x, p.y) }.map_err(|e| format!("Moving the mouse failed: {e}"))?;
            }
            Ok(())
        }
        InputStep::ButtonDown(b) | InputStep::ButtonUp(b) => {
            let down = matches!(step, InputStep::ButtonDown(_));
            let f = match (b, down) {
                (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            };
            send(&[mouse(f, 0, 0, 0)])
        }
        InputStep::Wheel { horizontal, notches } => {
            let f = if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL };
            send(&[mouse(f, 0, 0, notches * 120)])
        }
        InputStep::KeyDown(k) | InputStep::KeyUp(k) => {
            let (v, ext) = vk(k);
            // SAFETY: plain call.
            let scan = unsafe { MapVirtualKeyW(v as u32, MAPVK_VK_TO_VSC) } as u16;
            let mut f = KEYBD_EVENT_FLAGS(0);
            if ext {
                f |= KEYEVENTF_EXTENDEDKEY;
            }
            if matches!(step, InputStep::KeyUp(_)) {
                f |= KEYEVENTF_KEYUP;
            }
            send(&[key_input(v, scan, f)])
        }
        InputStep::Unicode(u) => send(&[
            key_input(0, u, KEYEVENTF_UNICODE),
            key_input(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ]),
        InputStep::Pause(ms) => {
            std::thread::sleep(Duration::from_millis(ms as u64));
            Ok(())
        }
    }
}

pub(super) fn probe() -> super::input::InputProbe {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    ensure_dpi_aware_thread();
    let mut p = POINT::default();
    // SAFETY: plain calls / out point.
    let (esc, cursor) = unsafe {
        let esc = (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0;
        let cursor = GetCursorPos(&mut p).ok().map(|_| Point::new(p.x, p.y));
        (esc, cursor)
    };
    super::input::InputProbe { esc_down: esc, cursor }
}

pub(super) fn monitors() -> Vec<Monitor> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    ensure_dpi_aware_thread();
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<Monitor>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(m, &mut mi).as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            out.push(Monitor {
                rect: rect_of(mi.rcMonitor),
                scale: dx.max(1) as f64 / 96.0,
            });
        }
        true.into()
    }
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: `out` outlives the synchronous enumeration.
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

pub(super) fn exclude_from_capture(id: u64, on: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    // SAFETY: user32 validates the handle.
    unsafe { SetWindowDisplayAffinity(hwnd(id), if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }) }
        .map_err(|e| format!("SetWindowDisplayAffinity: {e}"))
}

/// Click-through, never-activated, no taskbar button (agent highlight ring).
pub(super) fn prepare_overlay(id: u64) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };
    // SAFETY: user32 validates the handle; only extended style bits are added.
    unsafe {
        let h = hwnd(id);
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let add = WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0;
        SetWindowLongW(h, GWL_EXSTYLE, (ex | add) as i32);
    }
}

pub(super) fn show_overlay(id: u64, show: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNOACTIVATE};
    // SAFETY: user32 validates the handle.
    unsafe {
        let _ = ShowWindow(hwnd(id), if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
    true
}

/// Opt-in: print the accessibility snapshot of whatever window is focused after 3 s.
///   cargo test --manifest-path src-tauri/Cargo.toml --lib uia_snapshot_probe -- --ignored --nocapture
#[cfg(test)]
#[test]
#[ignore]
fn uia_snapshot_probe() {
    std::thread::sleep(Duration::from_secs(3));
    let s = snapshot(SnapshotOptions::default()).expect("snapshot");
    eprintln!(
        "window={:?} process={} rect={:?} elements={} truncated={} took={} ms useless={}",
        s.window.title,
        s.window.process,
        s.window.rect,
        s.elements.len(),
        s.truncated,
        s.took_ms,
        s.is_useless()
    );
    eprintln!("{}", s.observation());
    eprintln!("monitors: {:?}", monitors());
}
