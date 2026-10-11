// DigUp's shell for Windows and Linux (tried out on the Mac first): the tray icon, the hotkey and the search panel.
// The work is DigUp's helper, `digup`, run as processes speaking JSON lines (helper.rs): `search-service` answers
// the panel, `index-service` keeps the index current. The Mac app (Sources/DigUpApp) is the reference for behavior.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod helper;
mod options;

use helper::{IndexService, Launch, SearchService};
use options::Options;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_opener::OpenerExt;

const PANEL_WIDTH: f64 = 780.0;
const FIELD_HEIGHT: f64 = 56.0;

/// What the commands and events share.
struct Shell {
    options: Options,
    search: SearchService,
    index: IndexService,
    /// The tray menu's first line: what's going on.
    status: Mutex<Option<MenuItem<Wry>>>,
    /// When the search service last reloaded the index (while indexing, at most every few seconds).
    reloaded: Mutex<Instant>,
}

/// The panel's questions for `search-service`.
#[tauri::command]
async fn ask(shell: State<'_, Arc<Shell>>, request: Value) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default();
    if !["search", "embed_ahead", "text", "render", "prewarm", "status"].contains(&op) {
        return Err(format!("not a question the panel asks: {op}"));
    }
    shell.search.ask(request).await
}

/// What indexing these folders would take, one answer per folder (`digup estimate --json`): files by kind, the first
/// pass's seconds, the rest of long files, subfolders. For onboarding and Add Folders.
#[tauri::command]
async fn estimate(shell: State<'_, Arc<Shell>>, folders: Vec<String>) -> Result<Vec<Value>, String> {
    let helper = shell.options.helper.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut command = std::process::Command::new(&helper);
        command.arg("estimate").args(&folders).arg("--json");
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
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
}

/// The panel grows to show results and shrinks back to its field.
#[tauri::command]
fn panel_height(app: AppHandle, height: f64) {
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.set_size(LogicalSize::new(PANEL_WIDTH, height.clamp(FIELD_HEIGHT, 640.0)));
    }
}

/// Debug runs: the query the page types in when it loads.
#[tauri::command]
fn debug_query(shell: State<'_, Arc<Shell>>) -> Option<String> {
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

fn main() {
    let options = match Options::from_args() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("digup-shell: {error}");
            std::process::exit(2);
        }
    };
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show_panel(app)))
        .plugin(tauri_plugin_opener::init())
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
            ask, estimate, open_path, reveal, hide_panel, panel_height, debug_query, debug_report
        ])
        .build(tauri::generate_context!())
        .expect("DigUp couldn't start");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            let shell = app.state::<Arc<Shell>>();
            shell.index.stop();
            shell.search.stop();
        }
    });
}

fn setup(app: &mut tauri::App, options: Options) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory); // the menu bar only, no Dock icon

    let handle = app.handle().clone();
    let mut common = Vec::new();
    if let Some(index) = &options.index {
        common.extend(["--index".to_string(), index.display().to_string()]);
    }
    let roots: Vec<String> = options.roots.iter().map(|root| root.display().to_string()).collect();
    let env: Vec<(String, String)> =
        options.models.iter().map(|models| ("DIGUP_MODELS".to_string(), models.display().to_string())).collect();
    let logs = app.path().app_log_dir()?;
    std::fs::create_dir_all(&logs)?;

    let search = SearchService::new(
        Launch {
            program: options.helper.clone(),
            args: [vec!["search-service".to_string()], common.clone(), roots.clone()].concat(),
            env: env.clone(),
            log: Some(logs.join("search.log")),
        },
        {
            let app = handle.clone();
            move |event| search_event(&app, event)
        },
    );
    let index = IndexService::new(
        Launch {
            program: options.helper.clone(),
            args: [vec!["index-service".to_string(), "--backfill".to_string()], common, roots].concat(),
            env,
            log: Some(logs.join("index.log")),
        },
        {
            let app = handle.clone();
            move |event| index_event(&app, event)
        },
    );
    // Previews show files from the chosen folders (and the helper's renders, once it says where the index is).
    for root in &options.roots {
        let _ = app.asset_protocol_scope().allow_directory(root, true);
    }
    let shell = Arc::new(Shell { options, search, index, status: Mutex::new(None), reloaded: Mutex::new(Instant::now()) });
    app.manage(shell.clone());

    make_panel(&handle)?;
    make_tray(&handle, &shell)?;
    if let Err(error) = app.global_shortcut().register(shell.options.hotkey.as_str()) {
        set_status(&handle, &format!("The shortcut {} is taken", shell.options.hotkey));
        eprintln!("digup-shell: can't use {} as the shortcut: {error}", shell.options.hotkey);
    }

    // The search service starts now (the first search is then quick); indexing syncs the folders.
    let warm = shell.clone();
    tauri::async_runtime::spawn(async move {
        let _ = warm.search.ask(json!({"op": "status"})).await;
    });
    if !shell.options.roots.is_empty() {
        if let Err(error) = shell.index.run() {
            set_status(&handle, &format!("Indexing couldn't start: {error}"));
        }
    } else {
        set_status(&handle, "No folders chosen yet");
    }
    if shell.options.debug_show {
        show_panel(&handle);
    }
    Ok(())
}

