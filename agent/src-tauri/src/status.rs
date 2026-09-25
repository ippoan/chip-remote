//! Connection state shown as the first (disabled) tray menu item.

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// WebSocket to the Worker is open.
    Connected,
    /// Not connected; waiting for the next reconnect attempt (also the initial state).
    Disconnected,
    /// config.json is missing, unreadable or has no url / token. Retried every 30 s.
    NoConfig,
    /// The Worker closed us with 4000: another agent connected. Waits takeoverBackoffSec.
    Replaced,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Connected => "接続中",
            Status::Disconnected => "切断中 (再接続待ち)",
            Status::NoConfig => "設定がありません",
            Status::Replaced => "別の agent に交代しました",
        }
    }
}

/// Where the agent loop reports its state (the tray in the app, a channel in tests).
pub type StatusSink = Arc<dyn Fn(Status) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(Status::Connected.label(), "接続中");
        assert_eq!(Status::Disconnected.label(), "切断中 (再接続待ち)");
        assert_eq!(Status::NoConfig.label(), "設定がありません");
        assert_eq!(Status::Replaced.label(), "別の agent に交代しました");
    }
}
