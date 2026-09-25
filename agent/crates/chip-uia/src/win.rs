//! Windows backend: IUIAutomation (COM) + a few user32 calls for the un-occlude dance.
//!
//! Port of windows-agent/ChipUia.psm1. The measured facts this relies on are in
//! docs/PROTOCOL.md "UIA での chip の見え方" and in the comments below.

use std::cell::RefCell;
use std::collections::HashMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    IUIAutomationInvokePattern, IUIAutomationTreeWalker, TreeScope, TreeScope_Children,
    TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_ControlTypePropertyId,
    UIA_GroupControlTypeId, UIA_InvokePatternId, UIA_StatusBarControlTypeId, UIA_TextControlTypeId,
    UIA_CONTROLTYPE_ID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetWindowPos, ShowWindow, GW_OWNER,
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE,
};

use crate::logic::{self, Child, Kind, PagerState, SiblingStep};
use crate::{Action, ActionError, ChipInfo, Labels, WindowInfo};

/// Chromium's lazy a11y tree: after this long without a query, the first FindAll
/// returns only the title bar; ask again after WARMUP_DELAY.
const WARMUP_IDLE: Duration = Duration::from_secs(30);
const WARMUP_DELAY: Duration = Duration::from_millis(1500);
/// Poll interval while waiting for chips to render.
const POLL: Duration = Duration::from_millis(400);
/// After any chip rendered, how long to wait for the requested title (other panes'
/// chips may be stale for a moment).
const TITLE_WAIT: Duration = Duration::from_millis(1500);
/// Upper bound of "next" presses while searching a paged chip.
const MAX_PAGES: usize = 20;
/// The a11y tree lags a "next" press by ~300 ms+; wait up to this for the pane title to change.
const PAGE_SETTLE: Duration = Duration::from_secs(2);
const PAGE_POLL: Duration = Duration::from_millis(150);
/// Safety cap for the sibling walk (the real chip has < 10 siblings).
const MAX_SIBLINGS: usize = 64;

fn fail(what: &str) -> impl FnOnce(windows::core::Error) -> ActionError + '_ {
    move |e| ActionError::InvokeFailed(format!("{what}: {}", e.message()))
}

/// Balances CoInitializeEx. Declared as the LAST field of [`Uia`]: struct fields drop
/// in declaration order, so every COM interface is released before CoUninitialize.
struct ComGuard {
    uninit: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { CoUninitialize() };
        }
    }
}

/// A chip plus its live StatusBar element (needed to press its buttons).
struct LiveChip {
    info: ChipInfo,
    element: IUIAutomationElement,
}

fn infos(chips: &[LiveChip]) -> Vec<ChipInfo> {
    chips.iter().map(|c| c.info.clone()).collect()
}

pub struct Uia {
    automation: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    cond_status_bar: IUIAutomationCondition,
    cond_button: IUIAutomationCondition,
    cond_text: IUIAutomationCondition,
    cond_true: IUIAutomationCondition,
    labels: Labels,
    /// hwnd -> last time its tree was queried (lazy a11y warm-up).
    last_query: RefCell<HashMap<isize, Instant>>,
    _com: ComGuard,
}

