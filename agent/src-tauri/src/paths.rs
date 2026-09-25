//! Well-known locations (same as the retired PowerShell agent) and the config template.

use std::io;
use std::path::{Path, PathBuf};

use chip_core::Config;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// `%APPDATA%` (Roaming): config.json and Claude desktop's session files live under it.
pub fn appdata_dir() -> PathBuf {
    env_dir("APPDATA")
}

/// `%APPDATA%\chip-remote\config.json` (shared with the Windows hooks).
pub fn config_path() -> PathBuf {
    Config::default_path(&appdata_dir())
}

/// `%LOCALAPPDATA%\chip-remote` (agent.log, agent.log.1, first-run marker).
pub fn local_dir() -> PathBuf {
    env_dir("LOCALAPPDATA").join("chip-remote")
}

/// Written by 「設定ファイルを開く」 when config.json does not exist yet. Same keys and
/// defaults as chip_core::Config, token left empty for the user.
pub const CONFIG_TEMPLATE: &str = r#"{
  "url": "https://chip-remote.ippoan.org",
  "token": "",
  "labels": {
    "marker": "推奨タスク",
    "start": "ワークツリーで開始",
    "dismiss": "提案を非表示",
    "next": "次の提案を表示"
  },
  "locateTimeoutSec": 5,
  "scanIntervalSec": 2,
  "takeoverBackoffSec": 300,
  "raiseWaitSec": 5,
  "preventSleep": true,
  "watchSessions": true
}
"#;

/// Creates `path` (UTF-8, no BOM) from [`CONFIG_TEMPLATE`] unless it exists.
/// Returns whether it was created.
pub fn ensure_config_file(path: &Path) -> io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(CONFIG_TEMPLATE.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_core::config::ConfigError;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "chip-remote-agent-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn template_parses_with_defaults_and_is_incomplete() {
        let c = Config::from_json(CONFIG_TEMPLATE).unwrap();
        let d = Config::default();
        assert_eq!(c.labels, d.labels);
        assert_eq!(c.locate_timeout_sec, d.locate_timeout_sec);
        assert_eq!(c.scan_interval_sec, d.scan_interval_sec);
        assert_eq!(c.takeover_backoff_sec, d.takeover_backoff_sec);
        assert_eq!(c.raise_wait_sec, d.raise_wait_sec);
        assert_eq!(c.prevent_sleep, d.prevent_sleep);
        assert_eq!(c.watch_sessions, d.watch_sessions);

        let dir = tmp("tpl");
        let p = dir.join("chip-remote").join("config.json");
        assert!(ensure_config_file(&p).unwrap());
        assert!(matches!(Config::load(&p), Err(ConfigError::Incomplete(_))));
        let bytes = std::fs::read(&p).unwrap();
        assert_ne!(&bytes[..3], b"\xEF\xBB\xBF", "no BOM");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn never_overwrites() {
        let dir = tmp("keep");
        let p = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, "{\"token\":\"mine\"}").unwrap();
        assert!(!ensure_config_file(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"token\":\"mine\"}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
