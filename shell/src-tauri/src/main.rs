// DigUp's shell for Windows and Linux (tried out on the Mac first): the tray icon, the hotkey, the search panel, the
// window, and the setup and settings windows. The work is DigUp's helper, `digup`, run as processes speaking JSON lines
// (helper.rs): `search-service` answers the panel and the window, `index-service` keeps the index current,
// `download-model` fetches the model, `estimate` sizes up folders. The Mac app (Sources/DigUpApp) is the reference for
// behavior.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod helper;
mod hotkey;
mod options;
mod settings;
mod status;
mod watcher;
mod wayland;

use helper::{IndexService, Launch, ModelDownload, SearchService};
use hotkey::Method;
use options::{Options, DEFAULT_HOTKEY};
use serde_json::{json, Value};
use settings::Settings;
use status::Status;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_opener::OpenerExt;
use watcher::FolderWatcher;

const PANEL_WIDTH: f64 = 780.0;
/// The panel's window, under one name or the other: made anew when the person turns centering on or off.
const PANEL: &str = "panel";
const CENTERED_PANEL: &str = "panel-centered";
const FIELD_HEIGHT: f64 = 56.0;
/// The window's label.
const WINDOW: &str = "main";

/// What the commands and events share.
struct Shell {
    options: Options,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    /// How the panel's shortcut reaches DigUp on this desktop (hotkey.rs).
    shortcut: Method,
    /// The desktop places windows itself (GTK on Wayland), so the panel sits where it puts it unless the person has it
    /// centered (Settings): its window then fills the screen and the page places the panel (wayland.rs).
    wayland: bool,
    /// The panel's window now: PANEL, or CENTERED_PANEL when it fills the screen.
    panel_label: Mutex<&'static str>,
    search: SearchService,
    index: IndexService,
    model: ModelDownload,
    /// The model for the setup window and the tray: {"state": "checking|downloading|waiting|ready|failed", …}.
    model_state: Mutex<Value>,
    /// What indexing is doing, from the index helper's events (status.rs).
    status: Mutex<Status>,
    watcher: Mutex<Option<FolderWatcher>>,
    /// The tray menu's first line: what's going on.
    status_item: Mutex<Option<MenuItem<Wry>>>,
    /// When the search service last reloaded the index (while indexing, at most every few seconds).
    reloaded: Mutex<Instant>,
    /// When the index helper's watchdog ended it lately (on a file whose reading ran away).
    watchdog_stops: Mutex<Vec<Instant>>,
    /// A search the panel hands the window (its query, and the kind of results to show): taken when the window opens.
    window_search: Mutex<Option<(String, Option<String>)>>,
    /// Debug runs with setup: the panel's query waits until the first index is in.
    debug_query_sent: AtomicBool,
    /// Quit was chosen: no helper starts again (an index event arriving meanwhile would start the search service).
    quitting: AtomicBool,
    logs: PathBuf,
}

impl Shell {
    /// The folders searched: the launch arguments' in dev runs, else the settings'.
    fn folders(&self) -> Vec<String> {
        if !self.options.roots.is_empty() {
            return self.options.roots.iter().map(|root| root.display().to_string()).collect();
        }
        self.settings.lock().unwrap().folders.clone()
    }

    fn is_set_up(&self) -> bool {
        !self.options.roots.is_empty() || self.settings.lock().unwrap().set_up
    }

    fn hotkey(&self) -> String {
        let chosen = self.settings.lock().unwrap().hotkey.clone();
        self.options.hotkey.clone().unwrap_or(if chosen.is_empty() { DEFAULT_HOTKEY.to_string() } else { chosen })
    }

    /// The shortcut as people read it: the desktop's, once DigUp's is there (it may have been changed there).
    fn hotkey_text(&self) -> String {
        #[cfg(target_os = "linux")]
        match self.shortcut {
            Method::Gnome => {
                if let Some(binding) = hotkey::gnome::binding() {
                    return hotkey::gnome::shown(&binding);
                }
            }
            Method::Kde => {
                if let Some(key) = hotkey::kde::current() {
                    return key;
                }
            }
            _ => {}
        }
        shortcut_text(&self.hotkey())
    }

    /// A background helper (indexing, the download), below the person's own programs for the processor.
    fn launch(&self, args: Vec<String>, log: &str) -> Launch {
        Launch {
            program: self.options.helper.clone(),
            args,
            env: self.options.models.iter().map(|models| ("DIGUP_MODELS".to_string(), models.display().to_string())).collect(),
            log: Some(self.logs.join(log)),
            background: true,
        }
    }

    fn index_arguments(&self) -> Vec<String> {
        self.options.index.iter().flat_map(|index| ["--index".to_string(), index.display().to_string()]).collect()
    }