impl Uia {
    /// Initializes COM (MTA; an already-STA thread is accepted as is) and IUIAutomation.
    pub fn new(labels: Labels) -> Result<Uia, ActionError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com = if hr.is_ok() {
            ComGuard { uninit: true }
        } else if hr == RPC_E_CHANGED_MODE {
            // The thread is already STA (e.g. a UI thread). COM is usable; do not
            // balance a call that did not succeed.
            ComGuard { uninit: false }
        } else {
            return Err(ActionError::InvokeFailed(format!(
                "CoInitializeEx: {}",
                hr.message()
            )));
        };
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(fail("CoCreateInstance(CUIAutomation)"))?;
        let type_cond = |id: UIA_CONTROLTYPE_ID| unsafe {
            automation
                .CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(id.0))
                .map_err(fail("CreatePropertyCondition"))
        };
        let cond_status_bar = type_cond(UIA_StatusBarControlTypeId)?;
        let cond_button = type_cond(UIA_ButtonControlTypeId)?;
        let cond_text = type_cond(UIA_TextControlTypeId)?;
        let cond_true =
            unsafe { automation.CreateTrueCondition() }.map_err(fail("CreateTrueCondition"))?;
        let walker =
            unsafe { automation.ControlViewWalker() }.map_err(fail("ControlViewWalker"))?;
        Ok(Uia {
            automation,
            walker,
            cond_status_bar,
            cond_button,
            cond_text,
            cond_true,
            labels,
            last_query: RefCell::new(HashMap::new()),
            _com: com,
        })
    }

    /// Claude's main window (read-only; for diagnostics).
    pub fn window_info(&self) -> Result<WindowInfo, ActionError> {
        let (hwnd, title) = find_claude_window().ok_or(ActionError::ClaudeNotRunning)?;
        Ok(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
            minimized: unsafe { IsIconic(hwnd) }.as_bool(),
            foreground: unsafe { GetForegroundWindow() } == hwnd,
        })
    }

    /// Read-only: never moves/raises any window. Handles the lazy-a11y warm-up.
    pub fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError> {
        let (hwnd, win) = self.window()?;
        let key = hwnd.0 as isize;
        let last = self.last_query.borrow().get(&key).copied();
        let cold = logic::is_cold(last, Instant::now(), WARMUP_IDLE);
        let mut chips = self.read_chips(&win)?;
        if chips.is_empty() && cold {
            // The first query only wakes Chromium's a11y tree up; ask again.
            sleep(WARMUP_DELAY);
            chips = self.read_chips(&win)?;
        }
        self.touch(key);
        Ok(infos(&chips))
    }

    /// Un-occlude, wait until any chip renders (<= raise_wait), then list. No paging,
    /// no pressing. For the probe CLI's --raise.
    pub fn list_chips_raised(&self, raise_wait: Duration) -> Result<Vec<ChipInfo>, ActionError> {
        let (hwnd, win) = self.window()?;
        let _raised = RaiseGuard::raise(hwnd);
        Ok(infos(&self.wait_chips(&win, hwnd, None, raise_wait)))
    }

    /// Un-occlude → wait for any chip (<= raise_wait) → wait for `title` (<= 1.5 s) →
    /// page with "next" if needed → press start/dismiss → restore the window state.
    pub fn act(
        &self,
        title: &str,
        tldr: Option<&str>,
        action: Action,
        raise_wait: Duration,
    ) -> Result<(), ActionError> {
        let (hwnd, win) = self.window()?;
        // The window is usually covered while the user is away, so Chromium has not
        // rendered the chip; keep it un-occluded for find + invoke (restored on drop).
        let _raised = RaiseGuard::raise(hwnd);
        self.wait_chips(&win, hwnd, None, raise_wait);
        self.wait_chips(&win, hwnd, Some((title, tldr)), TITLE_WAIT);
        let hit = self
            .find_paged(&win, title, tldr)?
            .ok_or(ActionError::ChipNotFound)?;
        let label = match action {
            Action::Start => &self.labels.start,
            Action::Dismiss => &self.labels.dismiss,
        };
        // InvokePattern on a Chromium button moves focus: Claude becomes active (accepted).
        self.invoke_button(&hit.element, label)
    }

    // ------------------------------------------------------------------ internals

    fn window(&self) -> Result<(HWND, IUIAutomationElement), ActionError> {
        let (hwnd, _) = find_claude_window().ok_or(ActionError::ClaudeNotRunning)?;
        let el = unsafe { self.automation.ElementFromHandle(hwnd) }
            .map_err(|_| ActionError::ClaudeNotRunning)?;
        Ok((hwnd, el))
    }

    fn touch(&self, key: isize) {
        self.last_query.borrow_mut().insert(key, Instant::now());
    }

    /// Polls until a chip matching `title` (or any chip when None) is in the tree, or
    /// `timeout` elapses. Returns the last list. Errors count as "nothing yet".
    fn wait_chips(
        &self,
        win: &IUIAutomationElement,
        hwnd: HWND,
        title: Option<(&str, Option<&str>)>,
        timeout: Duration,
    ) -> Vec<LiveChip> {
        let deadline = Instant::now() + timeout;
        loop {
            let chips = self.read_chips(win).unwrap_or_default();
            self.touch(hwnd.0 as isize);
            let done = match title {
                Some((t, d)) => chip_core::select_chip(&infos(&chips), t, d).is_some(),
                None => !chips.is_empty(),
            };
            if done || Instant::now() >= deadline {
                return chips;
            }
            sleep(POLL);
        }
    }

    /// When a session has several chips only the current one is rendered: press "next"
    /// on each paged pane until `title` shows up or every pane cycled back to a title
    /// already seen.
    fn find_paged(
        &self,
        win: &IUIAutomationElement,
        title: &str,
        tldr: Option<&str>,
    ) -> Result<Option<LiveChip>, ActionError> {
        let next = self.labels.next.as_str();
        let mut pager = PagerState::default();
        for _ in 0..=MAX_PAGES {
            let mut chips = self.read_chips(win)?;
            let list = infos(&chips);
            if let Some(i) = chip_core::select_chip(&list, title, tldr) {
                return Ok(Some(chips.swap_remove(i)));
            }
            let mut pressed: HashMap<i32, String> = HashMap::new();
            for i in pager.plan(&list, next) {
                if self.invoke_button(&chips[i].element, next).is_ok() {
                    pressed.insert(list[i].pane_x, list[i].title.clone());
                }
            }
            if pressed.is_empty() {
                return Ok(None);
            }
            // Wait until every pressed pane shows a different title, or the next round
            // would see the old one as "cycled".
            let deadline = Instant::now() + PAGE_SETTLE;
            loop {
                sleep(PAGE_POLL);
                let now = infos(&self.read_chips(win).unwrap_or_default());
                if !logic::any_stale(&pressed, &now) || Instant::now() >= deadline {
                    break;
                }
            }
        }
        Ok(None)
    }

    fn read_chips(&self, win: &IUIAutomationElement) -> Result<Vec<LiveChip>, ActionError> {
        let bars = unsafe { win.FindAll(TreeScope_Descendants, &self.cond_status_bar) }
            .map_err(fail("FindAll(StatusBar)"))?;
        let n = unsafe { bars.Length() }.unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..n {
            let Ok(bar) = (unsafe { bars.GetElement(i) }) else {
                continue;
            };
            // Errors mean the element vanished while we read it; skip it.
            if let Ok(Some(info)) = self.read_chip(&bar) {
                out.push(LiveChip { info, element: bar });
            }
        }
        Ok(out)
    }

    fn read_chip(&self, bar: &IUIAutomationElement) -> windows::core::Result<Option<ChipInfo>> {
        let kids = self.find_all(bar, TreeScope_Children, &self.cond_true)?;
        let nodes: Vec<Child> = kids
            .iter()
            .map(|k| Child {
                kind: kind(k),
                name: name(k),
            })
            .collect();
        let Some((title, tldr)) =
            logic::parse_chip(&nodes, &self.labels, |i| self.group_tldr(&kids[i]))
        else {
            return Ok(None);
        };
        let buttons = self.chip_buttons(bar);
        // BoundingRectangle is an integer RECT in the COM API (the .NET wrapper's
        // ±Infinity for offscreen elements cannot occur here); an error maps to 0.
        let pane_x = unsafe { bar.CurrentBoundingRectangle() }
            .map(|r| r.left)
            .unwrap_or(0);
        Ok(Some(ChipInfo {
            title,
            tldr,
            buttons: buttons.iter().map(name).collect(),
            pane_x,
        }))
    }

    /// Some(text) when the group is the text-only tldr group, None when it holds buttons.
    fn group_tldr(&self, group: &IUIAutomationElement) -> Option<String> {
        // FindFirst returns Err for "not found" (null element) as well as for failures.
        if unsafe { group.FindFirst(TreeScope_Descendants, &self.cond_button) }.is_ok() {
            return None;
        }
        let texts: Vec<String> = self
            .find_all(group, TreeScope_Descendants, &self.cond_text)
            .unwrap_or_default()
            .iter()
            .map(name)
            .collect();
        Some(logic::join_tldr(&texts, &name(group)))
    }

    /// The chip's buttons: descendants of the StatusBar (in case a future layout nests
    /// them) plus the buttons among its following siblings — measured layout: dismiss,
    /// optional pager, start group (logic::sibling_step decides where the chip ends).
    fn chip_buttons(&self, bar: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
        let mut out = self
            .find_all(bar, TreeScope_Descendants, &self.cond_button)
            .unwrap_or_default();
        let mut sib = unsafe { self.walker.GetNextSiblingElement(bar) }.ok();
        let mut steps = 0;
        while let Some(s) = sib {
            steps += 1;
            if steps > MAX_SIBLINGS {
                break;
            }
            match logic::sibling_step(kind(&s), &name(&s), &self.labels) {
                SiblingStep::TakeButton => out.push(s.clone()),
                SiblingStep::TakeGroupButtons => out.extend(
                    self.find_all(&s, TreeScope_Descendants, &self.cond_button)
                        .unwrap_or_default(),
                ),
                SiblingStep::Skip => {}
                SiblingStep::Stop => break,
            }
            sib = unsafe { self.walker.GetNextSiblingElement(&s) }.ok();
        }
        out
    }

    fn invoke_button(&self, chip: &IUIAutomationElement, label: &str) -> Result<(), ActionError> {
        // A stale element (chip gone since it was read) fails every property read.
        if unsafe { chip.CurrentControlType() }.is_err() {
            return Err(ActionError::ChipNotFound);
        }
        let btn = self
            .chip_buttons(chip)
            .into_iter()
            .find(|b| name(b) == label)
            .ok_or(ActionError::ButtonNotFound)?;
        if !unsafe { btn.CurrentIsEnabled() }
            .map(|b| b.as_bool())
            .unwrap_or(true)
        {
            return Err(ActionError::InvokeFailed("button is disabled".into()));
        }
        let pattern: IUIAutomationInvokePattern =
            unsafe { btn.GetCurrentPatternAs(UIA_InvokePatternId) }
                .map_err(|_| ActionError::InvokeFailed("InvokePattern not supported".into()))?;
        unsafe { pattern.Invoke() }.map_err(fail("Invoke"))
    }

    fn find_all(
        &self,
        el: &IUIAutomationElement,
        scope: TreeScope,
        cond: &IUIAutomationCondition,
    ) -> windows::core::Result<Vec<IUIAutomationElement>> {
        let arr = unsafe { el.FindAll(scope, cond) }?;
        let n = unsafe { arr.Length() }?;
        Ok((0..n)
            .filter_map(|i| unsafe { arr.GetElement(i) }.ok())
            .collect())
    }
}

