#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod runner;
mod tray;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dupli_core::config::{AppSettings, PairConfig};
use dupli_core::history::{HistEntry, Pruned, RestoreReport};
use dupli_core::volumes::{self, VolumeInfo};
use dupli_core::Side;
use runner::{AppView, Engine, GroupKey, HistoryView, PlanFile, PlanGroups, RunRecord, Signal, Want};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt as _;
use tauri_plugin_notification::NotificationExt as _;

struct Shell {
    engine: Arc<Engine>,
    tray: OnceLock<tray::Tray>,
    start_hidden: bool,
}

type Fallible<T> = Result<T, String>;

pub(crate) fn show_window(app: &AppHandle, select: Option<&str>) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if let Some(id) = select {
            let _ = w.emit("select", id);
        }
        let _ = w.emit("window-visible", true);
    }
}

// The application continues in the tray.
fn hide_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.emit("window-visible", false);
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(false);
}

fn on_signal(app: &AppHandle, signal: Signal) {
    let Some(shell) = app.try_state::<Shell>() else { return };
    match signal {
        Signal::State => {
            let _ = app.emit("state", shell.engine.view());
            if let Some(tray) = shell.tray.get() {
                tray.update(app, &shell.engine.tray());
            }
        }
        Signal::Notify { title, body } => {
            if shell.engine.settings().notifications {
                let _ = app.notification().builder().title(title).body(body).show();
            }
        }
    }
}

#[tauri::command]
fn state(shell: State<Shell>) -> AppView {
    shell.engine.view()
}

#[tauri::command]
fn blank_pair() -> PairConfig {
    PairConfig::default()
}

#[tauri::command]
fn save_pair(shell: State<Shell>, pair: PairConfig) -> Fallible<String> {
    shell.engine.save_pair(pair)
}

#[tauri::command]
fn remove_pair(shell: State<Shell>, id: String) {
    shell.engine.remove_pair(&id);
}

#[tauri::command]
fn reorder_pairs(shell: State<Shell>, ids: Vec<String>) {
    shell.engine.reorder(&ids);
}

#[tauri::command]
fn compare(shell: State<Shell>, id: String) {
    shell.engine.request(&id, Want::Compare);
}

#[tauri::command]
fn sync(shell: State<Shell>, id: String, confirmed: bool) {
    shell.engine.request(&id, Want::Sync { confirmed });
}

#[tauri::command]
fn stop(shell: State<Shell>, id: String) {
    shell.engine.stop(&id);
}

#[tauri::command]
fn sync_all(shell: State<Shell>) {
    shell.engine.sync_all();
}

#[tauri::command]
fn plan_groups(shell: State<Shell>, id: String) -> PlanGroups {
    shell.engine.plan_groups(&id)
}

#[tauri::command]
fn plan_files(shell: State<Shell>, id: String, group: GroupKey, offset: usize, limit: usize) -> Vec<PlanFile> {
    shell.engine.plan_files(&id, &group, offset, limit)
}

#[tauri::command]
fn activity(shell: State<Shell>, id: String) -> Vec<RunRecord> {
    shell.engine.activity(&id)
}

async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> Fallible<T> + Send + 'static) -> Fallible<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn history(shell: State<'_, Shell>, id: String) -> Fallible<HistoryView> {
    let engine = shell.engine.clone();
    off_thread(move || engine.history(&id)).await
}

#[tauri::command]
async fn history_entries(shell: State<'_, Shell>, id: String, side: Side, run: String) -> Fallible<Vec<HistEntry>> {
    let engine = shell.engine.clone();
    off_thread(move || engine.history_entries(&id, side, &run)).await
}

#[tauri::command]
async fn history_restore(
    shell: State<'_, Shell>,
    id: String,
    side: Side,
    run: String,
    rels: Vec<String>,
) -> Fallible<RestoreReport> {
    let engine = shell.engine.clone();
    off_thread(move || engine.history_restore(&id, side, &run, &rels)).await
}

#[tauri::command]
async fn history_clear(shell: State<'_, Shell>, id: String) -> Fallible<Pruned> {
    let engine = shell.engine.clone();
    off_thread(move || engine.history_clear(&id)).await
}

