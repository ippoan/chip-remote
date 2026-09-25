//! chip-remote Windows agent as a Tauri 2 tray app (no window).
//!
//! - [`agent`] — Worker WebSocket, locate queue, actions (testable, no Tauri)
//! - [`watcher`] / [`reporter`] / [`book`] — Claude desktop's session files → chips
//!   reported to the Worker over HTTP (no hook needed on this PC)
//! - [`driver`] — UIA behind a trait, served by one dedicated thread
//! - [`uia`] — the real driver (`chip_uia::Uia`)
//! - [`logging`] — agent.log (1 MB rotation) + ring buffer for 「ログをコピー」
//! - [`paths`] / [`status`] — locations, config template, tray status labels
//! - this file — tray menu, plugins (single instance, autostart, opener, updater)

pub mod agent;
pub mod book;
pub mod driver;
pub mod logging;
pub mod paths;
pub mod reporter;
pub mod status;
pub mod uia;
pub mod watcher;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, RunEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_opener::OpenerExt as _;
use tauri_plugin_updater::UpdaterExt as _;

use crate::logging::LogRing;
use crate::status::{Status, StatusSink};

const TOOLTIP: &str = "chip-remote-agent";
/// Created after the first run turned autostart on, so a user's later opt-out sticks.
const AUTOSTART_MARKER: &str = "autostart-initialized";

fn status_text(s: Status) -> String {
    format!("状態: {}", s.label())
}

/// 「設定ファイルを開く」: create config.json from the template if missing, then open it
/// with the default app for .json (Notepad as a fallback).
fn open_config(app: &AppHandle, path: &Path) {
    match paths::ensure_config_file(path) {
        Ok(true) => tracing::info!("config: created template {}", path.display()),
        Ok(false) => {}
        Err(e) => {
            tracing::warn!("config: cannot create {}: {e}", path.display());
            return;
        }
    }
    let p = path.to_string_lossy().to_string();
    if let Err(e) = app.opener().open_path(p.clone(), None::<&str>) {
        tracing::warn!("config: default app failed ({e}); using notepad");
        if let Err(e) = app.opener().open_path(p, Some("notepad.exe")) {
            tracing::warn!("config: notepad failed: {e}");
        }
    }
}

fn open_log_dir(app: &AppHandle, dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    if let Err(e) = app
        .opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
    {
        tracing::warn!("tray: cannot open log folder: {e}");
    }
}

fn copy_logs(ring: &LogRing) {
    let text = ring.snapshot();
    let bytes = text.len();
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
        Ok(()) => tracing::info!("tray: {bytes} bytes of log copied to the clipboard"),
        Err(e) => tracing::warn!("tray: clipboard failed: {e}"),
    }
}

/// Turns autostart on the first time the agent runs (NSIS does not register it).
fn autostart_first_run(app: &AppHandle, local_dir: &Path) {
    let marker = local_dir.join(AUTOSTART_MARKER);
    if marker.exists() {
        return;
    }
    match app.autolaunch().enable() {
        Ok(()) => {
            tracing::info!("autostart: enabled on first run");
            let _ = std::fs::create_dir_all(local_dir);
            let _ = std::fs::write(&marker, b"");
        }
        Err(e) => tracing::warn!("autostart: enable failed: {e}"),
    }
}

fn toggle_autostart(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    // muda flips the check mark before the event arrives.
    let want = item.is_checked().unwrap_or(false);
    let al = app.autolaunch();
    let r = if want { al.enable() } else { al.disable() };
    match r {
        Ok(()) => tracing::info!("autostart: {}", if want { "on" } else { "off" }),
        Err(e) => {
            tracing::warn!("autostart: change failed: {e}");
        }
    }
    let actual = al.is_enabled().unwrap_or(!want);
    let _ = item.set_checked(actual);
}

