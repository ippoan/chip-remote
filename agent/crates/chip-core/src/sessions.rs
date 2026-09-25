//! Claude desktop's per-session files — the agent's own chip source.
//!
//! Claude desktop keeps one JSON per Code session at
//! `%APPDATA%\Claude\claude-code-sessions\<id>\<id>\local_<uuid>.json` and rewrites it
//! often. The fields used here (everything else is ignored):
//!
//! | field | meaning |
//! |---|---|
//! | `title` | session title, exactly as shown in the sidebar |
//! | `cliSessionId` | Claude Code `session_id` (what the hooks send) |
//! | `cwd` | Windows path, or `/home/...` for sessions on an SSH host |
//! | `isArchived` | archived sessions are treated as having no chips |
//! | `backgroundTaskSuggestions` | pending chips `[{id, title, tldr, prompt, createdAt}]` (absent when none) |
//! | `resolvedBackgroundTaskSuggestions` | `{ "task_x": "started_notified" \| "dismissed" }` |
//!
//! [`parse_session`] reads one file defensively (a partial write is an error → the
//! caller retries next round); [`diff`] turns two snapshots of the same file into
//! [`ChipEvent`]s; [`SessionIndex`] answers "where does this task stand" across all
//! files (used to close Worker chips that were resolved while nobody reported it).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// One chip waiting in a session (`backgroundTaskSuggestions[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChip {
    pub task_id: String,
    pub title: String,
    pub tldr: String,
    /// `createdAt` (ms since epoch; a string in the file).
    pub created_at_ms: Option<i64>,
}

/// How a chip left the pending list (`resolvedBackgroundTaskSuggestions` value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// `started_notified`: started (from the PC or through the agent).
    Started,
    /// `dismissed`.
    Dismissed,
    /// Anything else Claude desktop may write in the future.
    Other(String),
}

impl Resolution {
    pub fn from_wire(s: &str) -> Resolution {
        match s {
            "started_notified" => Resolution::Started,
            "dismissed" => Resolution::Dismissed,
            other => Resolution::Other(other.to_string()),
        }
    }
}

/// The chip-related part of one session file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionChips {
    pub session_title: String,
    /// `cliSessionId` (None when missing / empty).
    pub cli_session_id: Option<String>,
    pub cwd: String,
    pub archived: bool,
    /// Only chips with a well-formed id ([`is_valid_task_id`]).
    pub pending: Vec<PendingChip>,
    pub resolved: HashMap<String, Resolution>,
}

impl SessionChips {
    /// Pending chips that count: none while the session is archived.
    pub fn active_pending(&self) -> &[PendingChip] {
        if self.archived {
            &[]
        } else {
            &self.pending
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    /// Not JSON (e.g. caught mid-write).
    Json(serde_json::Error),
    /// JSON, but not an object.
    NotAnObject,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Json(e) => write!(f, "invalid JSON: {e}"),
            ParseError::NotAnObject => f.write_str("not a JSON object"),
        }
    }
}

impl std::error::Error for ParseError {}

