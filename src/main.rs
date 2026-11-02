#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod asr;
mod audio;
mod budget;
mod bundle;
mod context;
mod core;
mod db;
mod discover;
mod export;
mod i18n;
mod icon;
mod llama;
mod net;
mod proc;
mod providers;
mod redact;
mod screen;
mod secrets;
mod session;
mod settings;
mod types;
mod ui;
mod update;
mod win;

use std::path::PathBuf;

/// The previous build's folder layout is kept, so settings, history and downloaded models carry over.
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPILOT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let new = home.join("copilot");
    let old = home.join("kairos-m");
    if !new.exists() && old.exists() {
        // A locked file aborts the move; I fall back to whichever folder exists.
        let _ = std::fs::rename(&old, &new);
    }
    if new.exists() || !old.exists() {
        new
    } else {
        old
    }
}

fn main() {
    // A test instance (own data folder) runs next to the user's real one and must not touch it.
    let isolated = std::env::var_os("COPILOT_DATA_DIR").is_some() || std::env::var_os("COPILOT_SNAPSHOT").is_some();
    if !isolated && !win::single_instance(std::env::args().any(|a| a == update::RELAUNCH_ARG)) {
        return;
    }
    update::clean_old();
    std::panic::set_hook(Box::new(|info| {
        db::diag("panic", serde_json::json!({ "message": redact::redact(&info.to_string()) }));
    }));
    let core = match core::Core::new(data_dir()) {
        Ok(c) => c,
        Err(e) => {
            rfd::MessageDialog::new().set_title("Copilot").set_description(e).set_level(rfd::MessageLevel::Error).show();
            return;
        }
    };
    if let Err(e) = ui::run(core.clone()) {
        db::diag("ui_exit", serde_json::json!({ "error": e.to_string() }));
    }
    core.shutdown();
}
