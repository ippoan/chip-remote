// Tray-only app: never show a console window, also not in debug builds started from Explorer.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    chip_remote_agent_lib::run();
}