    /// `index-service` for the folders as they are now.
    fn index_launch(&self) -> Launch {
        let settings = self.settings.lock().unwrap().clone();
        let mut args = vec!["index-service".to_string(), "--backfill".to_string()];
        args.extend(self.index_arguments());
        for folder in &settings.skipped {
            args.extend(["--exclude".to_string(), folder.clone()]);
        }
        for extension in &settings.skip_types {
            args.extend(["--exclude-ext".to_string(), extension.clone()]);
        }
        args.extend(self.folders());
        self.launch(args, "index.log")
    }

    fn model_ready(&self) -> bool {
        self.model_state.lock().unwrap()["state"] == "ready"
    }

    /// The panel sits centered in a window that fills the screen (Settings, on Wayland only).
    fn centers_panel(&self) -> bool {
        self.wayland && self.settings.lock().unwrap().center_panel
    }

    /// DigUp may open at login: not on a Mac (the DigUp app has its own login item) and not in a dev run with a settings
    /// file of its own.
    fn opens_at_login(&self) -> bool {
        !cfg!(target_os = "macos") && self.options.settings.is_none()
    }

    /// What the pages show about indexing and the model (status.rs).
    fn snapshot(&self) -> Value {
        let model = self.model_state.lock().unwrap().clone();
        self.status.lock().unwrap().snapshot(!self.folders().is_empty(), &model)
    }
}

// MARK: Commands

/// The panel's and the window's questions for `search-service`.
#[tauri::command]
async fn ask(shell: State<'_, Arc<Shell>>, request: Value) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default();
    if !["search", "embed_ahead", "text", "render", "prewarm", "status", "summaries"].contains(&op) {
        return Err(format!("not a question the panel asks: {op}"));
    }
    shell.search.ask(request).await
}

/// What indexing these folders would take, one answer per folder (`digup estimate --json`): files by kind, the first
/// pass's seconds, the rest of long files, subfolders. For setup.
#[tauri::command]
async fn estimate(shell: State<'_, Arc<Shell>>, folders: Vec<String>) -> Result<Vec<Value>, String> {
    let helper = shell.options.helper.clone();
    // The index's costs.json: what indexing costs on this computer, once it has indexed enough.
    let index = shell.index_arguments();
    tauri::async_runtime::spawn_blocking(move || {
        let mut command = std::process::Command::new(&helper);
        command.arg("estimate").args(&folders).args(&index).arg("--json");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let output = command.output().map_err(|error| format!("can't run {}: {error}", helper.display()))?;
        Ok(String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| serde_json::from_str(line).ok()).collect())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// What the setup window starts from: the folders it offers, the hotkey, the model, the folders already chosen.
#[tauri::command]
fn setup_info(app: AppHandle, shell: State<'_, Arc<Shell>>) -> Value {
    let offered: Vec<PathBuf> = match &shell.options.offered {
        Some(folders) => folders.clone(),
        None => {
            let paths = app.path();
            [paths.document_dir(), paths.picture_dir(), paths.video_dir(), paths.audio_dir(), paths.desktop_dir(),
             paths.download_dir()]
                .into_iter()
                .filter_map(Result::ok)
                .filter(|folder| folder.is_dir())
                .collect()
        }
    };
    json!({
        "offered": offered.iter().map(|folder| folder.display().to_string()).collect::<Vec<_>>(),
        "chosen": shell.folders(),
        "hotkey": shell.hotkey_text(),
        "shortcut": {"method": shell.shortcut.name(), "command": hotkey::toggle_command()},
        "model": shell.model_state.lock().unwrap().clone(),
        "auto": shell.options.debug_setup,
    })
}

/// Folders chosen in the system's folder picker (none when it's cancelled).
#[tauri::command]
async fn pick_folders(app: AppHandle) -> Vec<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Add Folders").pick_folders(move |folders| {
        let _ = sender.send(folders);
    });
    let picked = receiver.await.ok().flatten().unwrap_or_default();
    picked.into_iter().filter_map(|folder| folder.into_path().ok()).map(|path| path.display().to_string()).collect()
}

/// Setup's last step: the folders are saved and indexing starts (once the model is in).
#[tauri::command]
fn finish_setup(app: AppHandle, shell: State<'_, Arc<Shell>>, folders: Vec<String>) -> Result<(), String> {
    let first = !shell.is_set_up();
    {
        let mut settings = shell.settings.lock().unwrap();
        settings.folders = folders;
        settings.set_up = true;
        settings.save(&shell.settings_path).map_err(|error| format!("can't save the settings: {error}"))?;
    }
    // The first setup has DigUp open at login (it lives in the tray; Settings turns that off), as the Mac app does.
    if first && shell.opens_at_login() {
        if let Err(error) = app.autolaunch().enable() {
            eprintln!("digup-shell: can't open at login: {error}");
        }
    }
    // The first setup adds the shortcut to GNOME's settings; a later one (Choose Folders…) leaves it as the person has
    // it there.
    #[cfg(target_os = "linux")]
    if first && shell.shortcut == Method::Gnome {
        if let Err(error) = hotkey::gnome::bind(&shell.hotkey(), &hotkey::toggle_command()) {
            set_status(&app, &format!("The shortcut couldn't be added to GNOME's settings: {error}"));
        }
    }
    apply_folders(&app);
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.close();
    }
    // The first time, the window comes up: searching starts there while indexing goes on.
    if first && !shell.options.debug_setup {
        later(&app, |app| {
            let _ = show_window(app);
        });
    }
    Ok(())
}

