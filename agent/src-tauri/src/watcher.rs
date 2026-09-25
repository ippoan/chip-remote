//! Watches Claude desktop's session files and reports their chips to the Worker, so a
//! chip is known even when no hook ran (docs/PROTOCOL.md "session files").
//!
//! - every `poll` (2 s): stat every `local_*.json` under `sessionsDir`; only files whose
//!   (mtime, len) changed are re-read. A file that fails to parse (caught mid-write) keeps
//!   its old snapshot and is retried next round.
//! - the first round is a full scan: chips already pending are reported too (the Worker
//!   answers 201 new / 200 known).
//! - `Appeared` → `POST /v1/chips`; `Resolved` / `Vanished` → `DELETE /v1/chips/:id`,
//!   except when the chip left because the agent pressed it for the phone (the Worker
//!   already has the result).
//! - [`ChipBook`] gets the file's exact title / tldr and the session title for actions.
//! - after every complete round (directory present and fully listed, every file parsed
//!   at least once) [`ChipBook`] gets a [`SessionIndex`] of all files; the first complete
//!   round of a watch (the full scan) also reconciles the Worker's last `hello` against
//!   it ([`crate::reconcile`]). Watching off / another directory → no index.
//! - config.json is re-read every round (`watchSessions`, `sessionsDir`, url / token).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chip_core::sessions::{
    diff, is_session_file_name, parse_session, ChipEvent, SessionChips, SessionIndex,
};
use chip_core::Config;

use crate::book::{BookEntry, ChipBook};
use crate::reconcile::now_ms;
use crate::reporter::{self, Job, NewChip, RetryPolicy, Withdrawer};

/// `<id>\<id>\local_*.json`; a little slack for layout changes.
const MAX_DEPTH: usize = 4;
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

type Stamp = (Option<SystemTime>, u64);

struct FileEntry {
    stamp: Stamp,
    chips: SessionChips,
}

/// Result of one [`SessionWatch::poll`].
#[derive(Debug, Default)]
pub struct Poll {
    pub events: Vec<ChipEvent>,
    /// Pending chips (task_id, entry) of every file re-read this round.
    pub refreshed: Vec<(String, BookEntry)>,
    /// Session files seen / parsed this round / failed to read or parse this round.
    pub files: usize,
    pub parsed: usize,
    pub failed: usize,
    /// The directory exists, was listed completely and every file in it has a snapshot:
    /// [`SessionWatch::index`] reflects all sessions (a chip missing from it really is
    /// in no file).
    pub complete: bool,
}

/// Snapshot of one sessions directory (pure file I/O; no network).
pub struct SessionWatch {
    dir: PathBuf,
    files: HashMap<PathBuf, FileEntry>,
    warned: HashSet<PathBuf>,
    dir_missing_logged: bool,
    /// Had a complete round ([`Poll::complete`]).
    ready: bool,
}

/// Collects `local_*.json` files; false when some directory could not be read (then a
/// missing file must not be taken as deleted).
fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, Stamp)>) -> bool {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    let mut complete = true;
    for e in rd {
        let Ok(e) = e else {
            complete = false;
            continue;
        };
        let Ok(t) = e.file_type() else {
            complete = false;
            continue;
        };
        if t.is_dir() {
            if depth > 0 {
                complete &= walk(&e.path(), depth - 1, out);
            }
        } else if t.is_file() && is_session_file_name(&e.file_name().to_string_lossy()) {
            match e.metadata() {
                Ok(m) => out.push((e.path(), (m.modified().ok(), m.len()))),
                Err(_) => complete = false,
            }
        }
    }
    complete
}

impl SessionWatch {
    pub fn new(dir: PathBuf) -> SessionWatch {
        SessionWatch {
            dir,
            files: HashMap::new(),
            warned: HashSet::new(),
            dir_missing_logged: false,
            ready: false,
        }
    }

