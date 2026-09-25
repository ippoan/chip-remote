//! Non-Windows stub so the workspace builds and tests on Linux / macOS.

use crate::{Action, ActionError, ChipInfo, Labels, WindowInfo};
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

    pub fn act(
        &self,
        _title: &str,
        _tldr: Option<&str>,
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
        assert_eq!(
            u.act("t", None, Action::Start, Duration::ZERO),
            Err(ActionError::ClaudeNotRunning)
        );
        assert!(!keep_awake());
    }
}