/// Opens a result in its default app.
#[tauri::command]
fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    hide_panel(app.clone());
    app.opener().open_path(path, None::<&str>).map_err(|error| error.to_string())
}

/// Shows a result in the file manager, selected.
#[tauri::command]
fn reveal(app: AppHandle, path: String) -> Result<(), String> {
    hide_panel(app.clone());
    app.opener().reveal_item_in_dir(path).map_err(|error| error.to_string())
}

#[tauri::command]
fn hide_panel(app: AppHandle) {
    if let Some(panel) = panel_window(&app) {
        let _ = panel.hide();
    }
}

/// The panel grows to show results and shrinks back to its field.
#[tauri::command]
fn panel_height(app: AppHandle, height: f64) {
    if let Some(panel) = panel_window(&app) {
        let _ = panel.set_size(LogicalSize::new(PANEL_WIDTH, height.clamp(FIELD_HEIGHT, 640.0)));
    }
}

/// Where the page drew the panel in a window that fills the screen (wayland.rs): only there does it take the pointer.
#[tauri::command]
fn panel_area(app: AppHandle, x: f64, y: f64, width: f64, height: f64) {
    #[cfg(target_os = "linux")]
    if let Some(panel) = panel_window(&app) {
        let area = (x.floor() as i32, y.floor() as i32, width.ceil() as i32, height.ceil() as i32);
        wayland::set_area(&panel, area);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (app, x, y, width, height);
}

/// Debug runs: the query the panel types in when it loads (with setup, it comes later as a "debug-query" event).
#[tauri::command]
fn debug_query(shell: State<'_, Arc<Shell>>) -> Option<String> {
    if shell.options.debug_setup {
        return None;
    }
    shell.options.debug_query.clone()
}

/// Debug runs: what the page showed for the debug query, printed (and the shell quits, if asked).
#[tauri::command]
fn debug_report(app: AppHandle, shell: State<'_, Arc<Shell>>, report: Value) {
    println!("{report}");
    if shell.options.debug_quit {
        quit(&app);
    }
}

// MARK: The window's commands

/// What the window shows besides results: the folders searched, the shortcut (none where the person has to add one),
/// indexing, and a search the panel handed over (taken once).
#[tauri::command]
fn window_info(shell: State<'_, Arc<Shell>>) -> Value {
    let search = shell.window_search.lock().unwrap().take();
    json!({
        "folders": shell.folders(),
        "hotkey": (shell.shortcut != Method::ByHand).then(|| shell.hotkey_text()),
        "state": shell.snapshot(),
        "search": search.map(|(query, kind)| json!({"query": query, "kind": kind})),
        "debugQuery": shell.options.debug_window,
    })
}

/// Indexing and the model, as the pages show them (also sent as "state" events when they change).
#[tauri::command]
fn app_state(shell: State<'_, Arc<Shell>>) -> Value {
    shell.snapshot()
}

/// The panel's search in the window, for a bigger look (Ctrl+O, "Show all…"); `kind` limits it to one group.
#[tauri::command]
fn open_window(app: AppHandle, shell: State<'_, Arc<Shell>>, query: String, kind: Option<String>) {
    hide_panel(app.clone());
    *shell.window_search.lock().unwrap() = Some((query, kind));
    later(&app, |app| {
        let _ = show_window(app);
    });
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    later(&app, |app| {
        let _ = show_settings(app);
    });
}

#[tauri::command]
fn choose_folders(app: AppHandle) {
    later(&app, |app| {
        let _ = show_setup(app);
    });
}

#[tauri::command]
fn index_now(app: AppHandle) {
    run_index(&app);
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    quit(&app);
}

/// Ctrl+W in a window.
#[tauri::command]
fn close_window(window: tauri::WebviewWindow) {
    let _ = window.close();
}

/// Runs on the main thread once the command at hand has returned: a window made inside a command's handler can
/// deadlock on Windows.
fn later(app: &AppHandle, work: impl FnOnce(&AppHandle) + Send + 'static) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || work(&handle));
}

fn main() {
    let options = match Options::from_args() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("digup-shell: {error}");
            std::process::exit(2);
        }
    };
    // A desktop shortcut's `--toggle` while DigUp runs goes to it before this process starts GTK (hotkey.rs).
    #[cfg(target_os = "linux")]
    if options.toggle && hotkey::hand_over() {
        return;
    }
    let app = tauri::Builder::default()
        // A second launch (DigUp opened again from the app grid or the Start menu) shows the window, as on the Mac; a
        // desktop shortcut's `--toggle` shows or hides the panel.
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if args.iter().any(|arg| arg == "--toggle") {
                toggle_panel(app)
            } else if !app.state::<Arc<Shell>>().is_set_up() {
                let _ = show_setup(app);
            } else {
                let _ = show_window(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // The launch at login says so: DigUp then waits in the tray instead of showing its window.
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--at-login"])))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .setup(move |app| setup(app, options))
        .invoke_handler(tauri::generate_handler![
            ask, estimate, setup_info, pick_folders, finish_setup, open_path, reveal, hide_panel, panel_height,
            panel_area, settings_info, set_open_at_login, set_center_panel, debug_query, debug_report, window_info,
            app_state, open_window, open_settings, choose_folders, index_now, quit_app, close_window
        ])
        .build(tauri::generate_context!())
        .expect("DigUp couldn't start");
    app.run(|app, event| match event {
        tauri::RunEvent::Exit => {
            stop_helpers(app);
            #[cfg(target_os = "linux")]
            if app.state::<Arc<Shell>>().shortcut == Method::Kde {
                hotkey::kde::stop();
            }
        }
        // Closing the last window isn't quitting: DigUp lives in the tray.
        tauri::RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        _ => {}
    });
}

fn setup(app: &mut tauri::App, options: Options) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory); // the menu bar only, no Dock icon

    let handle = app.handle().clone();
    let logs = app.path().app_log_dir()?;
    std::fs::create_dir_all(&logs)?;
    let settings_path = match &options.settings {
        Some(path) => path.clone(),
        None => app.path().app_config_dir()?.join("settings.json"),
    };
    let settings = Settings::load(&settings_path);
    let roots: Vec<String> = if options.roots.is_empty() {
        settings.folders.clone()
    } else {
        options.roots.iter().map(|root| root.display().to_string()).collect()
    };
    let mut search_args = vec!["search-service".to_string()];
    search_args.extend(options.index.iter().flat_map(|index| ["--index".to_string(), index.display().to_string()]));
    search_args.extend(roots);
    let search = SearchService::new(
        Launch {
            program: options.helper.clone(),
            args: search_args,
            env: options.models.iter().map(|models| ("DIGUP_MODELS".to_string(), models.display().to_string())).collect(),
            log: Some(logs.join("search.log")),
            background: false,   // it answers typing
        },
        {
            let app = handle.clone();
            move |event| search_event(&app, event)
        },
    );
    let index = IndexService::new({
        let app = handle.clone();
        move |event| index_event(&app, event)
    });
    let model = ModelDownload::new({
        let app = handle.clone();
        move |event| model_event(&app, event)
    });
    // Previews show files from the chosen folders (and the helper's renders, once it says where the index is).
    for root in &settings.folders {
        let _ = app.asset_protocol_scope().allow_directory(root, true);
    }
    for root in &options.roots {
        let _ = app.asset_protocol_scope().allow_directory(root, true);
    }
    let shell = Arc::new(Shell {
        options,
        settings: Mutex::new(settings),
        settings_path,
        shortcut: hotkey::method(),
        wayland: wayland::active(),
        panel_label: Mutex::new(PANEL),
        search,
        index,
        model,
        model_state: Mutex::new(json!({"state": "checking"})),
        watcher: Mutex::new(None),
        status: Mutex::new(Status::default()),
        status_item: Mutex::new(None),
        reloaded: Mutex::new(Instant::now()),
        watchdog_stops: Mutex::default(),
        window_search: Mutex::new(None),
        debug_query_sent: AtomicBool::new(false),
        quitting: AtomicBool::new(false),
        logs,
    });
    app.manage(shell.clone());

    make_panel(&handle, shell.centers_panel())?;
    // No tray (a session without a taskbar, a Linux desktop without one): the hotkey and the panel still work.
    if let Err(error) = make_tray(&handle, &shell) {
        eprintln!("digup-shell: no tray icon: {error}");
    }
    match shell.shortcut {
        Method::Grab => {
            let hotkey = shell.hotkey();
            if let Err(error) = app.global_shortcut().register(hotkey.as_str()) {
                set_status(&handle, &format!("The shortcut {} is taken", shortcut_text(&hotkey)));
                eprintln!("digup-shell: can't use {hotkey} as the shortcut: {error}");
            }
        }
        // GNOME runs DigUp's custom shortcut (added at setup); its command follows DigUp when DigUp moves.
        #[cfg(target_os = "linux")]
        Method::Gnome if shell.is_set_up() => hotkey::gnome::refresh(&hotkey::toggle_command()),
        // KDE signals DigUp while it runs.
        #[cfg(target_os = "linux")]
        Method::Kde => {
            let pressed = handle.clone();
            if let Err(error) = hotkey::kde::listen(&shell.hotkey(), move || toggle_panel(&pressed)) {
                set_status(&handle, &format!("The shortcut couldn't be added to KDE's settings: {error}"));
                eprintln!("digup-shell: can't add the shortcut to KDE's: {error}");
            }
        }
        _ => {}
    }

    // The search service starts now (the first search is then quick); the model is checked (and fetched if it's
    // missing: on first launch that runs while setup is open, as on the Mac); indexing waits for both.
    let warm = shell.clone();
    tauri::async_runtime::spawn(async move {
        let _ = warm.search.ask(json!({"op": "status"})).await;
    });
    let mut download = vec!["download-model".to_string()];
    if let Some(models) = &shell.options.models {
        download.extend(["--to".to_string(), models.display().to_string()]);
    }
    if let Err(error) = shell.model.run(&shell.launch(download, "model.log")) {
        model_event(&handle, json!({"event": "error", "message": error}));
    }
    if shell.is_set_up() {
        watch_folders(&handle);
    } else {
        show_setup(&handle)?;
    }
    let options = &shell.options;
    if options.toggle || options.debug_show && options.debug_window.is_none() {
        // Started by the shortcut (DigUp wasn't running): the panel comes up as it would have.
        show_panel(&handle);
    } else if options.debug_window.is_some() || shell.is_set_up() && !options.at_login && options.debug_query.is_none() {
        // Opened by hand: the window, so opening DigUp never looks like nothing happened (at login it waits in the
        // tray).
        if let Err(error) = show_window(&handle) {
            eprintln!("digup-shell: can't open the window: {error}");
        }
    }
    // The entry that opens DigUp at login is written again: it follows DigUp if it moved, and older ones didn't say
    // `--at-login`.
    if shell.opens_at_login() && app.autolaunch().is_enabled().unwrap_or(false) {
        if let Err(error) = app.autolaunch().enable() {
            eprintln!("digup-shell: can't renew opening at login: {error}");
        }
    }
    Ok(())
}

