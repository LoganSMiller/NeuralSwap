use neuralswap_core::error::{Code, Error};
use neuralswap_core::fsx::paths::is_inside;
use neuralswap_core::library::{self, Game};
use neuralswap_core::platform;
use neuralswap_core::scan::FolderScan;
use neuralswap_core::settings::{Health, Settings, SettingsStore, Theme};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Manager, State, Window};

use crate::scanner::Scanner;

use crate::validate::{AbsolutePath, LanguageTag};

pub struct AppState {
    pub settings: SettingsStore,
    pub scanner: Arc<Scanner>,
}

type Reply<T> = Result<T, Error>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    pub version: String,
    pub theme: Theme,
    pub lang: String,
    pub group_games_by_store: bool,
    /// Whether settings loaded cleanly, were migrated, were recovered from the
    /// backup, or had to be set aside. The window shows this: the upstream
    /// loader silently returned blank defaults on any read failure, so a user
    /// whose settings file was truncated simply found everything reset one
    /// morning with no indication why.
    pub settings_health: Health,
}

#[tauri::command]
pub fn app_boot(app: tauri::AppHandle, state: State<'_, AppState>) -> Reply<BootInfo> {
    let settings = state.settings.get();
    let health = state.settings.health();
    // Recorded at Info because it is the first thing anyone reading a bug
    // report needs: which schema loaded, and whether it loaded cleanly.
    log::info!(
        "boot: schema {}, settings {:?}, {} scan folders",
        settings.schema,
        health.status,
        settings.folders.len()
    );
    Ok(BootInfo {
        version: app.package_info().version.to_string(),
        theme: settings.theme,
        lang: settings.lang,
        group_games_by_store: settings.group_games_by_store,
        settings_health: state.settings.health(),
    })
}

#[tauri::command]
pub fn settings_health(state: State<'_, AppState>) -> Reply<Health> {
    Ok(state.settings.health())
}

#[tauri::command]
pub fn settings_read(state: State<'_, AppState>) -> Reply<Settings> {
    Ok(state.settings.get())
}

#[tauri::command]
pub fn settings_set_theme(state: State<'_, AppState>, theme: Theme) -> Reply<Theme> {
    state.settings.update(|settings| {
        settings.theme = theme;
        settings.theme
    })
}

#[tauri::command]
pub fn settings_set_language(state: State<'_, AppState>, lang: LanguageTag) -> Reply<String> {
    state.settings.update(|settings| {
        settings.lang = lang.as_str().to_owned();
        settings.lang.clone()
    })
}

#[tauri::command]
pub fn settings_set_group_games_by_store(state: State<'_, AppState>, enabled: bool) -> Reply<bool> {
    state.settings.update(|settings| {
        settings.group_games_by_store = enabled;
        settings.group_games_by_store
    })
}

#[tauri::command]
pub fn settings_set_auto_scan_drives(state: State<'_, AppState>, enabled: bool) -> Reply<bool> {
    state.settings.update(|settings| {
        settings.auto_scan_drives = enabled;
        settings.auto_scan_drives
    })
}

/// Case-insensitive membership, because Windows paths that differ only in case
/// are the same folder.
fn contains_path(list: &[String], candidate: &str) -> bool {
    list.iter().any(|item| item.eq_ignore_ascii_case(candidate))
}