/// `task_` followed by ASCII letters / digits. The id goes into a URL path
/// (`DELETE /v1/chips/<id>`), so anything else is skipped.
pub fn is_valid_task_id(id: &str) -> bool {
    id.strip_prefix("task_")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// True for the files [`parse_session`] understands (`local_*.json`); the directory
/// also holds `scheduled-tasks.json`, `backlog/tasks.json`, ...
pub fn is_session_file_name(name: &str) -> bool {
    name.starts_with("local_") && name.ends_with(".json")
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn millis(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Parses one session file. Missing / oddly typed fields fall back to defaults;
/// malformed chip entries are skipped.
pub fn parse_session(text: &str) -> Result<SessionChips, ParseError> {
    let v: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(ParseError::Json)?;
    if !v.is_object() {
        return Err(ParseError::NotAnObject);
    }
    let cli = str_field(&v, "cliSessionId");
    let pending = v
        .get("backgroundTaskSuggestions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let task_id = s.get("id")?.as_str()?;
                    if !is_valid_task_id(task_id) {
                        return None;
                    }
                    Some(PendingChip {
                        task_id: task_id.to_string(),
                        title: str_field(s, "title"),
                        tldr: str_field(s, "tldr"),
                        created_at_ms: millis(s.get("createdAt")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let resolved = v
        .get("resolvedBackgroundTaskSuggestions")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, r)| Some((k.clone(), Resolution::from_wire(r.as_str()?))))
                .collect()
        })
        .unwrap_or_default();
    Ok(SessionChips {
        session_title: str_field(&v, "title"),
        cli_session_id: (!cli.is_empty()).then_some(cli),
        cwd: str_field(&v, "cwd"),
        archived: v
            .get("isArchived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pending,
        resolved,
    })
}

/// A change between two snapshots of one session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipEvent {
    /// A pending chip that was not there before (every one on the first snapshot).
    Appeared {
        chip: PendingChip,
        session_title: String,
        cli_session_id: Option<String>,
        cwd: String,
    },
    /// Left the pending list with a resolved entry (or a new resolved entry showed up
    /// for a chip that came and went between two snapshots).
    Resolved {
        task_id: String,
        resolution: Resolution,
    },
    /// Left the pending list without a resolved entry (also: file deleted, session
    /// archived).
    Vanished { task_id: String },
}

/// Events between `prev` (None: first time this file is seen) and `cur` (None: the file
/// is gone). Archived sessions count as having no pending chips. Resolved entries
/// that already existed in the first snapshot are history, not events.
pub fn diff(prev: Option<&SessionChips>, cur: Option<&SessionChips>) -> Vec<ChipEvent> {
    let prev_pending = prev.map_or(&[][..], |p| p.active_pending());
    let cur_pending = cur.map_or(&[][..], |c| c.active_pending());
    let mut out = Vec::new();

    if let Some(c) = cur {
        for chip in cur_pending {
            if !prev_pending.iter().any(|p| p.task_id == chip.task_id) {
                out.push(ChipEvent::Appeared {
                    chip: chip.clone(),
                    session_title: c.session_title.clone(),
                    cli_session_id: c.cli_session_id.clone(),
                    cwd: c.cwd.clone(),
                });
            }
        }
    }
    for p in prev_pending {
        if cur_pending.iter().any(|c| c.task_id == p.task_id) {
            continue;
        }
        match cur.and_then(|c| c.resolved.get(&p.task_id)) {
            Some(r) => out.push(ChipEvent::Resolved {
                task_id: p.task_id.clone(),
                resolution: r.clone(),
            }),
            None => out.push(ChipEvent::Vanished {
                task_id: p.task_id.clone(),
            }),
        }
    }
    // A chip that appeared and was resolved between two snapshots (only after the
    // first snapshot: older entries are history).
    if let (Some(p), Some(c)) = (prev, cur) {
        let mut late: Vec<_> = c
            .resolved
            .iter()
            .filter(|(id, _)| {
                is_valid_task_id(id)
                    && !p.resolved.contains_key(*id)
                    && !prev_pending.iter().any(|x| &x.task_id == *id)
                    && !cur_pending.iter().any(|x| &x.task_id == *id)
            })
            .collect();
        late.sort_by(|a, b| a.0.cmp(b.0));
        for (id, r) in late {
            out.push(ChipEvent::Resolved {
                task_id: id.clone(),
                resolution: r.clone(),
            });
        }
    }
    out
}

/// Where a task_id stands across every session file ([`SessionIndex::presence`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Pending in some (non-archived) session.
    Pending,
    /// Not pending anywhere, but listed in some `resolvedBackgroundTaskSuggestions`.
    Resolved(Resolution),
    /// In no session file at all (an archived session's pending chip counts as absent).
    Absent,
}

/// Pending / resolved task_ids of a set of session files (a whole sessions directory).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIndex {
    pending: HashSet<String>,
    resolved: HashMap<String, Resolution>,
}

impl SessionIndex {
    pub fn from_sessions<'a, I>(sessions: I) -> SessionIndex
    where
        I: IntoIterator<Item = &'a SessionChips>,
    {
        let mut idx = SessionIndex::default();
        for s in sessions {
            idx.add(s);
        }
        idx
    }

    pub fn add(&mut self, s: &SessionChips) {
        for c in s.active_pending() {
            self.pending.insert(c.task_id.clone());
        }
        for (id, r) in &s.resolved {
            self.resolved.entry(id.clone()).or_insert_with(|| r.clone());
        }
    }

    /// Pending anywhere wins over resolved elsewhere.
    pub fn presence(&self, task_id: &str) -> Presence {
        if self.pending.contains(task_id) {
            Presence::Pending
        } else if let Some(r) = self.resolved.get(task_id) {
            Presence::Resolved(r.clone())
        } else {
            Presence::Absent
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn resolved_len(&self) -> usize {
        self.resolved.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a real session file (made-up values; extra keys are ignored).
    const ONE_PENDING: &str = r#"{
      "sessionId": "local_00000000-0000-4000-8000-000000000001",
      "cliSessionId": "11111111-2222-4333-8444-555555555555",
      "title": "テスト用セッション",
      "cwd": "C:\\work\\repo",
      "isArchived": false,
      "lastActivityAt": 1790000000000,
      "model": "x",
      "backgroundTaskSuggestions": [
        {"id": "task_0a1b2c3d", "title": "Fix stale badge", "tldr": "README の badge が古い",
         "prompt": "long prompt", "createdAt": "1790000000123"}
      ],
      "resolvedBackgroundTaskSuggestions": {"task_99999999": "dismissed"}
    }"#;

    fn session(pending: &[&str], resolved: &[(&str, &str)]) -> SessionChips {
        SessionChips {
            session_title: "S".into(),
            cli_session_id: Some("cli".into()),
            cwd: "/home/claude/x".into(),
            archived: false,
            pending: pending
                .iter()
                .map(|id| PendingChip {
                    task_id: id.to_string(),
                    title: format!("title {id}"),
                    tldr: String::new(),
                    created_at_ms: None,
                })
                .collect(),
            resolved: resolved
                .iter()
                .map(|(k, v)| (k.to_string(), Resolution::from_wire(v)))
                .collect(),
        }
    }

    #[test]
    fn parses_a_session_file() {
        let s = parse_session(ONE_PENDING).unwrap();
        assert_eq!(s.session_title, "テスト用セッション");
        assert_eq!(
            s.cli_session_id.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        assert_eq!(s.cwd, "C:\\work\\repo");
        assert!(!s.archived);
        assert_eq!(
            s.pending,
            vec![PendingChip {
                task_id: "task_0a1b2c3d".into(),
                title: "Fix stale badge".into(),
                tldr: "README の badge が古い".into(),
                created_at_ms: Some(1790000000123),
            }]
        );
        assert_eq!(s.resolved["task_99999999"], Resolution::Dismissed);
    }

    #[test]
    fn missing_fields_default_and_bad_entries_are_skipped() {
        let s = parse_session(
            r#"{"backgroundTaskSuggestions":[
                {"id":"task_1"},
                {"id":"task_../x","title":"t"},
                {"id":7},
                {"title":"no id"},
                "junk",
                {"id":"task_2","title":"T","createdAt":1790000000000}
            ],
            "resolvedBackgroundTaskSuggestions":{"task_a":"started_notified","task_b":"weird","task_c":3},
            "isArchived":"yes","cliSessionId":""}"#,
        )
        .unwrap();
        assert_eq!(s.session_title, "");
        assert_eq!(s.cli_session_id, None);
        assert!(!s.archived, "non-bool isArchived counts as false");
        let ids: Vec<_> = s.pending.iter().map(|c| c.task_id.as_str()).collect();
        assert_eq!(ids, vec!["task_1", "task_2"]);
        assert_eq!(s.pending[0].title, "");
        assert_eq!(s.pending[1].created_at_ms, Some(1790000000000));
        assert_eq!(s.resolved.len(), 2);
        assert_eq!(s.resolved["task_a"], Resolution::Started);
        assert_eq!(s.resolved["task_b"], Resolution::Other("weird".into()));
        // A file without any chip keys (the common case).
        let empty = parse_session(r#"{"title":"x","isArchived":true}"#).unwrap();
        assert!(empty.pending.is_empty() && empty.resolved.is_empty() && empty.archived);
    }

    #[test]
    fn partial_writes_and_non_objects_are_errors() {
        let cut = &ONE_PENDING[..ONE_PENDING.len() / 2];
        assert!(matches!(parse_session(cut), Err(ParseError::Json(_))));
        assert!(matches!(parse_session(""), Err(ParseError::Json(_))));
        assert!(matches!(
            parse_session("[1,2]"),
            Err(ParseError::NotAnObject)
        ));
        assert!(parse_session("\u{feff}{}").is_ok());
    }

    #[test]
    fn task_id_and_file_name_filters() {
        assert!(is_valid_task_id("task_12d25b98"));
        assert!(is_valid_task_id("task_ABC1"));
        assert!(!is_valid_task_id("task_"));
        assert!(!is_valid_task_id("task_a/b"));
        assert!(!is_valid_task_id("x_1"));
        assert!(is_session_file_name("local_0000.json"));
        assert!(!is_session_file_name("scheduled-tasks.json"));
        assert!(!is_session_file_name("tasks.json"));
    }

    #[test]
    fn first_snapshot_reports_pending_but_not_history() {
        let cur = session(&["task_1", "task_2"], &[("task_0", "dismissed")]);
        let ev = diff(None, Some(&cur));
        assert_eq!(ev.len(), 2);
        match &ev[0] {
            ChipEvent::Appeared {
                chip,
                session_title,
                cli_session_id,
                cwd,
            } => {
                assert_eq!(chip.task_id, "task_1");
                assert_eq!(chip.title, "title task_1");
                assert_eq!(session_title, "S");
                assert_eq!(cli_session_id.as_deref(), Some("cli"));
                assert_eq!(cwd, "/home/claude/x");
            }
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn appeared_resolved_vanished() {
        let a = session(&["task_1", "task_2", "task_3"], &[]);
        assert!(diff(Some(&a), Some(&a)).is_empty(), "no change, no events");
        let b = session(
            &["task_3", "task_4"],
            &[("task_1", "started_notified"), ("task_9", "dismissed")],
        );
        let ev = diff(Some(&a), Some(&b));
        assert_eq!(
            ev,
            vec![
                ChipEvent::Appeared {
                    chip: b.pending[1].clone(),
                    session_title: "S".into(),
                    cli_session_id: Some("cli".into()),
                    cwd: "/home/claude/x".into(),
                },
                ChipEvent::Resolved {
                    task_id: "task_1".into(),
                    resolution: Resolution::Started
                },
                ChipEvent::Vanished {
                    task_id: "task_2".into()
                },
                // task_9 came and went between the two snapshots.
                ChipEvent::Resolved {
                    task_id: "task_9".into(),
                    resolution: Resolution::Dismissed
                },
            ]
        );
    }

    #[test]
    fn archiving_or_deleting_withdraws_pending_chips() {
        let a = session(&["task_1"], &[]);
        let mut archived = a.clone();
        archived.archived = true;
        assert_eq!(
            diff(Some(&a), Some(&archived)),
            vec![ChipEvent::Vanished {
                task_id: "task_1".into()
            }]
        );
        // Archived sessions report nothing on the first snapshot.
        assert!(diff(None, Some(&archived)).is_empty());
        assert_eq!(
            diff(Some(&a), None),
            vec![ChipEvent::Vanished {
                task_id: "task_1".into()
            }]
        );
        // Un-archiving brings the chip back.
        assert_eq!(diff(Some(&archived), Some(&a)).len(), 1);
    }

    #[test]
    fn index_presence_across_files() {
        let a = session(&["task_1"], &[("task_2", "dismissed")]);
        let b = session(&["task_2"], &[("task_3", "started_notified")]);
        let mut archived = session(&["task_4"], &[]);
        archived.archived = true;
        let idx = SessionIndex::from_sessions([&a, &b, &archived]);
        assert_eq!(idx.presence("task_1"), Presence::Pending);
        assert_eq!(
            idx.presence("task_2"),
            Presence::Pending,
            "pending in one file wins over resolved in another"
        );
        assert_eq!(
            idx.presence("task_3"),
            Presence::Resolved(Resolution::Started)
        );
        assert_eq!(idx.presence("task_4"), Presence::Absent);
        assert_eq!(idx.presence("task_9"), Presence::Absent);
        assert_eq!((idx.pending_len(), idx.resolved_len()), (2, 2));
        assert_eq!(SessionIndex::default().presence("task_1"), Presence::Absent);
    }
}
