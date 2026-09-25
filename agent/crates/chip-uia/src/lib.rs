//! Finds and presses `spawn_task` chips in Claude desktop through UI Automation.
//!
//! Port of `windows-agent/ChipUia.psm1` (the behaviour there was verified on the real
//! Claude desktop app; see docs/PROTOCOL.md "UIA での chip の見え方").
//!
//! - [`Uia::list_chips`] is read-only: it never moves or raises a window.
//! - [`Uia::act`] / [`Uia::list_chips_raised`] temporarily un-occlude Claude's window,
//!   because Chromium stops updating its a11y tree while the window is covered,
//!   minimized or the display is off.
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