#[tauri::command]
pub fn library_add_folder(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<Vec<String>> {
    let dir = dir.as_path().to_string_lossy().into_owned();
    state.settings.update(|settings| {
        if !contains_path(&settings.folders, &dir) {
            settings.folders.push(dir.clone());
        }
        // Adding a folder undoes a previous exclusion of the same path.
        settings
            .excluded_roots
            .retain(|root| !root.eq_ignore_ascii_case(&dir));
        settings.folders.clone()
    })
}

#[tauri::command]
pub fn library_remove_folder(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<Vec<String>> {
    let dir = dir.as_path().to_string_lossy().into_owned();
    state.settings.update(|settings| {
        settings
            .folders
            .retain(|item| !item.eq_ignore_ascii_case(&dir));
        // Removing it also remembers not to rediscover it on the next sweep.
        if !contains_path(&settings.excluded_roots, &dir) {
            settings.excluded_roots.push(dir.clone());
        }
        settings.folders.clone()
    })
}

#[tauri::command]
pub fn library_hide_game(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<Vec<String>> {
    let dir = dir.as_path().to_string_lossy().into_owned();
    state.settings.update(|settings| {
        if !contains_path(&settings.hidden, &dir) {
            settings.hidden.push(dir.clone());
        }
        settings.hidden.clone()
    })
}

/// Opening a folder is a small privilege, but it should still only ever be a
/// folder the user put in their own library.
///
/// The upstream handler is `(_e, dir) => shell.openPath(dir)`, which opens
/// whatever the frontend names - so a content-injection bug in the UI becomes
/// "launch that".
#[tauri::command]
pub fn shell_reveal_folder(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<bool> {
    let settings = state.settings.get();
    let known = settings
        .folders
        .iter()
        .chain(settings.manual.iter())
        .any(|root| is_inside(dir.as_path(), std::path::Path::new(root)));
    if !known {
        return Err(Error::new(
            Code::UnsafePath,
            format!("{} is not in the library", dir.as_path().display()),
        ));
    }
    if !dir.as_path().is_dir() {
        return Err(Error::new(Code::UnsafePath, "not a folder"));
    }

    // No shell involved: the path is passed as a single argument, so nothing
    // in it can be interpreted as a command.
    #[cfg(windows)]
    let launched = std::process::Command::new("explorer.exe")
        .arg(dir.as_path())
        .spawn()
        .is_ok();
    #[cfg(not(windows))]
    let launched = std::process::Command::new("xdg-open")
        .arg(dir.as_path())
        .spawn()
        .is_ok();

    if launched {
        Ok(true)
    } else {
        Err(Error::new(Code::UnsafePath, "could not open the folder"))
    }
}

/// The games this machine has installed, plus any folders added by hand.
///
/// Reads storefront records only - no folder is walked and nothing is written,
/// so this is cheap enough to call on every launch.
#[tauri::command]
pub async fn library_list(state: State<'_, AppState>) -> Reply<Vec<Game>> {
    let manual = state.settings.get().manual;
    tauri::async_runtime::spawn_blocking(move || {
        let mut games = library::discover(&platform::roots());
        for dir in manual {
            let path = std::path::PathBuf::from(&dir);
            if !path.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(dir);
            games.push(Game {
                name,
                dir: path,
                source: library::Source::Manual,
                app_id: None,
            });
        }
        library::dedupe(games)
    })
    .await
    .map_err(|error| {
        Error::new(
            Code::BadRequest,
            format!("discovery did not finish: {error}"),
        )
    })
}

/// Add a folder the storefronts do not know about.
#[tauri::command]
pub fn library_add_game(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<Vec<String>> {
    let dir = dir.as_path().to_string_lossy().into_owned();
    state.settings.update(|settings| {
        if !contains_path(&settings.manual, &dir) {
            settings.manual.push(dir.clone());
        }
        settings.manual.clone()
    })
}

/// Ask the user for a folder.
///
/// Driven from Rust rather than through the JS dialog plugin, so the frontend
/// is granted no capability that could open a file dialog on its own.
#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle) -> Reply<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    // The blocking picker must not run on the main thread.
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a game folder")
            .blocking_pick_folder()
            .map(|picked| picked.to_string())
    })
    .await
    .map_err(|error| Error::new(Code::BadRequest, format!("dialog failed: {error}")))
}

/// Scan one folder for installable executables.
///
/// Blocking work - a cold scan of a large folder reads a lot of headers - so
/// it is handed to a blocking thread rather than run on the async runtime,
/// where it would stall every other command including the one that cancels it.
#[tauri::command]
pub async fn scan_folder(state: State<'_, AppState>, dir: AbsolutePath) -> Reply<FolderScan> {
    let scanner = Arc::clone(&state.scanner);
    let path = dir.into_inner();
    tauri::async_runtime::spawn_blocking(move || scanner.scan(&path))
        .await
        .map_err(|error| Error::new(Code::BadRequest, format!("scan did not finish: {error}")))
}

/// Abandon the in-flight scan. The UI calls this when the user navigates away
/// from a result they no longer care about.
#[tauri::command]
pub fn scan_cancel(state: State<'_, AppState>) -> Reply<bool> {
    state.scanner.cancel();
    Ok(true)
}

/// How many binaries the persisted scan cache knows about, and how many stale
/// entries were dropped. Shown in the UI because "your rescan is instant" is
/// only credible if the number behind it is visible.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    pub entries: usize,
    pub pruned: usize,
}

#[tauri::command]
pub fn scan_cache_info(state: State<'_, AppState>) -> Reply<CacheInfo> {
    let pruned = state.scanner.prune_cache();
    Ok(CacheInfo {
        entries: state.scanner.cache_entries(),
        pruned,
    })
}

// ---- window controls -------------------------------------------------------
//
// Driven from Rust rather than through the JS window plugin, so the frontend
// needs no capability that lets it move or close arbitrary windows.

#[tauri::command]
pub fn window_minimize(window: Window) -> Reply<()> {
    window.minimize().map_err(window_error)
}

#[tauri::command]
pub fn window_toggle_maximize(window: Window) -> Reply<()> {
    let maximized = window.is_maximized().map_err(window_error)?;
    if maximized {
        window.unmaximize().map_err(window_error)
    } else {
        window.maximize().map_err(window_error)
    }
}

#[tauri::command]
pub fn window_close(window: Window) -> Reply<()> {
    window.close().map_err(window_error)
}

/// Flush any queued settings write before the window goes away, rather than
/// losing the user's last change.
#[tauri::command]
pub fn app_shutdown(app: tauri::AppHandle) -> Reply<()> {
    if let Some(state) = app.try_state::<AppState>() {
        // Writes are synchronous under the store's lock, so taking the lock
        // once is enough to know none is in flight.
        let _ = state.settings.get();
    }
    Ok(())
}

fn window_error(error: tauri::Error) -> Error {
    Error::new(
        Code::BadRequest,
        format!("window operation failed: {error}"),
    )
}
