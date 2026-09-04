// `expect` and `panic` are correct in a test: a broken fixture should abort
// the run, loudly. The lints stay strict for everything that ships.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

mod commands;
mod installer;
mod scanner;
mod validate;

use commands::AppState;
use installer::Installer;
use neuralswap_core::settings::SettingsStore;
use scanner::Scanner;
use std::sync::Arc;
use tauri::Manager;

/// Everything the frontend is allowed to ask for, in one list.
///
/// This is the Tauri equivalent of the contract table the TypeScript build
/// carried: one place that says what crosses the boundary. Arguments are
/// deserialised into types that validate themselves, so a handler cannot
/// receive input that was never checked.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[allow(clippy::expect_used)] // nothing exists yet to report a failure into
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let file = app
                .path()
                .app_config_dir()
                .map(|dir| dir.join("settings.json"))
                .map_err(|error| format!("no config directory: {error}"))?;

            // A settings file written by a newer build is refused rather than
            // downgraded. That is not a reason to fail to start, so it is
            // reported and the app continues on defaults with the file left
            // untouched for the newer build to find.
            let settings = match SettingsStore::open(&file) {
                Ok(store) => store,
                Err(error) => {
                    log::warn!("settings not loaded ({error}); continuing on defaults");
                    // Opening a path that cannot exist yields a Fresh store,
                    // so defaults are in memory and nothing overwrites the
                    // real file until the user changes something.
                    SettingsStore::open(file.with_extension("session-only.json"))
                        .map_err(|inner| format!("cannot initialise settings: {inner}"))?
                }
            };

            // The scan cache lives beside the settings and is loaded here so
            // the first scan of a session is already warm.
            let cache_file = file
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("scan-cache.json");
            let scanner = Arc::new(Scanner::load(cache_file));

            // Journals, backups and install records live beside the settings.
            let data_dir = file
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            let installer = Arc::new(Installer::new(&data_dir));

            // Before the window is usable. An install interrupted by a crash
            // or a power cut leaves a half-changed game folder, and the user
            // must not be invited to install on top of one - so this runs
            // first and reports what it did.
            for outcome in installer.recover_at_startup() {
                log::warn!(
                    "recovered install journal {}: {:?} ({}), {} restored, {} removed, {} failed",
                    outcome.id,
                    outcome.decision,
                    outcome.reason,
                    outcome.restored.len(),
                    outcome.removed.len(),
                    outcome.failures.len()
                );
            }

            app.manage(AppState {
                settings,
                scanner,
                installer,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_boot,
            commands::app_shutdown,
            commands::settings_health,
            commands::settings_read,
            commands::settings_set_theme,
            commands::settings_set_language,
            commands::settings_set_group_games_by_store,
            commands::settings_set_auto_scan_drives,
            commands::library_add_folder,
            commands::library_remove_folder,
            commands::library_hide_game,
            commands::library_list,
            commands::library_add_game,
            commands::pick_folder,
            commands::scan_folder,
            commands::scan_cancel,
            commands::scan_cache_info,
            commands::install_plan,
            commands::install_apply,
            commands::install_cancel,
            commands::install_status,
            commands::install_restore_preview,
            commands::install_restore,
            commands::shell_reveal_folder,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