/// After setup or a change of folders: the search service, the watcher and indexing follow the new folders.
fn apply_folders(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let folders = shell.folders();
    let (skipped, types) = {
        let settings = shell.settings.lock().unwrap();
        (settings.skipped.clone(), settings.skip_types.clone())
    };
    for folder in &folders {
        let _ = app.asset_protocol_scope().allow_directory(folder, true);
    }
    let search = shell.clone();
    tauri::async_runtime::spawn(async move {
        let _ = search.search.ask(json!({"op": "folders", "roots": folders, "skipped": skipped, "types": types})).await;
    });
    watch_folders(app);
}

/// Watches the folders, and indexes them now when the model is in.
fn watch_folders(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let folders: Vec<PathBuf> = shell.folders().iter().map(PathBuf::from).collect();
    changed(app);
    if folders.is_empty() {
        return;
    }
    let changed = {
        let app = app.clone();
        move || run_index(&app)
    };
    let skipped = shell.settings.lock().unwrap().skipped.iter().map(PathBuf::from).collect();
    *shell.watcher.lock().unwrap() = Some(FolderWatcher::start(&folders, skipped, shell.options.index_folder(), changed));
    run_index(app);
}

/// Has the index helper sync and index, once there's a model and a folder.
fn run_index(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>();
    if !shell.is_set_up() || shell.folders().is_empty() || !shell.model_ready() || shell.quitting.load(Ordering::SeqCst) {
        return;
    }
    if let Err(error) = shell.index.run(&shell.index_launch()) {
        set_status(app, &format!("Indexing couldn't start: {error}"));
    }
}

