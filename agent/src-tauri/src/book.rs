//! What the session-file watcher knows about each pending chip, shared with the WS
//! session: an `action` for a known task_id uses the file's exact title / tldr and the
//! session title (so the UIA driver can bring that session's pane up).
//!
//! It also meets the two halves of [`crate::reconcile`]: the Worker's open chips from
//! the last `hello` and the watcher's [`SessionIndex`] (set only after a complete scan).
//! Whichever arrives second triggers the reconcile.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use chip_core::protocol::WireChip;
use chip_core::sessions::{Presence, SessionIndex};

use crate::reconcile::reconcile;

/// A pending chip as written by Claude desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEntry {
    pub session_title: String,
    pub title: String,
    pub tldr: String,
}

#[derive(Default)]
struct Inner {
    chips: HashMap<String, BookEntry>,
    /// task_ids the agent is pressing / has pressed for the phone. When such a chip then
    /// leaves the session file, the Worker already has the result (`done`); a DELETE
    /// would turn it into `withdrawn` and cancel the phone's result notification.
    acting: HashSet<String>,
    /// Chips that left the session file after our own action ([`ChipBook::take_acting`]):
    /// never DELETEd by the reconcile either.
    ours: HashSet<String>,
    /// All session files as of the watcher's last complete scan (None: no complete scan
    /// yet, or watching is off).
    index: Option<SessionIndex>,
    /// The Worker's open chips from the last `hello`, minus the ones already DELETEd.
    last_hello: Option<Vec<WireChip>>,
}

impl Inner {
    fn reconcile(&mut self, now_ms: i64) -> Vec<String> {
        let (Some(index), Some(hello)) = (&self.index, &mut self.last_hello) else {
            return Vec::new();
        };
        let ours: HashSet<String> = self.acting.union(&self.ours).cloned().collect();
        let stale = reconcile(hello, index, &ours, now_ms);
        hello.retain(|c| !stale.contains(&c.task_id));
        stale
    }
}

#[derive(Clone, Default)]
pub struct ChipBook(Arc<Mutex<Inner>>);

impl ChipBook {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn upsert(&self, task_id: &str, entry: BookEntry) {
        self.lock().chips.insert(task_id.to_string(), entry);
    }

    pub fn remove(&self, task_id: &str) {
        self.lock().chips.remove(task_id);
    }

    pub fn get(&self, task_id: &str) -> Option<BookEntry> {
        self.lock().chips.get(task_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.lock().chips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// An action for `task_id` is about to be performed.
    pub fn mark_acting(&self, task_id: &str) {
        self.lock().acting.insert(task_id.to_string());
    }

    /// The action failed: a later PC-side resolve should be reported again.
    pub fn unmark_acting(&self, task_id: &str) {
        self.lock().acting.remove(task_id);
    }

    /// True (once) when the chip left the file because of our own action.
    pub fn take_acting(&self, task_id: &str) -> bool {
        let mut g = self.lock();
        let ours = g.acting.remove(task_id);
        if ours {
            g.ours.insert(task_id.to_string());
        }
        ours
    }

    /// Pressed by the agent itself (in flight, or resolved by our own action).
    pub fn is_ours(&self, task_id: &str) -> bool {
        let g = self.lock();
        g.acting.contains(task_id) || g.ours.contains(task_id)
    }

    /// The watcher's view of every session file (None: not known / watching is off).
    pub fn set_index(&self, index: Option<SessionIndex>) {
        self.lock().index = index;
    }

    pub fn has_index(&self) -> bool {
        self.lock().index.is_some()
    }

    /// Where `task_id` stands in the session files; None until a complete scan.
    pub fn presence(&self, task_id: &str) -> Option<Presence> {
        self.lock().index.as_ref().map(|i| i.presence(task_id))
    }

    /// `hello`: remembers the Worker's open chips and returns the ones to DELETE now
    /// (none before the watcher's first complete scan: then [`Self::reconcile_last_hello`]
    /// runs once that scan is done).
    pub fn on_hello(&self, chips: Vec<WireChip>, now_ms: i64) -> Vec<String> {
        let mut g = self.lock();
        g.last_hello = Some(chips);
        g.reconcile(now_ms)
    }

    /// After a full scan: reconciles the last `hello` (if any) against the new index.
    pub fn reconcile_last_hello(&self, now_ms: i64) -> Vec<String> {
        self.lock().reconcile(now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_core::sessions::parse_session;

    #[test]
    fn entries_and_acting_marks() {
        let b = ChipBook::default();
        assert!(b.is_empty());
        let e = BookEntry {
            session_title: "S".into(),
            title: "T".into(),
            tldr: "D".into(),
        };
        b.upsert("task_1", e.clone());
        assert_eq!(b.clone().get("task_1"), Some(e));
        b.remove("task_1");
        assert_eq!(b.get("task_1"), None);

        b.mark_acting("task_2");
        assert!(b.take_acting("task_2"));
        assert!(!b.take_acting("task_2"), "consumed once");
        b.mark_acting("task_3");
        b.unmark_acting("task_3");
        assert!(!b.take_acting("task_3"));
        assert!(b.is_ours("task_2"), "resolved by our own action stays ours");
        assert!(!b.is_ours("task_3"));
        b.mark_acting("task_4");
        assert!(b.is_ours("task_4"), "in flight");
    }

    fn wire(id: &str, status: &str) -> WireChip {
        WireChip {
            task_id: id.into(),
            title: String::new(),
            tldr: String::new(),
            status: status.into(),
            created_at: 1,
        }
    }

    fn index() -> SessionIndex {
        let s = parse_session(
            r#"{"backgroundTaskSuggestions":[{"id":"task_p"}],
                "resolvedBackgroundTaskSuggestions":{"task_r":"dismissed","task_m":"started_notified"}}"#,
        )
        .unwrap();
        SessionIndex::from_sessions([&s])
    }

    #[test]
    fn hello_before_the_first_scan_is_reconciled_after_it() {
        let b = ChipBook::default();
        let hello = vec![
            wire("task_p", "notified"),
            wire("task_r", "failed"),
            wire("task_m", "failed"),
        ];
        assert!(b.presence("task_r").is_none());
        assert!(b.on_hello(hello.clone(), 100).is_empty(), "no index yet");
        b.mark_acting("task_m");
        assert!(b.take_acting("task_m"));
        b.set_index(Some(index()));
        assert_eq!(
            b.presence("task_r"),
            Some(Presence::Resolved(
                chip_core::sessions::Resolution::Dismissed
            ))
        );
        assert_eq!(b.reconcile_last_hello(100), vec!["task_r"]);
        assert!(b.reconcile_last_hello(100).is_empty(), "DELETEd once");
        // A later hello is reconciled right away.
        assert_eq!(b.on_hello(hello, 100), vec!["task_r"]);
        b.set_index(None);
        assert!(!b.has_index());
        assert!(b.on_hello(vec![wire("task_r", "failed")], 100).is_empty());
    }
}
