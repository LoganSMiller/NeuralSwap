use neuralswap_core::error::{Code, Error};
use neuralswap_core::fsx::paths::is_inside;
use neuralswap_core::library::{self, Game};
use neuralswap_core::platform;
use neuralswap_core::scan::FolderScan;
use neuralswap_core::settings::{Health, Settings, SettingsStore, Theme};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Manager, State, Window};

use crate::installer::Installer;
use crate::scanner::Scanner;

use crate::validate::{AbsolutePath, LanguageTag, RelativeDir};

pub struct AppState {
    pub settings: SettingsStore,
    pub scanner: Arc<Scanner>,
    pub installer: Arc<Installer>,
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
pub async fn pick_folder(
    app: tauri::AppHandle,
    purpose: Option<PickPurpose>,
) -> Reply<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    // An enum rather than a caller-supplied title: the dialog's text is not
    // something the renderer needs to be able to set.
    let title = purpose.unwrap_or(PickPurpose::Game).title();
    // The blocking picker must not run on the main thread.
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title(title)
            .blocking_pick_folder()
            .map(|picked| picked.to_string())
    })
    .await
    .map_err(|error| Error::new(Code::BadRequest, format!("dialog failed: {error}")))
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PickPurpose {
    Game,
    Package,
}

impl PickPurpose {
    const fn title(self) -> &'static str {
        match self {
            PickPurpose::Game => "Choose a game folder",
            PickPurpose::Package => "Choose the folder holding the runtime DLLs",
        }
    }
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

// ------------------------------------------------------------------ install

/// Prove the frontend is asking about a folder the user actually has in their
/// library.
///
/// This is the gate on everything that writes. A path that merely passed
/// `AbsolutePath` is a *shape* we are willing to reason about; it is not
/// evidence that anybody chose it. Without this check, a compromised or buggy
/// renderer could name any folder on the machine and have DLLs written into
/// it, and the core would happily oblige because the path is well-formed.
///
/// Configured roots are checked first because it is a string comparison.
/// Discovery is the fallback, because a Steam game lives under a library
/// folder the user never had to add by hand - refusing those would mean
/// refusing almost every game.
fn assert_in_library(state: &State<'_, AppState>, dir: &std::path::Path) -> Reply<()> {
    let settings = state.settings.get();
    let configured = settings
        .folders
        .iter()
        .chain(settings.manual.iter())
        .any(|root| is_inside(dir, std::path::Path::new(root)));
    if configured {
        return Ok(());
    }

    // `hidden` is deliberately not consulted: hiding a game is a display
    // preference, not a withdrawal of permission, and a hidden game the user
    // has explicitly asked to install into is still their game.
    let discovered = library::discover(&platform::roots())
        .into_iter()
        .any(|game| is_inside(dir, &game.dir));
    if discovered {
        return Ok(());
    }

    Err(Error::new(
        Code::UnsafePath,
        format!(
            "{} is not a game in the library - add its folder first",
            dir.display()
        ),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReply {
    pub plan: neuralswap_core::install::Plan,
    /// The checks, run against this plan. Returned with the plan rather than
    /// on demand, so the one screen a user reads has everything on it.
    pub preflight: neuralswap_core::install::Preflight,
    /// Whether an install or restore is already running for this game.
    pub busy: bool,
}

/// Derive a plan and run the checks. Writes nothing.
#[tauri::command]
pub async fn install_plan(
    state: State<'_, AppState>,
    game_dir: AbsolutePath,
    install_dir: RelativeDir,
    package_dir: AbsolutePath,
) -> Reply<PlanReply> {
    assert_in_library(&state, game_dir.as_path())?;
    let installer = Arc::clone(&state.installer);
    let game = game_dir.into_inner();
    let package = package_dir.into_inner();
    let rel = install_dir.as_str().to_owned();

    // Hashing a package and the files it would replace is disk-bound, so it
    // goes to a blocking thread rather than stalling the UI thread.
    blocking(move || {
        let plan = installer.plan(&game, &rel, &package)?;
        let preflight = installer.preflight(&game, &plan, &package);
        Ok(PlanReply {
            busy: installer.is_busy(&game),
            plan,
            preflight,
        })
    })
    .await
}

/// Install. The plan is rebuilt here rather than accepted from the frontend.
///
/// That is the important part: a plan is a decision about what to overwrite,
/// and taking one from the renderer would mean trusting it to say which files
/// to replace. Rebuilding costs one more pass over a handful of DLLs and means
/// the only thing crossing the boundary is *which game and which package*.
/// If the folder has changed since the user looked, the rebuilt plan describes
/// the change and `apply` refuses a stale one anyway.
#[tauri::command]
pub async fn install_apply(
    state: State<'_, AppState>,
    game_dir: AbsolutePath,
    install_dir: RelativeDir,
    package_dir: AbsolutePath,
) -> Reply<neuralswap_core::install::Outcome> {
    assert_in_library(&state, game_dir.as_path())?;
    let installer = Arc::clone(&state.installer);
    let game = game_dir.into_inner();
    let package = package_dir.into_inner();
    let rel = install_dir.as_str().to_owned();

    blocking(move || {
        let plan = installer.plan(&game, &rel, &package)?;
        log::info!(
            "installing into {}: {} change(s), {} to write",
            game.display(),
            plan.changes,
            plan.write_bytes
        );
        let outcome = installer.apply(&game, &plan, &package)?;
        log::info!("install outcome: {outcome:?}");
        Ok(outcome)
    })
    .await
}

/// Ask a running install to stop. It rolls back what it has already written.
#[tauri::command]
pub fn install_cancel(state: State<'_, AppState>) -> Reply<()> {
    state.installer.cancel();
    Ok(())
}

/// What we installed in this game, and whether it is still what we wrote.
#[tauri::command]
pub async fn install_status(
    state: State<'_, AppState>,
    game_dir: AbsolutePath,
) -> Reply<Option<neuralswap_core::install::Integrity>> {
    assert_in_library(&state, game_dir.as_path())?;
    let installer = Arc::clone(&state.installer);
    let game = game_dir.into_inner();
    blocking(move || installer.status(&game)).await
}

/// What a restore would do, without doing it.
#[tauri::command]
pub async fn install_restore_preview(
    state: State<'_, AppState>,
    game_dir: AbsolutePath,
) -> Reply<neuralswap_core::install::restore::Outcome> {
    assert_in_library(&state, game_dir.as_path())?;
    let installer = Arc::clone(&state.installer);
    let game = game_dir.into_inner();
    blocking(move || installer.restore_preview(&game)).await
}

/// Put the game back the way it was.
#[tauri::command]
pub async fn install_restore(
    state: State<'_, AppState>,
    game_dir: AbsolutePath,
) -> Reply<neuralswap_core::install::restore::Outcome> {
    assert_in_library(&state, game_dir.as_path())?;
    let installer = Arc::clone(&state.installer);
    let game = game_dir.into_inner();
    blocking(move || {
        let outcome = installer.restore(&game)?;
        log::info!("restore outcome for {}: {outcome:?}", game.display());
        Ok(outcome)
    })
    .await
}

/// Run a closure on the blocking pool and flatten the join error.
///
/// A panic in a blocking task would otherwise surface as a join failure with
/// no code attached, which the frontend has no way to display.
async fn blocking<T, F>(work: F) -> Reply<T>
where
    T: Send + 'static,
    F: FnOnce() -> Reply<T> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(Error::new(
            Code::BadRequest,
            format!("the operation did not finish: {error}"),
        )),
    }
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