fn stop_helpers(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>();
    shell.model.stop();
    shell.index.stop();
    shell.search.stop();
}

fn quit(app: &AppHandle) {
    app.state::<Arc<Shell>>().quitting.store(true, Ordering::SeqCst);
    stop_helpers(app);
    app.exit(0);
}

// MARK: Windows

/// The panel's window, hidden: fitted to the panel, or filling the screen with the panel centered in it.
fn make_panel(app: &AppHandle, centered: bool) -> tauri::Result<()> {
    let (label, page) = if centered { (CENTERED_PANEL, "index.html?fills") } else { (PANEL, "index.html") };
    let panel = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title("DigUp")
        .inner_size(PANEL_WIDTH, FIELD_HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        // GTK 3 makes a window that can't be resized at least 200 px tall when its content has no height of its own (a
        // web view's), so the field alone wouldn't fit; resizable, it takes the size asked for. It has no edges to drag.
        .resizable(cfg!(target_os = "linux"))
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;
    #[cfg(target_os = "linux")]
    if centered {
        wayland::track(&panel);
    }
    *app.state::<Arc<Shell>>().panel_label.lock().unwrap() = label;
    let app = app.clone();
    panel.on_window_event(move |event| {
        // Like Spotlight: it steps away when you click elsewhere.
        if let WindowEvent::Focused(false) = event {
            if !app.state::<Arc<Shell>>().options.debug_show {
                hide_panel(app.clone());
            }
        }
    });
    Ok(())
}

fn panel_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    let label = *app.state::<Arc<Shell>>().panel_label.lock().unwrap();
    app.get_webview_window(label)
}