    /// Pending / resolved chips of every file as of the last poll.
    pub fn index(&self) -> SessionIndex {
        SessionIndex::from_sessions(self.files.values().map(|e| &e.chips))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn poll(&mut self) -> Poll {
        let mut out = Poll::default();
        if !self.dir.is_dir() {
            if !self.dir_missing_logged {
                tracing::info!(
                    "sessions: {} does not exist (yet); waiting",
                    self.dir.display()
                );
                self.dir_missing_logged = true;
            }
            return out;
        }
        self.dir_missing_logged = false;
        let mut seen = Vec::new();
        let complete = walk(&self.dir, MAX_DEPTH, &mut seen);
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        out.files = seen.len();

        for (path, stamp) in &seen {
            let prev = self.files.get(path);
            if prev.is_some_and(|e| e.stamp == *stamp) {
                continue;
            }
            let parsed = std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_session(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(chips) => {
                    out.parsed += 1;
                    self.warned.remove(path);
                    out.events
                        .extend(diff(prev.map(|e| &e.chips), Some(&chips)));
                    for c in chips.active_pending() {
                        out.refreshed.push((
                            c.task_id.clone(),
                            BookEntry {
                                session_title: chips.session_title.clone(),
                                title: c.title.clone(),
                                tldr: c.tldr.clone(),
                            },
                        ));
                    }
                    self.files.insert(
                        path.clone(),
                        FileEntry {
                            stamp: *stamp,
                            chips,
                        },
                    );
                }
                Err(e) => {
                    // Probably caught mid-write: keep the old snapshot, retry next round.
                    out.failed += 1;
                    if self.warned.insert(path.clone()) {
                        tracing::debug!("sessions: skipped {} this round: {e}", path.display());
                    }
                }
            }
        }

        if complete {
            let present: HashSet<&PathBuf> = seen.iter().map(|(p, _)| p).collect();
            let gone: Vec<PathBuf> = self
                .files
                .keys()
                .filter(|p| !present.contains(p))
                .cloned()
                .collect();
            for p in gone {
                if let Some(e) = self.files.remove(&p) {
                    out.events.extend(diff(Some(&e.chips), None));
                }
                self.warned.remove(&p);
            }
            out.complete = seen.iter().all(|(p, _)| self.files.contains_key(p));
        }
        out
    }
}

pub struct WatchEnv {
    pub config_path: PathBuf,
    /// `%APPDATA%` (base of the default sessions dir).
    pub appdata: PathBuf,
    pub book: ChipBook,
    pub poll: Duration,
    pub retry: RetryPolicy,
    /// DELETEs decided by the reconcile.
    pub withdraw: Withdrawer,
}

/// Applies one round's events: book updates and Worker reports (in order).
async fn apply(poll: Poll, env: &WatchEnv, cfg: &Config, client: &reqwest::Client) {
    for (id, entry) in poll.refreshed {
        env.book.upsert(&id, entry);
    }
    let host = reporter::host_name(cfg);
    let load = || Config::load(&env.config_path).ok();
    for ev in poll.events {
        let job = match ev {
            ChipEvent::Appeared {
                chip,
                session_title: _,
                cli_session_id,
                cwd,
            } => {
                tracing::info!("sessions: chip {} appeared", chip.task_id);
                Job::Post(NewChip {
                    task_id: chip.task_id,
                    title: chip.title,
                    tldr: chip.tldr,
                    cwd,
                    host: host.clone(),
                    session_id: cli_session_id,
                })
            }
            ChipEvent::Resolved {
                task_id,
                resolution,
            } => {
                env.book.remove(&task_id);
                if env.book.take_acting(&task_id) {
                    tracing::info!(
                        "sessions: chip {task_id} resolved by our own action ({resolution:?})"
                    );
                    continue;
                }
                tracing::info!("sessions: chip {task_id} resolved ({resolution:?})");
                Job::Delete(task_id)
            }
            ChipEvent::Vanished { task_id } => {
                env.book.remove(&task_id);
                if env.book.take_acting(&task_id) {
                    tracing::info!("sessions: chip {task_id} gone after our own action");
                    continue;
                }
                tracing::info!("sessions: chip {task_id} gone");
                Job::Delete(task_id)
            }
        };
        reporter::deliver(client, load, &job, env.retry).await;
    }
}

/// The watcher's main loop. Never returns.
pub async fn run_forever(env: WatchEnv) {
    let client = reporter::client();
    let mut watch: Option<SessionWatch> = None;
    loop {
        match Config::load(&env.config_path) {
            // The WS loop already reports a missing / broken config.
            Err(_) => {}
            Ok(cfg) if !cfg.watch_sessions => {
                if watch.take().is_some() {
                    tracing::info!("sessions: watching turned off (watchSessions=false)");
                    env.book.set_index(None);
                }
            }
            Ok(cfg) => {
                let dir = cfg.sessions_dir(&env.appdata);
                let mut w = match watch.take() {
                    Some(w) if w.dir() == dir => w,
                    _ => {
                        tracing::info!("sessions: watching {}", dir.display());
                        // Another directory: the old index says nothing about it.
                        env.book.set_index(None);
                        SessionWatch::new(dir)
                    }
                };
                let first = w.files.is_empty();
                let joined = tokio::task::spawn_blocking(move || {
                    let p = w.poll();
                    (w, p)
                })
                .await;
                match joined {
                    Ok((mut w, poll)) => {
                        if first && poll.files > 0 {
                            tracing::info!(
                                "sessions: full scan: {} file(s), {} parsed, {} skipped, {} pending chip(s)",
                                poll.files,
                                poll.parsed,
                                poll.failed,
                                poll.refreshed.len()
                            );
                        }
                        if poll.complete {
                            // Before apply(): its deliveries may sit in a retry backoff.
                            let full_scan = !w.ready;
                            w.ready = true;
                            let index = w.index();
                            if full_scan {
                                tracing::info!(
                                    "sessions: index ready: {} pending, {} resolved",
                                    index.pending_len(),
                                    index.resolved_len()
                                );
                            }
                            env.book.set_index(Some(index));
                            if full_scan {
                                for id in env.book.reconcile_last_hello(now_ms()) {
                                    env.withdraw.withdraw(
                                        id,
                                        "open on the Worker, resolved / gone on the PC",
                                    );
                                }
                            }
                        }
                        watch = Some(w);
                        apply(poll, &env, &cfg, &client).await;
                    }
                    Err(e) => tracing::warn!("sessions: scan task failed: {e}"),
                }
            }
        }
        tokio::time::sleep(env.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::fake_http::{self, FakeHttp};
    use chip_core::sessions::Presence;
    use serde_json::json;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!(
                "chip-remote-watch-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn session_json(title: &str, pending: &[(&str, &str)], resolved: &[(&str, &str)]) -> String {
        let pend: Vec<_> = pending
            .iter()
            .map(|(id, t)| json!({"id": id, "title": t, "tldr": format!("tldr of {t}"), "prompt": "p", "createdAt": "1790000000000"}))
            .collect();
        let res: serde_json::Map<String, serde_json::Value> = resolved
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        let mut v = json!({
            "sessionId": "local_x", "cliSessionId": "cli-1", "title": title,
            "cwd": "C:\\w", "isArchived": false, "lastActivityAt": 1,
            "resolvedBackgroundTaskSuggestions": res,
        });
        if !pend.is_empty() {
            v["backgroundTaskSuggestions"] = json!(pend);
        }
        v.to_string()
    }

    /// Writes `<dir>/<a>/<a>/<name>` (the real layout).
    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let d = dir.join("acct").join("org");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn poll_reports_changes_only_and_retries_partial_writes() {
        let tmp = TempDir::new("poll");
        let a = write(
            &tmp.0,
            "local_a.json",
            &session_json("S1", &[("task_1", "T1")], &[]),
        );
        write(
            &tmp.0,
            "local_b.json",
            &session_json("S2", &[], &[("task_0", "dismissed")]),
        );
        write(&tmp.0, "scheduled-tasks.json", "[]");
        let mut w = SessionWatch::new(tmp.0.clone());

        let p = w.poll();
        assert_eq!((p.files, p.parsed, p.failed), (2, 2, 0));
        assert!(p.complete);
        assert_eq!(p.events.len(), 1);
        assert!(
            matches!(&p.events[0], ChipEvent::Appeared { chip, .. } if chip.task_id == "task_1")
        );
        assert_eq!(
            p.refreshed,
            vec![(
                "task_1".to_string(),
                BookEntry {
                    session_title: "S1".into(),
                    title: "T1".into(),
                    tldr: "tldr of T1".into()
                }
            )]
        );

        // Nothing changed: nothing re-read.
        let p = w.poll();
        assert_eq!((p.parsed, p.events.len()), (0, 0));

        // Mid-write: skipped, old snapshot kept (no false "vanished").
        std::fs::write(&a, "{\"title\":\"S1\",\"backgroundTask").unwrap();
        let p = w.poll();
        assert_eq!((p.failed, p.events.len()), (1, 0));
        assert!(p.complete, "the old snapshot still counts");
        assert_eq!(w.index().presence("task_1"), Presence::Pending);

        // Complete write: task_1 started, task_2 new.
        std::fs::write(
            &a,
            session_json("S1", &[("task_2", "T2")], &[("task_1", "started_notified")]),
        )
        .unwrap();
        let p = w.poll();
        assert_eq!(p.events.len(), 2, "{:?}", p.events);
        assert!(
            matches!(&p.events[0], ChipEvent::Appeared { chip, .. } if chip.task_id == "task_2")
        );
        assert!(matches!(&p.events[1], ChipEvent::Resolved { task_id, .. } if task_id == "task_1"));

        // File deleted: its pending chip is gone.
        std::fs::remove_file(&a).unwrap();
        let p = w.poll();
        assert_eq!(
            p.events,
            vec![ChipEvent::Vanished {
                task_id: "task_2".into()
            }]
        );
    }

    #[test]
    fn missing_dir_is_quiet() {
        let tmp = TempDir::new("missing");
        let mut w = SessionWatch::new(tmp.0.join("nope"));
        let p = w.poll();
        assert_eq!((p.files, p.events.len()), (0, 0));
        assert!(
            !p.complete,
            "no directory: no index (everything would look absent)"
        );
    }

    /// A file never read yet (caught mid-write on the first round) leaves the index
    /// incomplete: its chips would look absent to the reconcile.
    #[test]
    fn index_is_complete_only_when_every_file_was_read() {
        let tmp = TempDir::new("complete");
        let a = write(&tmp.0, "local_a.json", "{\"title\":\"S1\",\"backgroundTask");
        write(
            &tmp.0,
            "local_b.json",
            &session_json("S2", &[("task_2", "T2")], &[("task_0", "dismissed")]),
        );
        let mut w = SessionWatch::new(tmp.0.clone());
        let p = w.poll();
        assert_eq!((p.parsed, p.failed), (1, 1));
        assert!(!p.complete);
        std::fs::write(&a, session_json("S1", &[("task_1", "T1")], &[])).unwrap();
        let p = w.poll();
        assert!(p.complete);
        let idx = w.index();
        assert_eq!(idx.presence("task_1"), Presence::Pending);
        assert_eq!(idx.presence("task_2"), Presence::Pending);
        assert_eq!(
            idx.presence("task_0"),
            Presence::Resolved(chip_core::sessions::Resolution::Dismissed)
        );
        // An empty (but present) directory is complete: nothing is pending anywhere.
        let empty = TempDir::new("empty");
        assert!(SessionWatch::new(empty.0.clone()).poll().complete);
    }

    fn env(tmp: &TempDir, srv: &FakeHttp, book: &ChipBook) -> WatchEnv {
        let cfg = tmp.0.join("config.json");
        std::fs::write(
            &cfg,
            json!({"url": srv.url, "token": "secret-token", "sessionsDir": tmp.0.join("sessions"), "host": "PC1"})
                .to_string(),
        )
        .unwrap();
        let retry = RetryPolicy {
            initial: Duration::from_millis(5),
            max: Duration::from_millis(20),
        };
        WatchEnv {
            config_path: cfg.clone(),
            appdata: tmp.0.clone(),
            book: book.clone(),
            poll: Duration::from_millis(20),
            retry,
            withdraw: Withdrawer::new(cfg, retry),
        }
    }

    async fn wait_requests(srv: &FakeHttp, n: usize) -> Vec<fake_http::Recorded> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let r = srv.requests();
                if r.len() >= n {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expected requests to the fake Worker")
    }

    #[tokio::test]
    async fn watcher_posts_and_deletes_and_fills_the_book() {
        let tmp = TempDir::new("run");
        let sessions = tmp.0.join("sessions");
        let f = write(
            &sessions,
            "local_a.json",
            &session_json("Session A", &[("task_1", "T1"), ("task_2", "T2")], &[]),
        );
        let srv = fake_http::start().await;
        // The first POST hits a dropped connection: retried.
        srv.script(&[0]);
        let book = ChipBook::default();
        let task = tokio::spawn(run_forever(env(&tmp, &srv, &book)));

        let r = wait_requests(&srv, 3).await;
        assert!(r
            .iter()
            .all(|x| x.method == "POST" && x.path == "/v1/chips"));
        assert!(r
            .iter()
            .all(|x| x.auth.as_deref() == Some("Bearer secret-token")));
        let body: serde_json::Value = serde_json::from_str(&r[2].body).unwrap();
        assert_eq!(
            body,
            json!({"task_id":"task_2","title":"T2","tldr":"tldr of T2","cwd":"C:\\w",
                   "host":"PC1","session_id":"cli-1"})
        );
        assert_eq!(book.get("task_1").unwrap().session_title, "Session A");

        // task_1 started by our own action (no DELETE), task_2 dismissed on the PC.
        book.mark_acting("task_1");
        std::fs::write(
            &f,
            session_json(
                "Session A",
                &[],
                &[("task_1", "started_notified"), ("task_2", "dismissed")],
            ),
        )
        .unwrap();
        let r = wait_requests(&srv, 4).await;
        assert_eq!(r[3].method, "DELETE");
        assert_eq!(r[3].path, "/v1/chips/task_2");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(srv.requests().len(), 4, "no DELETE for our own action");
        assert!(book.is_empty());
        assert!(book.is_ours("task_1"));
        assert_eq!(
            book.presence("task_2"),
            Some(Presence::Resolved(
                chip_core::sessions::Resolution::Dismissed
            )),
            "the book's index follows every complete round"
        );
        task.abort();
    }

    #[tokio::test]
    async fn watch_sessions_false_reports_nothing() {
        let tmp = TempDir::new("off");
        write(
            &tmp.0.join("sessions"),
            "local_a.json",
            &session_json("S", &[("task_1", "T1")], &[]),
        );
        let srv = fake_http::start().await;
        let e = env(&tmp, &srv, &ChipBook::default());
        let mut cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&e.config_path).unwrap()).unwrap();
        cfg["watchSessions"] = json!(false);
        std::fs::write(&e.config_path, cfg.to_string()).unwrap();
        let task = tokio::spawn(run_forever(e));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(srv.requests().is_empty());
        task.abort();
    }
}
