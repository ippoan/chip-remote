//! Finds and presses `spawn_task` chips in Claude desktop through UI Automation.
//!
//! Port of `windows-agent/ChipUia.psm1` (the behaviour there was verified on the real
//! Claude desktop app; see docs/PROTOCOL.md "UIA での chip の見え方").
//!
//! - [`Uia::list_chips`] is read-only: it never moves or raises a window.
//! - [`Uia::act`] / [`Uia::list_chips_raised`] temporarily un-occlude Claude's window,
//!   because Chromium stops updating its a11y tree while the window is covered,
//!   minimized or the display is off.
//! - Chips exist in the tree only for sessions whose chat pane is shown. Given the
//!   session title, [`Uia::act`] opens a session that is not shown from the sidebar.
//!   [`Uia::shown_sessions`] / [`Uia::sidebar_sessions`] are read-only diagnostics.
//!
//! A [`Uia`] owns COM objects: create and use it on one thread. On non-Windows targets
//! a stub is compiled whose methods return [`ActionError::ClaudeNotRunning`].

pub use chip_core::protocol::Action;
pub use chip_core::{ActionError, ChipInfo, Labels};

pub mod logic;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{keep_awake, Uia};

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::{keep_awake, Uia};

/// Claude desktop's main window as found by [`Uia::window_info`] (diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// HWND value.
    pub hwnd: isize,
    pub title: String,
    pub minimized: bool,
    pub foreground: bool,
}

/// A session entry in Claude desktop's sidebar as read by [`Uia::sidebar_sessions`]
/// (diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarSession {
    /// The button's full Name, "<status> <title>".
    pub name: String,
    /// Status / PR prefix ("実行中", "アイドル", "#21, #584 · マージ済み", ...).
    pub status: String,
    /// Title split off the name ([`logic::sidebar_session_title`]); None when the name
    /// does not start with "<status> ".
    pub title: Option<String>,
    /// UIA IsOffscreen (scrolled out of the sidebar, or the sidebar is collapsed).
    pub offscreen: bool,
}