/// Settings turned centering on or off: the panel's window is made the other way (a window can't change between the
/// two), then the old one goes.
fn remake_panel(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let old = panel_window(app);
    if old.as_ref().is_some_and(|panel| (panel.label() == CENTERED_PANEL) == shell.centers_panel()) {
        return;
    }
    if let Err(error) = make_panel(app, shell.centers_panel()) {
        eprintln!("digup-shell: can't make the panel: {error}");
        return;
    }
    if let Some(old) = old {
        let _ = old.destroy();
    }
}

fn show_panel(app: &AppHandle) {
    if !app.state::<Arc<Shell>>().is_set_up() {
        let _ = show_setup(app);
        return;
    }
    let Some(panel) = panel_window(app) else { return };
    // On the screen with the pointer, centered, a fifth of the way down: the page puts it there in a window that fills
    // the screen (GTK keeps the window maximized while it's hidden). Elsewhere on Wayland the desktop places it.
    if panel.label() == CENTERED_PANEL {
        let _ = panel.maximize();
    } else if let Ok(pointer) = app.cursor_position() {
        if let Ok(Some(screen)) = app.monitor_from_point(pointer.x, pointer.y) {
            let width = (PANEL_WIDTH * screen.scale_factor()) as i32;
            let x = screen.position().x + (screen.size().width as i32 - width) / 2;
            let y = screen.position().y + (screen.size().height as f64 * 0.2) as i32;
            let _ = panel.set_position(PhysicalPosition::new(x, y));
        }
    }
    let _ = panel.show();
    let _ = panel.set_focus();
    let _ = panel.emit("panel-shown", ());
    if cfg!(debug_assertions) {
        println!("panel: shown");
    }
}

fn toggle_panel(app: &AppHandle) {
    match panel_window(app) {
        Some(panel) if panel.is_visible().unwrap_or(false) => hide_panel(app.clone()),
        _ => show_panel(app),
    }
}