// MARK: The panel

fn make_panel(app: &AppHandle) -> tauri::Result<()> {
    let panel = WebviewWindowBuilder::new(app, "panel", WebviewUrl::App("index.html".into()))
        .title("DigUp")
        .inner_size(PANEL_WIDTH, FIELD_HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;
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

fn show_panel(app: &AppHandle) {
    let Some(panel) = app.get_webview_window("panel") else { return };
    // On the screen with the pointer, centered, a fifth of the way down.
    if let Ok(pointer) = app.cursor_position() {
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
}

fn toggle_panel(app: &AppHandle) {
    match app.get_webview_window("panel") {
        Some(panel) if panel.is_visible().unwrap_or(false) => hide_panel(app.clone()),
        _ => show_panel(app),
    }
}

// MARK: The tray

fn make_tray(app: &AppHandle, shell: &Arc<Shell>) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let search = MenuItem::with_id(app, "search", "Search…", true, Some(shell.options.hotkey.as_str()))?;
    let index = MenuItem::with_id(app, "index", "Index Now", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit DigUp", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &separator, &search, &index, &PredefinedMenuItem::separator(app)?, &quit_item])?;
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
            "index" => {
                if let Err(error) = app.state::<Arc<Shell>>().index.run() {
                    set_status(app, &format!("Indexing couldn't start: {error}"));
                }
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Windows and Linux: a click opens the panel, the menu is on the other button.
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                if !cfg!(target_os = "macos") {
                    toggle_panel(tray.app_handle());
                }
            }
        })
        .build(app)?;
    *shell.status.lock().unwrap() = Some(status);
    Ok(())
}

fn set_status(app: &AppHandle, text: &str) {
    if let Some(status) = app.state::<Arc<Shell>>().status.lock().unwrap().as_ref() {
        let _ = status.set_text(text);
    }
    let _ = app.emit("status", text);
}

fn quit(app: &AppHandle) {
    let shell = app.state::<Arc<Shell>>();
    shell.index.stop();
    shell.search.stop();
    app.exit(0);
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
    let count = |key: &str| event[key].as_u64().unwrap_or(0);
    match event["event"].as_str().unwrap_or_default() {
        "file" => {
            set_status(app, &format!("Indexing {} of {}", count("done"), count("total")));
            reload(app, false);
        }
        "rest" => {
            set_status(app, &format!("Reading the rest of {} long files", count("files")));
            reload(app, false);
        }
        "idle" => {
            set_status(app, &format!("Up to date · {} files", count("searchable")));
            reload(app, true);
        }
        "waiting" => set_status(app, "Waiting for the model"),
        "error" => set_status(app, &format!("Indexing: {}", event["message"].as_str().unwrap_or("error"))),
        "exit" => reload(app, true),
        _ => {}
    }
    let _ = app.emit("helper", json!({"from": "index", "event": event}));
}

/// Has the search service read the index again: at once when indexing settles, else at most every three seconds.
fn reload(app: &AppHandle, now: bool) {
    let shell = app.state::<Arc<Shell>>().inner().clone();
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