/// Builds the tray; returns the sink that shows the agent status in it.
fn build_tray(
    app: &tauri::App,
    ring: LogRing,
    config_path: PathBuf,
    local_dir: PathBuf,
) -> tauri::Result<StatusSink> {
    let initial = Status::Disconnected;
    let status_item = MenuItem::with_id(app, "status", status_text(initial), false, None::<&str>)?;
    let version = MenuItem::with_id(
        app,
        "version",
        format!("バージョン {}", app.package_info().version),
        false,
        None::<&str>,
    )?;
    let open_cfg = MenuItem::with_id(app, "open_config", "設定ファイルを開く", true, None::<&str>)?;
    let copy = MenuItem::with_id(app, "copy_logs", "ログをコピー", true, None::<&str>)?;
    let open_logs = MenuItem::with_id(app, "open_logs", "ログフォルダを開く", true, None::<&str>)?;
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "ログオン時に起動",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &version,
            &PredefinedMenuItem::separator(app)?,
            &open_cfg,
            &copy,
            &open_logs,
            &PredefinedMenuItem::separator(app)?,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let autostart_item = autostart.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(format!("{TOOLTIP}: {}", initial.label()))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open_config" => open_config(app, &config_path),
            "copy_logs" => copy_logs(&ring),
            "open_logs" => open_log_dir(app, &local_dir),
            "autostart" => toggle_autostart(app, &autostart_item),
            "quit" => {
                tracing::info!("tray: quit");
                app.exit(0);
            }
            other => tracing::debug!("tray: unknown menu id {other}"),
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    } else {
        tracing::warn!("tray: no default icon");
    }
    let tray = builder.build(app)?;

    let last = std::sync::Mutex::new(initial);
    Ok(Arc::new(move |s: Status| {
        let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
        if *l == s {
            return;
        }
        *l = s;
        let _ = status_item.set_text(status_text(s));
        let _ = tray.set_tooltip(Some(format!("{TOOLTIP}: {}", s.label())));
    }))
}

/// Checks the agent-latest release at start and hourly; installs (passive NSIS) and
/// restarts when there is a newer version. Fail-open: errors are only logged.
async fn update_loop(app: AppHandle) {
    loop {
        match app.updater() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    let ver = update.version.clone();
                    tracing::info!("updater: {ver} available; downloading");
                    match update
                        .download_and_install(|_, _| {}, || tracing::info!("updater: installing"))
                        .await
                    {
                        Ok(()) => {
                            tracing::info!("updater: installed {ver}; restarting");
                            app.restart();
                        }
                        Err(e) => tracing::warn!("updater: install failed: {e}"),
                    }
                }
                Ok(None) => tracing::debug!("updater: up to date"),
                Err(e) => tracing::warn!("updater: check failed: {e}"),
            },
            Err(e) => tracing::info!("updater: not available: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

pub fn run() {
    let local_dir = paths::local_dir();
    let ring = logging::init(&local_dir);
    let config_path = paths::config_path();
    let context = tauri::generate_context!();
    tracing::info!(
        "chip-remote-agent {} starting pid={} config={}",
        context.package_info().version,
        std::process::id(),
        config_path.display()
    );
    // CI builds without the signing key keep plugins.updater (the plugin fails to start
    // without it) but be defensive: no config -> no plugin and no update loop.
    let has_updater = context.config().plugins.0.contains_key("updater");

    let mut builder = tauri::Builder::default()
        // Must be the first plugin: a second launch just exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            tracing::info!("another launch of the agent was ignored (already running)");
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init());
    if has_updater {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let app = builder
        .setup(move |app| {
            autostart_first_run(app.handle(), &local_dir);
            let status = build_tray(app, ring.clone(), config_path.clone(), local_dir.clone())?;

            let driver = driver::spawn(uia::UiaFactory);
            let book = book::ChipBook::default();
            tauri::async_runtime::spawn(agent::run_forever(agent::AgentEnv {
                config_path: config_path.clone(),
                driver,
                status,
                book: book.clone(),
            }));
            tauri::async_runtime::spawn(watcher::run_forever(watcher::WatchEnv {
                config_path: config_path.clone(),
                appdata: paths::appdata_dir(),
                book,
                poll: watcher::POLL_INTERVAL,
                retry: reporter::RetryPolicy::default(),
            }));
            if has_updater {
                tauri::async_runtime::spawn(update_loop(app.handle().clone()));
            } else {
                tracing::info!("updater: not configured in this build");
            }
            Ok(())
        })
        .build(context)
        .expect("error while building the tauri application");

    app.run(|_app, event| {
        // No windows: keep running until 「終了」 (app.exit gives an explicit code).
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}