/// The window: searches bigger than the panel (kind filters, a grid of pictures, the preview beside it), the home
/// DigUp opens to by hand, and its menu (Choose Folders, Index Now, Settings, Quit) where a desktop has no tray. It's
/// made when it opens and gone when it closes, so a closed window costs nothing. A search the panel hands over is in
/// `window_search`: a new window takes it as it loads, an open one is told.
fn show_window(app: &AppHandle) -> tauri::Result<()> {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    if let Some(window) = app.get_webview_window(WINDOW) {
        if let Some((query, kind)) = shell.window_search.lock().unwrap().take() {
            let _ = window.emit("window-search", json!({"query": query, "kind": kind}));
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    // Debug runs look at it hidden: a window on screen takes the keyboard.
    let hidden = shell.options.debug_window.is_some() && !shell.options.debug_show;
    let (width, height) = window_size(app);
    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("window.html".into()))
        .title("DigUp")
        .inner_size(width, height)
        .min_inner_size(820.0, 520.0)
        .center()
        .visible(!hidden)
        .build()?;
    Ok(())
}

/// The window's size: 1040 × 700, or less on a small screen (a laptop's 1366 × 768 at 125% has 1093 × 614 to show
/// it in), but never under its least, 820 × 520.
fn window_size(app: &AppHandle) -> (f64, f64) {
    let screen = app.cursor_position().ok().and_then(|pointer| app.monitor_from_point(pointer.x, pointer.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(screen) = screen else { return (1040.0, 700.0) };
    let area = screen.work_area().size.to_logical::<f64>(screen.scale_factor());
    ((area.width * 0.9).clamp(820.0, 1040.0), (area.height * 0.9).clamp(520.0, 700.0))
}

/// The setup window (first launch, and Choose Folders… later). Debug runs that click through it keep it hidden.
fn show_setup(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let hidden = app.state::<Arc<Shell>>().options.debug_setup;
    WebviewWindowBuilder::new(app, "setup", WebviewUrl::App("setup.html".into()))
        .title("Set Up DigUp")
        .inner_size(720.0, 600.0)
        .resizable(false)
        .maximizable(false)
        .center()
        .visible(!hidden)
        .build()?;
    Ok(())
}

/// Settings: opening at login, centering the panel (where the desktop places windows), and where the shortcut is.
fn show_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("DigUp Settings")
        .inner_size(600.0, 460.0)
        .resizable(false)
        .maximizable(false)
        .center()
        .build()?;
    Ok(())
}

/// What the Settings window shows.
#[tauri::command]
fn settings_info(app: AppHandle, shell: State<'_, Arc<Shell>>) -> Value {
    json!({
        "openAtLogin": shell.opens_at_login() && app.autolaunch().is_enabled().unwrap_or(false),
        "canOpenAtLogin": shell.opens_at_login(),
        "centered": shell.settings.lock().unwrap().center_panel,
        "canCenter": shell.wayland,
        "hotkey": shell.hotkey_text(),
        "shortcut": {"method": shell.shortcut.name(), "command": hotkey::toggle_command()},
    })
}

#[tauri::command]
fn set_open_at_login(app: AppHandle, shell: State<'_, Arc<Shell>>, on: bool) -> Result<(), String> {
    if !shell.opens_at_login() {
        return Err("not in a dev run".into());
    }
    let autolaunch = app.autolaunch();
    (if on { autolaunch.enable() } else { autolaunch.disable() }).map_err(|error| error.to_string())
}

#[tauri::command]
fn set_center_panel(app: AppHandle, shell: State<'_, Arc<Shell>>, on: bool) -> Result<(), String> {
    {
        let mut settings = shell.settings.lock().unwrap();
        settings.center_panel = on;
        settings.save(&shell.settings_path).map_err(|error| format!("can't save the settings: {error}"))?;
    }
    let remake = app.clone();
    app.run_on_main_thread(move || remake_panel(&remake)).map_err(|error| error.to_string())
}

/// "CommandOrControl+Shift+Space" as people read it here: "Ctrl+Shift+Space", or "⇧⌘Space" on a Mac.
fn shortcut_text(accelerator: &str) -> String {
    let parts: Vec<&str> = accelerator.split('+').collect();
    if cfg!(target_os = "macos") {
        let mut text = String::new();
        for (name, sign) in [("Ctrl", "⌃"), ("Control", "⌃"), ("Alt", "⌥"), ("Option", "⌥"), ("Shift", "⇧"),
                             ("CommandOrControl", "⌘"), ("CmdOrCtrl", "⌘"), ("Cmd", "⌘"), ("Command", "⌘"),
                             ("Super", "⌘")] {
            if parts.iter().any(|part| part.eq_ignore_ascii_case(name)) && !text.contains(sign) {
                text.push_str(sign);
            }
        }
        return text + parts.last().copied().unwrap_or_default();
    }
    parts
        .iter()
        .map(|part| match *part {
            "CommandOrControl" | "CmdOrCtrl" | "Control" => "Ctrl",
            "Super" if cfg!(windows) => "Win",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

// MARK: The tray

fn make_tray(app: &AppHandle, shell: &Arc<Shell>) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    // The shortcut beside Search… only where DigUp grabs it: elsewhere the desktop has it, maybe changed there.
    let accelerator = (shell.shortcut == Method::Grab).then(|| shell.hotkey());
    let search = MenuItem::with_id(app, "search", "Search…", true, accelerator.as_deref())?;
    let open = MenuItem::with_id(app, "open", "Open DigUp", true, None::<&str>)?;
    let folders = MenuItem::with_id(app, "folders", "Choose Folders…", true, None::<&str>)?;
    let index = MenuItem::with_id(app, "index", "Index Now", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit DigUp", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&status, &separator, &search, &open, &folders, &index, &settings, &PredefinedMenuItem::separator(app)?,
          &quit_item],
    )?;
    // A Mac's menu bar takes a template (it colors it); trays elsewhere show the app's own small icon.
    #[cfg(target_os = "macos")]
    let icon = tauri::include_image!("icons/tray-template.png");
    #[cfg(not(target_os = "macos"))]
    let icon = tauri::include_image!("icons/tray.png");
    TrayIconBuilder::with_id("digup")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("DigUp")
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            "search" => show_panel(app),
            "open" => {
                if app.state::<Arc<Shell>>().is_set_up() {
                    let _ = show_window(app);
                } else {
                    let _ = show_setup(app);
                }
            }
            "folders" => {
                let _ = show_setup(app);
            }
            "index" => run_index(app),
            "settings" => {
                let _ = show_settings(app);
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Windows (and trays on Linux that report clicks): a click opens DigUp's window, as a click on an app's
            // tray icon does there; the menu is on the other button. The panel is the shortcut's.
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                if !cfg!(target_os = "macos") {
                    let app = tray.app_handle();
                    if app.state::<Arc<Shell>>().is_set_up() {
                        let _ = show_window(app);
                    } else {
                        let _ = show_setup(app);
                    }
                }
            }
        })
        .build(app)?;
    *shell.status_item.lock().unwrap() = Some(status);
    Ok(())
}

/// A problem to see in the tray's first line (the shortcut is taken…), until indexing next has news.
fn set_status(app: &AppHandle, text: &str) {
    if let Some(status) = app.state::<Arc<Shell>>().status_item.lock().unwrap().as_ref() {
        let _ = status.set_text(text);
    }
}

/// Indexing or the model moved on: the tray's first line and every page follow (status.rs has the words).
fn changed(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let snapshot = shell.snapshot();
    if let Some(status) = shell.status_item.lock().unwrap().as_ref() {
        let _ = status.set_text(snapshot["line"].as_str().unwrap_or_default());
    }
    let _ = app.emit("state", snapshot);
}

// MARK: The helpers' events

fn search_event(app: &AppHandle, event: Value) {
    if cfg!(debug_assertions) {
        println!("search: {event}");
    }
    if event["event"] == "ready" {
        if let Some(index) = event["index"].as_str() {
            let _ = app.asset_protocol_scope().allow_directory(Path::new(index).join("previews"), false);
        }
    }
    let _ = app.emit("helper", json!({"from": "search", "event": event}));
}

fn index_event(app: &AppHandle, event: Value) {
    if cfg!(debug_assertions) {
        println!("index: {event}");
    }
    app.state::<Arc<Shell>>().status.lock().unwrap().apply(&event);
    match event["event"].as_str().unwrap_or_default() {
        "file" | "rest" => reload(app, false),
        "idle" => {
            reload(app, true);
            // A debug run through setup searches once the first index is in.
            let shell = app.state::<Arc<Shell>>();
            if let Some(query) = &shell.options.debug_query {
                if shell.options.debug_setup && !shell.debug_query_sent.swap(true, Ordering::SeqCst) {
                    let label = *shell.panel_label.lock().unwrap();
                    let _ = app.emit_to(label, "debug-query", query.clone());
                }
            }
        }
        "exit" => reload(app, true),
        "ended" => index_ended(app, &event),
        _ => {}
    }
    changed(app);
    let _ = app.emit("helper", json!({"from": "index", "event": event}));
}

/// The index helper ended by itself (helper.rs's "ended"). A sync asked of it as it was leaving (it exits a minute after
/// it's done) still has to happen; its watchdog's exit (75: a file whose reading ran away, which the next helper skips)
/// starts another, though not over and over if something else is wrong (as the Mac app's `Engine.helperExited`).
fn index_ended(app: &AppHandle, event: &Value) {
    const STOPPED_ON_FILE: i64 = 75;
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let again = if event["status"].as_i64() == Some(STOPPED_ON_FILE) {
        let mut stops = shell.watchdog_stops.lock().unwrap();
        stops.retain(|stop| stop.elapsed() < Duration::from_secs(600));
        stops.push(Instant::now());
        if stops.len() > 3 {
            eprintln!("digup-shell: indexing stopped on files 4 times in 10 min");
        }
        stops.len() <= 3
    } else {
        event["owed"] == true && event["status"].as_i64() == Some(0)
    };
    if again {
        run_index(app);
    }
}

fn model_event(app: &AppHandle, event: Value) {
    if cfg!(debug_assertions) && event["event"] != "progress" {
        println!("model: {event}");
    }
    let shell = app.state::<Arc<Shell>>().inner().clone();
    let before = shell.model_state.lock().unwrap().clone();
    let state = match event["event"].as_str().unwrap_or_default() {
        "progress" => {
            let (bytes, total) = (event["all_bytes"].as_f64().unwrap_or(0.0), event["all_total"].as_f64().unwrap_or(1.0));
            json!({"state": "downloading", "bytes": bytes, "total": total})
        }
        "file" => json!({"state": "downloading", "from": event["from"]}),
        "verifying" => json!({"state": "checking"}),
        "waiting" => json!({"state": "waiting"}),
        // Where it got to stays shown while it waits to try again.
        "retry" => json!({"state": "downloading", "bytes": before["bytes"], "total": before["total"], "problem": event["message"]}),
        "done" => json!({"state": "ready"}),
        "error" => json!({"state": "failed", "message": event["message"]}),
        _ => return,
    };
    let ready = state["state"] == "ready";
    *shell.model_state.lock().unwrap() = state.clone();
    let _ = app.emit("model", state);
    changed(app);
    if ready {
        run_index(app);
    }
}

/// Has the search service read the index again: at once when indexing settles, else at most every three seconds.
fn reload(app: &AppHandle, now: bool) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
    if shell.quitting.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut reloaded = shell.reloaded.lock().unwrap();
        if !now && reloaded.elapsed() < Duration::from_secs(3) {
            return;
        }
        *reloaded = Instant::now();
    }
    tauri::async_runtime::spawn(async move {
        let reply = shell.search.ask(json!({"op": "reload"})).await;
        if cfg!(debug_assertions) {
            println!("search: reload {reply:?}");
        }
    });
}
