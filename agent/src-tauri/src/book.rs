//! What the session-file watcher knows about each pending chip, shared with the WS
//! session: an `action` for a known task_id uses the file's exact title / tldr and the
//! session title (so the UIA driver can bring that session's pane up).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

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
        self.lock().acting.remove(task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
