//! Read-only check of Claude desktop's session files: prints COUNTS only (never
//! titles, tldrs or prompts).
//!
//! ```powershell
//! cargo run -p chip-core --example session_counts [-- <sessions dir>]
//! ```

use std::path::{Path, PathBuf};

use chip_core::sessions::{is_session_file_name, parse_session, Resolution};

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => walk(&p, depth - 1, out),
            Ok(t) if t.is_file() && is_session_file_name(&e.file_name().to_string_lossy()) => {
                out.push(p)
            }
            _ => {}
        }
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let appdata = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_default();
            chip_core::Config::default().sessions_dir(&appdata)
        });
    let mut files = Vec::new();
    walk(&dir, 4, &mut files);
    let (mut parsed, mut failed, mut archived) = (0, 0, 0);
    let (mut pending, mut pending_active, mut started, mut dismissed, mut other) = (0, 0, 0, 0, 0);
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            failed += 1;
            continue;
        };
        match parse_session(&text) {
            Ok(s) => {
                parsed += 1;
                archived += usize::from(s.archived);
                pending += s.pending.len();
                pending_active += s.active_pending().len();
                for r in s.resolved.values() {
                    match r {
                        Resolution::Started => started += 1,
                        Resolution::Dismissed => dismissed += 1,
                        Resolution::Other(_) => other += 1,
                    }
                }
            }
            Err(_) => failed += 1,
        }
    }
    println!("session files: {}", files.len());
    println!("parsed: {parsed}  failed: {failed}  archived: {archived}");
    println!("pending chips: {pending} (in non-archived sessions: {pending_active})");
    println!("resolved: started={started} dismissed={dismissed} other={other}");
}
