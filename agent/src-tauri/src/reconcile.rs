//! Closes Worker chips that were resolved on the PC while nobody reported it (before
//! the agent ran, hook-only chips, a missed file change, ...). Without this they stay
//! open forever: the phone lists them, an action finds nothing in the UI
//! (`chip_not_found` → `failed`, which stays open by design) and they can never be
//! cleared.
//!
//! The Worker's open chips (`hello`) are compared with the session files
//! ([`SessionIndex`], only after the watcher's first complete scan):
//!
//! | chip | decision |
//! |---|---|
//! | Worker status `acting` (an action is in flight) | keep |
//! | pressed by the agent itself (in flight or resolved by our own action) | keep (the Worker has / gets the result; a DELETE would cancel the phone's result notification) |
//! | pending in some session file | keep |
//! | resolved in some session file | DELETE |
//! | in no session file, created ≥ [`ABSENT_GRACE_MS`] ago | DELETE |
//! | in no session file, younger (or no `created_at`) | keep (a hook may report a chip before Claude desktop writes the file) |

use std::collections::HashSet;

use chip_core::protocol::WireChip;
use chip_core::sessions::{is_valid_task_id, Presence, SessionIndex};

/// How long a chip found in no session file is left alone.
pub const ABSENT_GRACE_MS: i64 = 10 * 60 * 1000;

/// task_ids of `worker_open` to `DELETE /v1/chips/:task_id` (see the module docs).
/// `ours`: chips the agent pressed itself.
pub fn reconcile(
    worker_open: &[WireChip],
    index: &SessionIndex,
    ours: &HashSet<String>,
    now_ms: i64,
) -> Vec<String> {
    let mut out = Vec::new();
    for c in worker_open {
        let id = c.task_id.as_str();
        if !is_valid_task_id(id)
            || !matches!(
                c.status.as_str(),
                "" | "located_pending" | "notified" | "failed"
            )
            || ours.contains(id)
            || out.iter().any(|x| x == id)
        {
            continue;
        }
        let stale = match index.presence(id) {
            Presence::Pending => false,
            Presence::Resolved(_) => true,
            Presence::Absent => c.created_at > 0 && now_ms - c.created_at >= ABSENT_GRACE_MS,
        };
        if stale {
            out.push(c.task_id.clone());
        }
    }
    out
}

/// Milliseconds since the Unix epoch (the Worker's `created_at` clock).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_core::sessions::parse_session;

    const NOW: i64 = 1_790_000_000_000;

    fn chip(id: &str, status: &str, created_at: i64) -> WireChip {
        WireChip {
            task_id: id.into(),
            title: format!("title {id}"),
            tldr: String::new(),
            status: status.into(),
            created_at,
        }
    }

    fn index() -> SessionIndex {
        let s = parse_session(
            r#"{"title":"S","backgroundTaskSuggestions":[{"id":"task_pend","title":"P"}],
                "resolvedBackgroundTaskSuggestions":{"task_done":"started_notified","task_gone":"dismissed","task_mine":"started_notified"}}"#,
        )
        .unwrap();
        SessionIndex::from_sessions([&s])
    }

    fn run(chips: &[WireChip], ours: &[&str]) -> Vec<String> {
        let ours = ours.iter().map(|s| s.to_string()).collect();
        reconcile(chips, &index(), &ours, NOW)
    }

    #[test]
    fn resolved_on_the_pc_is_deleted() {
        let chips = [
            chip("task_done", "notified", NOW),
            chip("task_gone", "failed", NOW - 1),
            chip("task_done", "notified", NOW),
        ];
        assert_eq!(run(&chips, &[]), vec!["task_done", "task_gone"]);
    }

    #[test]
    fn pending_is_kept() {
        let old = NOW - 24 * 3600 * 1000;
        assert!(run(&[chip("task_pend", "failed", old)], &[]).is_empty());
        assert!(run(&[chip("task_pend", "located_pending", old)], &[]).is_empty());
    }

    #[test]
    fn absent_young_is_kept_and_absent_old_is_deleted() {
        let young = chip("task_young", "notified", NOW - ABSENT_GRACE_MS + 1);
        let old = chip("task_old", "notified", NOW - ABSENT_GRACE_MS);
        let unknown_age = chip("task_noage", "notified", 0);
        assert_eq!(run(&[young, old, unknown_age], &[]), vec!["task_old"]);
    }

    #[test]
    fn acting_is_kept() {
        let chips = [
            chip("task_done", "acting", NOW),
            chip("task_old", "acting", NOW - ABSENT_GRACE_MS * 10),
        ];
        assert!(run(&chips, &[]).is_empty());
    }

    #[test]
    fn resolved_by_our_own_action_is_kept() {
        let chips = [
            chip("task_mine", "failed", NOW),
            chip("task_old", "notified", 1),
        ];
        assert!(run(&chips, &["task_mine", "task_old"]).is_empty());
        assert_eq!(run(&chips, &[]), vec!["task_mine", "task_old"]);
    }

    #[test]
    fn closed_statuses_and_bad_ids_are_ignored() {
        let chips = [
            chip("task_done", "done", NOW),
            chip("task_gone", "withdrawn", NOW),
            chip("task_x/../y", "notified", 1),
            chip("", "notified", 1),
        ];
        assert!(run(&chips, &[]).is_empty());
        // A missing status (older Worker) counts as open.
        assert_eq!(run(&[chip("task_done", "", NOW)], &[]), vec!["task_done"]);
    }
}
