//! Lists the chips chip-uia sees in Claude desktop.
//!
//!   cargo run -p chip-uia --example probe               read-only (never moves a window)
//!   cargo run -p chip-uia --example probe -- --sessions also list the sidebar sessions
//!                                                       (read-only)
//!   cargo run -p chip-uia --example probe -- --raise    un-occlude Claude first (moves z-order)
//!
//! Labels / raiseWaitSec come from %APPDATA%\chip-remote\config.json when present
//! (url / token are not required here), else the built-in defaults.

use chip_core::Config;
use chip_uia::Uia;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn load_config() -> Config {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Config::default();
    };
    let path = Config::default_path(std::path::Path::new(&appdata));
    match std::fs::read_to_string(&path) {
        Ok(text) => match Config::from_json(&text) {
            Ok(c) => {
                println!("config: {}", path.display());
                c
            }
            Err(e) => {
                eprintln!("config: {}: {e} (using defaults)", path.display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

fn print_sidebar(uia: &Uia) {
    let t0 = Instant::now();
    match uia.sidebar_sessions() {
        Ok(list) => {
            println!();
            println!(
                "sidebar sessions: {} ({} ms)",
                list.len(),
                t0.elapsed().as_millis()
            );
            for (i, s) in list.iter().enumerate() {
                let title = s.title.as_deref().unwrap_or("<unparsed>");
                println!(
                    "  [{:>3}] {title}   <- status {:?}{}",
                    i + 1,
                    s.status,
                    if s.offscreen { " (offscreen)" } else { "" }
                );
                if s.title.is_none() {
                    println!("        name: {}", s.name);
                }
            }
        }
        Err(e) => eprintln!("sidebar sessions failed: {e}"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let raise = args.iter().any(|a| a == "--raise");
    let sessions = args.iter().any(|a| a == "--sessions");
    let cfg = load_config();
    let uia = match Uia::new(cfg.labels.clone()) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("UIA init failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match uia.window_info() {
        Ok(w) => println!(
            "window: \"{}\" hwnd={:#x} minimized={} foreground={}",
            w.title, w.hwnd, w.minimized, w.foreground
        ),
        Err(e) => {
            println!("Claude desktop is not running (no claude.exe with a main window): {e}");
            return ExitCode::FAILURE;
        }
    }
    let t0 = Instant::now();
    let res = if raise {
        let wait = Duration::from_secs_f64(cfg.raise_wait_sec.max(0.0));
        uia.list_chips_raised(wait)
    } else {
        uia.list_chips()
    };
    let chips = match res {
        Ok(c) => c,
        Err(e) => {
            eprintln!("list failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    match uia.shown_sessions() {
        Ok(s) => println!("shown sessions: {s:?}"),
        Err(e) => eprintln!("shown sessions failed: {e}"),
    }
    println!(
        "chips: {} ({} ms{})",
        chips.len(),
        t0.elapsed().as_millis(),
        if raise { ", raised" } else { "" }
    );
    for (i, c) in chips.iter().enumerate() {
        println!();
        println!("[{}] title  : {}", i + 1, c.title);
        println!("    tldr   : {}", c.tldr);
        println!("    buttons: {}", c.buttons.join(" | "));
        println!("    pane_x : {}", c.pane_x);
    }
    if sessions {
        print_sidebar(&uia);
    }
    ExitCode::SUCCESS
}
