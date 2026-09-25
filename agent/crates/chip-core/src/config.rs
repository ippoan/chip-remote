//! `%APPDATA%\chip-remote\config.json` — shared with the (retired) PowerShell agent and
//! the Windows hooks (`url` / `token`). Unknown keys are ignored, missing ones default.
//!
//! `accessClientId` / `accessClientSecret` are the Cloudflare Access service token put in
//! front of the Worker (docs/PROTOCOL.md 「認証」). Optional: empty means the headers are
//! not sent (the Worker works without Access too).

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// UI labels of Claude desktop (Japanese UI). Configurable because they change with the
/// UI language / app version.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Labels {
    /// Text that marks a chip StatusBar ("推奨タスク").
    pub marker: String,
    /// Start button ("ワークツリーで開始").
    pub start: String,
    /// Any chip button / group whose name ends with this also counts as "start"
    /// ("開始"). The start button's text depends on where the session runs: local
    /// sessions say "ワークツリーで開始", SSH sessions "mini-ryzen-claudeでworktreeを使って開始"
    /// (measured). Empty disables suffix matching.
    #[serde(rename = "startSuffix")]
    pub start_suffix: String,
    /// Dismiss button ("提案を非表示").
    pub dismiss: String,
    /// Pager "next" button shown when a session has several chips ("次の提案を表示").
    pub next: String,
}