fn name(el: &IUIAutomationElement) -> String {
    unsafe { el.CurrentName() }
        .map(|b| b.to_string())
        .unwrap_or_default()
}

fn kind(el: &IUIAutomationElement) -> Kind {
    match unsafe { el.CurrentControlType() } {
        Ok(t) if t == UIA_StatusBarControlTypeId => Kind::StatusBar,
        Ok(t) if t == UIA_TextControlTypeId => Kind::Text,
        Ok(t) if t == UIA_GroupControlTypeId => Kind::Group,
        Ok(t) if t == UIA_ButtonControlTypeId => Kind::Button,
        _ => Kind::Other,
    }
}

// ---------------------------------------------------------------------- window

unsafe extern "system" fn collect_hwnd(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<HWND> passed by find_claude_window, alive for the
    // duration of EnumWindows.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    out.push(hwnd);
    BOOL(1)
}

fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Claude desktop's main window: a visible, unowned top-level window of claude.exe
/// (what .NET's Process.MainWindowHandle picks), preferring the one titled "Claude".
/// The Claude Code CLI is also claude.exe but has no such window.
fn find_claude_window() -> Option<(HWND, String)> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_hwnd),
            LPARAM(&mut all as *mut Vec<HWND> as isize),
        );
    }
    let mut is_claude: HashMap<u32, bool> = HashMap::new();
    let mut cands: Vec<(HWND, String)> = Vec::new();
    for hwnd in all {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                continue;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                continue;
            }
            let ok = *is_claude
                .entry(pid)
                .or_insert_with(|| process_image(pid).is_some_and(|p| logic::is_claude_image(&p)));
            if ok {
                cands.push((hwnd, window_title(hwnd)));
            }
        }
    }
    let titles: Vec<String> = cands.iter().map(|(_, t)| t.clone()).collect();
    logic::pick_main_window(&titles).map(|i| cands.swap_remove(i))
}

