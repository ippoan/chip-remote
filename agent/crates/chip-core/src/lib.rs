//! Platform-independent core of the chip-remote Windows agent.
//!
//! - [`config`] — `%APPDATA%\chip-remote\config.json` (same file the PowerShell agent used)
//! - [`protocol`] — WebSocket messages exchanged with the Worker (docs/PROTOCOL.md)
//! - [`matching`] — picking the chip that corresponds to a task (title, then tldr)
//! - [`ActionError`] — error codes reported in `action.result`

pub mod config;
pub mod matching;
pub mod protocol;

pub use config::{Config, Labels};
pub use matching::{normalize, select_chip, ChipInfo};

/// Why an action (or a locate) could not be completed. `code()` is the wire value
/// for `action.result.error` (docs/PROTOCOL.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// No claude.exe with a main window.
    ClaudeNotRunning,
    /// The chip is not in the UIA tree (pane not shown, already gone, ...).
    ChipNotFound,
    /// The chip is there but the requested button is not.
    ButtonNotFound,
    /// UIA call failed; the string is a human-readable detail for the log.
    InvokeFailed(String),
}

impl ActionError {
    pub fn code(&self) -> &'static str {
        match self {
            ActionError::ClaudeNotRunning => "claude_not_running",
            ActionError::ChipNotFound => "chip_not_found",
            ActionError::ButtonNotFound => "button_not_found",
            ActionError::InvokeFailed(_) => "invoke_failed",
        }
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::InvokeFailed(d) => write!(f, "{}: {}", self.code(), d),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for ActionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_protocol() {
        assert_eq!(ActionError::ClaudeNotRunning.code(), "claude_not_running");
        assert_eq!(ActionError::ChipNotFound.code(), "chip_not_found");
        assert_eq!(ActionError::ButtonNotFound.code(), "button_not_found");
        assert_eq!(
            ActionError::InvokeFailed("x".into()).code(),
            "invoke_failed"
        );
        assert_eq!(
            ActionError::InvokeFailed("boom".into()).to_string(),
            "invoke_failed: boom"
        );
    }
}