impl Default for Labels {
    fn default() -> Self {
        Labels {
            marker: "推奨タスク".into(),
            start: "ワークツリーで開始".into(),
            start_suffix: "開始".into(),
            dismiss: "提案を非表示".into(),
            next: "次の提案を表示".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Worker origin, e.g. `https://chip-remote.ippoan.org`.
    pub url: String,
    /// Bearer token (`CHIP_REMOTE_TOKEN`).
    pub token: String,
    pub labels: Labels,
    /// Read-only locate: report `chip.not_found` after this many seconds.
    pub locate_timeout_sec: u64,
    /// Read-only locate scan interval.
    pub scan_interval_sec: u64,
    /// Wait after close code 4000 (another agent took over) before reconnecting.
    pub takeover_backoff_sec: u64,
    /// After un-occluding Claude's window, how long to wait for Chromium to render.
    pub raise_wait_sec: f64,
    /// Block system sleep while the agent runs (SetThreadExecutionState).
    pub prevent_sleep: bool,
    /// Report chips found in Claude desktop's session files to the Worker
    /// (`POST` / `DELETE /v1/chips`), so the hooks are not needed on this PC.
    pub watch_sessions: bool,
    /// Where Claude desktop keeps its session files. Empty: the default
    /// `%APPDATA%\Claude\claude-code-sessions` ([`Config::sessions_dir`]).
    pub sessions_dir: String,
    /// `host` of chips reported from the session files (shared with the Windows hook).
    /// Empty: the machine name.
    pub host: String,
    /// Cloudflare Access service token id (`CF-Access-Client-Id`). Empty: not sent.
    pub access_client_id: String,
    /// Cloudflare Access service token secret (`CF-Access-Client-Secret`). Never logged.
    pub access_client_secret: String,
}

/// Header carrying [`Config::access_client_id`] (lower case: usable with `from_static`).
pub const ACCESS_CLIENT_ID_HEADER: &str = "cf-access-client-id";
/// Header carrying [`Config::access_client_secret`].
pub const ACCESS_CLIENT_SECRET_HEADER: &str = "cf-access-client-secret";

impl Default for Config {
    fn default() -> Self {
        Config {
            url: String::new(),
            token: String::new(),
            labels: Labels::default(),
            locate_timeout_sec: 5,
            scan_interval_sec: 2,
            takeover_backoff_sec: 300,
            raise_wait_sec: 5.0,
            prevent_sleep: true,
            watch_sessions: true,
            sessions_dir: String::new(),
            host: String::new(),
            access_client_id: String::new(),
            access_client_secret: String::new(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(PathBuf, std::io::Error),
    Parse(PathBuf, serde_json::Error),
    /// `url` or `token` is empty / still the placeholder from config.example.json.
    Incomplete(PathBuf),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(p, e) => write!(f, "{}: {}", p.display(), e),
            ConfigError::Parse(p, e) => write!(f, "{}: invalid JSON: {}", p.display(), e),
            ConfigError::Incomplete(p) => write!(f, "{}: url / token is not set", p.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// `<appdata>\chip-remote\config.json`.
    pub fn default_path(appdata: &Path) -> PathBuf {
        appdata.join("chip-remote").join("config.json")
    }

    pub fn from_json(text: &str) -> Result<Config, serde_json::Error> {
        // Tolerate a UTF-8 BOM (Notepad / PowerShell Out-File write one).
        serde_json::from_str(text.trim_start_matches('\u{feff}'))
    }

    /// Loads and validates (url / token must be set).
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Io(path.to_path_buf(), e))?;
        let mut cfg =
            Config::from_json(&text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
        cfg.token = cfg.token.trim().to_string();
        cfg.url = cfg.url.trim().trim_end_matches('/').to_string();
        cfg.access_client_id = cfg.access_client_id.trim().to_string();
        cfg.access_client_secret = cfg.access_client_secret.trim().to_string();
        if cfg.url.is_empty() || cfg.token.is_empty() || cfg.token.starts_with("REPLACE_") {
            return Err(ConfigError::Incomplete(path.to_path_buf()));
        }
        Ok(cfg)
    }

    /// Directory with Claude desktop's session files: `sessionsDir`, or
    /// `<appdata>\Claude\claude-code-sessions`.
    pub fn sessions_dir(&self, appdata: &Path) -> PathBuf {
        let d = self.sessions_dir.trim();
        if d.is_empty() {
            appdata.join("Claude").join("claude-code-sessions")
        } else {
            PathBuf::from(d)
        }
    }

    /// Cloudflare Access headers `[(name, value); 2]`, only when both the id and the
    /// secret are set (values trimmed). None: send neither.
    pub fn access_headers(&self) -> Option<[(&'static str, &str); 2]> {
        let id = self.access_client_id.trim();
        let secret = self.access_client_secret.trim();
        if id.is_empty() || secret.is_empty() {
            return None;
        }
        Some([
            (ACCESS_CLIENT_ID_HEADER, id),
            (ACCESS_CLIENT_SECRET_HEADER, secret),
        ])
    }

    /// For the log: `on`, `off`, or `off` with a hint when only one of the two is set.
    /// Never contains the id or the secret.
    pub fn access_state(&self) -> &'static str {
        let id = !self.access_client_id.trim().is_empty();
        let secret = !self.access_client_secret.trim().is_empty();
        match (id, secret) {
            (true, true) => "on",
            (false, false) => "off",
            _ => "off (accessClientId / accessClientSecret の片方しか設定されていません)",
        }
    }

    /// `https://x` → `wss://x/v1/agent/ws`, `http://x` → `ws://x/v1/agent/ws`.
    pub fn ws_url(&self) -> String {
        let base = self.url.trim_end_matches('/');
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        format!("{ws}/v1/agent/ws")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_keys_missing() {
        let c = Config::from_json(r#"{"url":"https://a","token":"t"}"#).unwrap();
        assert_eq!(c.locate_timeout_sec, 5);
        assert_eq!(c.labels, Labels::default());
        assert!(c.prevent_sleep);
        assert_eq!(c.raise_wait_sec, 5.0);
        assert!(c.watch_sessions);
        assert_eq!(c.sessions_dir, "");
        assert_eq!(c.host, "");
        assert_eq!(c.access_client_id, "");
        assert_eq!(c.access_client_secret, "");
        assert!(c.access_headers().is_none());
        assert_eq!(c.access_state(), "off");
    }

    #[test]
    fn access_keys_are_read_trimmed_and_sent_only_as_a_pair() {
        let dir = std::env::temp_dir().join(format!("chip-core-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        std::fs::write(
            &p,
            r#"{"url":"https://a","token":"t","accessClientId":" id.access \r\n","accessClientSecret":"\tsec "}"#,
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(c.access_client_id, "id.access");
        assert_eq!(c.access_client_secret, "sec");
        assert_eq!(
            c.access_headers(),
            Some([
                ("cf-access-client-id", "id.access"),
                ("cf-access-client-secret", "sec")
            ])
        );
        assert_eq!(c.access_state(), "on");

        let only_id = Config {
            access_client_id: "id".into(),
            ..Config::default()
        };
        assert!(only_id.access_headers().is_none());
        assert!(only_id.access_state().starts_with("off ("));
        let only_secret = Config {
            access_client_secret: "sec".into(),
            ..Config::default()
        };
        assert!(only_secret.access_headers().is_none());
        assert!(!only_secret.access_state().contains("sec"));
        let blank = Config {
            access_client_id: " ".into(),
            access_client_secret: " ".into(),
            ..Config::default()
        };
        assert!(blank.access_headers().is_none());
    }

    #[test]
    fn sessions_dir_default_and_override() {
        let appdata = Path::new("C:/Users/x/AppData/Roaming");
        let c = Config::from_json(r#"{"watchSessions":false}"#).unwrap();
        assert!(!c.watch_sessions);
        assert!(c
            .sessions_dir(appdata)
            .ends_with(Path::new("Claude").join("claude-code-sessions")));
        let c = Config::from_json(r#"{"sessionsDir":" D:/s "}"#).unwrap();
        assert_eq!(c.sessions_dir(appdata), PathBuf::from("D:/s"));
    }

    #[test]
    fn reads_powershell_era_config_with_bom_and_partial_labels() {
        let text = "\u{feff}{\"url\":\"https://a/\",\"token\":\" t \\r\\n\",\"labels\":{\"start\":\"Start\"},\"locateTimeoutSec\":9,\"unknown\":1}";
        let dir = std::env::temp_dir().join(format!("chip-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        std::fs::write(&p, text).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.token, "t");
        assert_eq!(c.url, "https://a");
        assert_eq!(c.labels.start, "Start");
        assert_eq!(c.labels.dismiss, "提案を非表示");
        assert_eq!(c.locate_timeout_sec, 9);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn placeholder_token_is_incomplete() {
        let dir = std::env::temp_dir().join(format!("chip-core-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        std::fs::write(
            &p,
            r#"{"url":"https://a","token":"REPLACE_WITH_CHIP_REMOTE_TOKEN"}"#,
        )
        .unwrap();
        assert!(matches!(Config::load(&p), Err(ConfigError::Incomplete(_))));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ws_url_conversion() {
        let mut c = Config {
            url: "https://chip-remote.ippoan.org".into(),
            ..Config::default()
        };
        assert_eq!(c.ws_url(), "wss://chip-remote.ippoan.org/v1/agent/ws");
        c.url = "http://localhost:8787/".into();
        assert_eq!(c.ws_url(), "ws://localhost:8787/v1/agent/ws");
    }

    #[test]
    fn default_path() {
        let p = Config::default_path(Path::new("C:/Users/x/AppData/Roaming"));
        assert!(p.ends_with("chip-remote/config.json") || p.ends_with("chip-remote\\config.json"));
    }
}