// ---------------------------------------------------------------------- raise

/// Keeps Claude's window un-occluded while alive, restores z-order / minimized state
/// on drop.
///
/// Why: while the window is covered, minimized or the display is off, Chromium stops
/// rendering, and chips that appeared meanwhile are not in the a11y tree. Measured fix
/// (~0.3 s until the chip appears): wake the display, make the window TOPMOST without
/// activating it, then nudge it 1px and back — Chromium recomputes occlusion only on
/// window events, a z-order change alone does not trigger a re-render.
struct RaiseGuard {
    hwnd: HWND,
    prev_fg: HWND,
    was_min: bool,
    raised: bool,
}

impl RaiseGuard {
    fn raise(hwnd: HWND) -> RaiseGuard {
        unsafe {
            // One-shot (no ES_CONTINUOUS): resets the display idle timer. Chromium treats
            // a powered-off display as occluding every window.
            SetThreadExecutionState(ES_DISPLAY_REQUIRED);
            let prev_fg = GetForegroundWindow();
            let was_min = IsIconic(hwnd).as_bool();
            let mut g = RaiseGuard {
                hwnd,
                prev_fg,
                was_min,
                raised: false,
            };
            if prev_fg == hwnd && !was_min {
                return g; // already visible and in front
            }
            g.raised = true;
            if was_min {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE,
            );
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_ok() {
                let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
                let _ = SetWindowPos(hwnd, None, r.left + 1, r.top, 0, 0, flags);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, 0, 0, flags);
            }
            g
        }
    }
}

impl Drop for RaiseGuard {
    fn drop(&mut self) {
        if !self.raised {
            return;
        }
        let flags = SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
            if self.was_min {
                let _ = ShowWindow(self.hwnd, SW_SHOWMINNOACTIVE);
            } else if !self.prev_fg.is_invalid() && self.prev_fg != self.hwnd {
                // Insert Claude right behind the window that had the focus.
                let _ = SetWindowPos(self.hwnd, Some(self.prev_fg), 0, 0, 0, 0, flags);
            }
        }
    }
}

/// ES_CONTINUOUS | ES_SYSTEM_REQUIRED on the calling thread: blocks system sleep while
/// that thread lives (call from a long-lived thread). Power settings are untouched; a
/// closed laptop lid still sleeps. true on success.
pub fn keep_awake() -> bool {
    unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) }.0 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // Creating the COM objects touches no window; safe to run on the user's desktop.
    #[test]
    fn creates_uia() {
        assert!(Uia::new(Labels::default()).is_ok());
    }
}