#[tauri::command]
fn history_reveal(shell: State<Shell>, id: String, side: Side, run: Option<String>, rel: Option<String>) -> Fallible<()> {
    let path = shell.engine.history_path(&id, side, run.as_deref(), rel.as_deref())?;
    if !path.exists() {
        return Err("Nothing has been kept there yet.".into());
    }
    if rel.is_some() {
        tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
    } else {
        tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn open_folder(shell: State<Shell>, id: String, side: Side) -> Fallible<()> {
    let view = shell.engine.view();
    let pair = view.pairs.iter().find(|p| p.config.id == id).ok_or("That pair no longer exists.")?;
    let path = if side == Side::Source { &pair.config.source } else { &pair.config.copy };
    if !Path::new(path).is_dir() {
        return Err("That folder is not connected right now.".into());
    }
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn eject(shell: State<'_, Shell>, id: String) -> Fallible<String> {
    let engine = shell.engine.clone();
    off_thread(move || engine.eject(&id)).await
}

#[derive(Serialize)]
struct FolderInfo {
    path: String,
    name: String,
    reachable: bool,
    volume: Option<VolumeInfo>,
    items: Option<u32>,
}

fn describe(path: &str) -> FolderInfo {
    let p = Path::new(path);
    let reachable = p.is_dir();
    let items = reachable.then(|| {
        std::fs::read_dir(p)
            .map(|list| {
                list.flatten()
                    .filter(|e| e.file_name().to_str().is_none_or(|n| n != ".dupli" && !dupli_core::scan::is_junk(n)))
                    .take(1000)
                    .count() as u32
            })
            .unwrap_or(0)
    });
    FolderInfo {
        path: path.to_owned(),
        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned()),
        reachable,
        volume: if reachable { volumes::volume_info(p) } else { None },
        items,
    }
}

#[tauri::command]
fn describe_folder(path: String) -> FolderInfo {
    describe(&path)
}

#[tauri::command]
async fn choose_folder(window: tauri::WebviewWindow, title: String, start: Option<String>) -> Option<FolderInfo> {
    let mut dialog = window.dialog().file().set_title(title).set_parent(&window);
    if let Some(dir) = start.filter(|s| Path::new(s).is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    let picked = dialog.blocking_pick_folder()?.into_path().ok()?;
    Some(describe(&picked.to_string_lossy()))
}

#[tauri::command]
fn set_settings(app: AppHandle, shell: State<Shell>, settings: AppSettings) -> Fallible<()> {
    let login = settings.launch_at_login;
    shell.engine.set_settings(settings);
    apply_login_item(&app, login)
}

fn apply_login_item(app: &AppHandle, enabled: bool) -> Fallible<()> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let auto = app.autolaunch();
    if auto.is_enabled().unwrap_or(false) == enabled {
        return Ok(());
    }
    let done = if enabled { auto.enable() } else { auto.disable() };
    done.map_err(|e| format!("Couldn't change the login item: {e}"))
}

#[tauri::command]
fn ui_ready(app: AppHandle, shell: State<Shell>) -> bool {
    if !shell.start_hidden {
        show_window(&app, None);
    }
    !shell.start_hidden
}

#[tauri::command]
fn window_minimize(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn window_close(window: tauri::WebviewWindow) {
    let _ = window.close();
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn ui_log(text: String) {
    eprintln!("[ui] {text}");
}

// Debug only: write an image of the window to a file.
#[tauri::command]
fn debug_snapshot(window: tauri::WebviewWindow, path: String) -> Fallible<()> {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        use block2::RcBlock;
        use objc2_app_kit::NSImage;
        use objc2_foundation::NSError;
        use objc2_web_kit::WKWebView;
        return window
            .with_webview(move |wv| unsafe {
                let view = &*(wv.inner() as *const WKWebView);
                let done = RcBlock::new(move |img: *mut NSImage, _: *mut NSError| {
                    if let Some(tiff) = img.as_ref().and_then(|i| i.TIFFRepresentation()) {
                        let _ = std::fs::write(&path, tiff.to_vec());
                    }
                });
                view.takeSnapshotWithConfiguration_completionHandler(None, &done);
            })
            .map_err(|e| e.to_string());
    }
    #[allow(unreachable_code)]
    {
        let _ = (window, path);
        Err("Snapshots are only available in debug builds on macOS.".into())
    }
}

fn build_window(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Dupli")
        .inner_size(1040.0, 700.0)
        .min_inner_size(900.0, 620.0)
        .visible(false);
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(20.0, 26.0));
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(false).shadow(true);
    builder.build()
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app, None)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            let mut dir = app.path().app_config_dir()?;
            if cfg!(debug_assertions) {
                if let Ok(home) = std::env::var("DUPLI_HOME") {
                    dir = home.into();
                }
            }
            let sink = handle.clone();
            let engine = Engine::start(dir, move |signal| on_signal(&sink, signal));
            let start_hidden = std::env::args().any(|a| a == "--hidden") && engine.settings().keep_in_tray;
            app.manage(Shell { engine: engine.clone(), tray: OnceLock::new(), start_hidden });

            let _ = apply_login_item(&handle, engine.settings().launch_at_login);

            let tray = tray::Tray::new(&handle, &engine.tray())?;
            let _ = handle.state::<Shell>().tray.set(tray);
            let for_menu = engine.clone();
            app.on_menu_event(move |app, event| match event.id().as_ref() {
                tray::OPEN => show_window(app, None),
                tray::SYNC_ALL => for_menu.sync_all(),
                tray::PAUSE => {
                    let mut s = for_menu.settings();
                    s.auto_paused = !s.auto_paused;
                    for_menu.set_settings(s);
                }
                tray::QUIT => app.exit(0),
                id => {
                    if let Some(pair) = id.strip_prefix(tray::PAIR) {
                        show_window(app, Some(pair));
                    }
                }
            });

            let window = build_window(&handle)?;
            if start_hidden {
                hide_window(&handle);
            } else {
                let late = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2500));
                    if late.get_webview_window("main").is_some_and(|w| !w.is_visible().unwrap_or(true)) {
                        show_window(&late, None);
                    }
                });
            }

            // Debug only: start a script in the window.
            #[cfg(debug_assertions)]
            if let Ok(js) = std::env::var("DUPLI_EVAL") {
                let win = window.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1800));
                    let _ = win.eval(js);
                });
            }
            let _ = window;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if app.state::<Shell>().engine.settings().keep_in_tray {
                    api.prevent_close();
                    hide_window(app);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            state,
            blank_pair,
            save_pair,
            remove_pair,
            reorder_pairs,
            compare,
            sync,
            stop,
            sync_all,
            plan_groups,
            plan_files,
            activity,
            history,
            history_entries,
            history_restore,
            history_clear,
            history_reveal,
            open_folder,
            eject,
            describe_folder,
            choose_folder,
            set_settings,
            ui_ready,
            window_minimize,
            window_close,
            quit,
            ui_log,
            debug_snapshot
        ])
        .build(tauri::generate_context!())
        .expect("Dupli could not start");

    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_window(app, None),
        _ => {
            let _ = app;
        }
    });
}
