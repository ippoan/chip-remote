//! Non-Windows stub so the workspace builds and tests on Linux / macOS.

use crate::{Action, ActionError, ChipInfo, Labels, SidebarSession, WindowInfo};
use std::time::Duration;

pub struct Uia {
    _labels: Labels,
}

impl Uia {
    pub fn new(labels: Labels) -> Result<Uia, ActionError> {
        Ok(Uia { _labels: labels })
    }

    pub fn window_info(&self) -> Result<WindowInfo, ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }

    pub fn list_chips(&self) -> Result<Vec<ChipInfo>, ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }

    pub fn shown_sessions(&self) -> Result<Vec<String>, ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }

    pub fn sidebar_sessions(&self) -> Result<Vec<SidebarSession>, ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }

    pub fn last_detail(&self) -> Option<String> {
        None
    }

    pub fn act(
        &self,
        _title: &str,
        _tldr: Option<&str>,
        _session_title: Option<&str>,
        _action: Action,
        _raise_wait: Duration,
    ) -> Result<(), ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }

    pub fn list_chips_raised(&self, _raise_wait: Duration) -> Result<Vec<ChipInfo>, ActionError> {
        Err(ActionError::ClaudeNotRunning)
    }
}

pub fn keep_awake() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_claude_not_running() {
        let u = Uia::new(Labels::default()).unwrap();
        assert_eq!(u.list_chips(), Err(ActionError::ClaudeNotRunning));
        assert_eq!(u.shown_sessions(), Err(ActionError::ClaudeNotRunning));
        assert_eq!(u.sidebar_sessions(), Err(ActionError::ClaudeNotRunning));
        assert_eq!(
            u.act("t", None, Some("s"), Action::Start, Duration::ZERO),
            Err(ActionError::ClaudeNotRunning)
        );
        assert_eq!(u.last_detail(), None);
        assert!(!keep_awake());
    }
}
