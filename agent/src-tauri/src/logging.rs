//! `tracing` → `%LOCALAPPDATA%\chip-remote\agent.log` (rotated to `agent.log.1` at 1 MB,
//! like the PowerShell agent) + an in-memory ring of recent lines for 「ログをコピー」.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::writer::MakeWriterExt;

pub const LOG_FILE: &str = "agent.log";
const LOG_MAX_BYTES: u64 = 1024 * 1024;
const RING_LINES: usize = 2000;

/// Recent formatted log lines (each ends with '\n').
#[derive(Clone, Default)]
pub struct LogRing(Arc<Mutex<VecDeque<String>>>);

impl LogRing {
    fn push(&self, line: String, cap: usize) {
        if let Ok(mut r) = self.0.lock() {
            while r.len() >= cap {
                r.pop_front();
            }
            r.push_back(line);
        }
    }

    pub fn snapshot(&self) -> String {
        self.0
            .lock()
            .map(|r| r.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// Append-only file that is moved to `<name>.1` once it would exceed `max_bytes`.
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        RotatingFile {
            path,
            max_bytes,
            file: None,
            size: 0,
        }
    }

    fn rotated(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(".1");
        PathBuf::from(s)
    }

    fn open(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(f);
        }
        Ok(self.file.as_mut().expect("just opened"))
    }

    pub fn write_line(&mut self, buf: &[u8]) -> io::Result<()> {
        self.open()?;
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.file = None;
            // rename replaces an existing agent.log.1 on Windows (MOVEFILE_REPLACE_EXISTING).
            std::fs::rename(&self.path, Self::rotated(&self.path))?;
            self.open()?;
        }
        let f = self.file.as_mut().expect("open");
        f.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(())
    }
}

/// `MakeWriter` that sends each formatted event to the file and the ring.
#[derive(Clone)]
struct Sink {
    file: Arc<Mutex<RotatingFile>>,
    ring: LogRing,
}

struct SinkWriter(Sink);

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Logging must never take the agent down: errors are swallowed.
        if let Ok(mut f) = self.0.file.lock() {
            let _ = f.write_line(buf);
        }
        self.0
            .ring
            .push(String::from_utf8_lossy(buf).into_owned(), RING_LINES);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = SinkWriter;
    fn make_writer(&'a self) -> SinkWriter {
        SinkWriter(self.clone())
    }
}

/// Installs the global subscriber. Filter: `CHIP_REMOTE_LOG` (default `info`).
pub fn init(log_dir: &Path) -> LogRing {
    let ring = LogRing::default();
    let sink = Sink {
        file: Arc::new(Mutex::new(RotatingFile::new(
            log_dir.join(LOG_FILE),
            LOG_MAX_BYTES,
        ))),
        ring: ring.clone(),
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("CHIP_REMOTE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
        // stderr is a no-op in the windowless release build; handy under `cargo run`.
        .with_writer(sink.and(io::stderr))
        .try_init();
    ring
}

/// Log-friendly view of a WS message: JSON string fields longer than 80 chars are cut,
/// and the whole line is capped at 1000 chars.
pub fn log_view(text: &str) -> String {
    const MAX_FIELD: usize = 80;
    const MAX_TOTAL: usize = 1000;

    fn cut(s: &str, max: usize) -> Option<String> {
        let n = s.chars().count();
        (n > max).then(|| {
            let head: String = s.chars().take(max).collect();
            format!("{head}…(+{})", n - max)
        })
    }
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                if let Some(c) = cut(s, MAX_FIELD) {
                    *s = c;
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            serde_json::Value::Object(o) => o.values_mut().for_each(walk),
            _ => {}
        }
    }

    let line = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(mut v) => {
            walk(&mut v);
            v.to_string()
        }
        Err(_) => text.to_string(),
    };
    cut(&line, MAX_TOTAL).unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_last_lines() {
        let r = LogRing::default();
        for i in 0..5 {
            r.push(format!("{i}\n"), 3);
        }
        assert_eq!(r.snapshot(), "2\n3\n4\n");
    }

    #[test]
    fn rotates_at_max_bytes() {
        let dir = std::env::temp_dir().join(format!("chip-remote-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("agent.log");
        let mut f = RotatingFile::new(p.clone(), 10);
        f.write_line(b"aaaaaa\n").unwrap();
        f.write_line(b"bbbbbb\n").unwrap(); // 14 > 10 -> rotate first
        f.write_line(b"cc\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bbbbbb\ncc\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("agent.log.1")).unwrap(),
            "aaaaaa\n"
        );
        // Reopening continues with the existing size.
        drop(f);
        let mut f = RotatingFile::new(p.clone(), 10);
        f.write_line(b"d\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "d\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("agent.log.1")).unwrap(),
            "bbbbbb\ncc\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_view_truncates_long_fields() {
        let long = "あ".repeat(100);
        let v = log_view(&format!(
            r#"{{"type":"chip.new","chip":{{"task_id":"task_1","tldr":"{long}"}}}}"#
        ));
        assert!(v.contains("task_1"));
        assert!(v.contains("…(+20)"));
        assert!(v.chars().count() < 200);
        let raw = "x".repeat(2000);
        assert_eq!(
            log_view(&raw).chars().count(),
            1000 + "…(+1000)".chars().count()
        );
    }
}
