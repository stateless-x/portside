//! Tauri entry point. Wires the platform/domain seam to the scanner loop, probe, stop
//! flow, and IPC surface (see docs/PLAN.md). The UI itself lives in src/ and talks to
//! this crate only through the frozen docs/IPC.md contract.

pub mod commands;
pub mod domain;
pub mod ipc;
pub mod keeplist;
pub mod platform;
pub mod probe;
pub mod scanner;

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
#[cfg(target_os = "macos")]
use tauri::ActivationPolicy;
use tauri::Manager;

use commands::AppState;
use scanner::{ClassifyDeps, ScannerState, Waker};

/// `Arc`, not `Box`: `commands.rs`'s blocking command handlers need an owned,
/// cheaply-cloned, `'static` handle to move into `tauri::async_runtime::spawn_blocking`
/// (see that module's doc comment for why sync work must run there rather than on a
/// tokio worker thread). The scan loop's own copy (constructed separately below, on
/// its own dedicated thread) doesn't need to share this one.
#[cfg(target_os = "macos")]
fn make_process_source() -> Arc<dyn platform::ProcessSource + Send + Sync> {
    Arc::new(platform::macos::MacosProcessSource)
}

/// P1/P2: only macos.rs is a real gathering backend today. A non-macOS build must
/// still compile (Windows/Linux plug in later per REQUIREMENTS.md Portability) — this
/// stub reports no listeners rather than failing the build, since there is nothing
/// else it could honestly do without a real platform backend.
#[cfg(not(target_os = "macos"))]
fn make_process_source() -> Arc<dyn platform::ProcessSource + Send + Sync> {
    struct NullProcessSource;
    impl platform::ProcessSource for NullProcessSource {
        fn enumerate(&self) -> Result<Vec<platform::RawListener>, String> {
            Ok(Vec::new())
        }
        fn owning_app(&self, _exe: &std::path::Path) -> Option<String> {
            None
        }
        fn request_stop(&self, _pid: u32) -> Result<(), String> {
            Err("not supported on this platform yet".to_string())
        }
        fn force_stop(&self, _pid: u32) -> Result<(), String> {
            Err("not supported on this platform yet".to_string())
        }
    }
    Arc::new(NullProcessSource)
}

pub fn classify_deps() -> ClassifyDeps<'static> {
    ClassifyDeps { owning_app: &owning_app, path_exists: &|p| p.exists() }
}

fn owning_app(exe: &std::path::Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use platform::ProcessSource;
        platform::macos::MacosProcessSource.owning_app(exe)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exe;
        None
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Registered FIRST, which the plugin's own documentation requires — a second
        // launch must be intercepted before any other plugin or the setup hook has
        // started doing work on its behalf.
        //
        // Portside must never run twice: two instances mean two tray icons showing two
        // counts, two scan loops paying N1's cost twice, and two writers racing on the
        // one persisted file (the Keeplist, F10). A menu bar app also invites the
        // mistake, since it has no dock icon to show it is already running — the user
        // launches it again precisely because they cannot see the first one.
        //
        // So a second launch is not an error to report; it is the user asking to see
        // the panel. Show and focus the existing window, which is exactly what the
        // tray's "Open Portside" item does.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        // Closing the panel (red button, ⌘W) must hide it, not destroy it — a
        // destroyed window cannot be shown again, which would leave the tray's
        // "Open Portside" item pointing at nothing for the rest of the app's life.
        // Hiding also blurs the webview, so panel_closed fires and the scan cadence
        // drops back to 15s (src/main.js).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::panel_opened,
            commands::panel_closed,
            commands::refresh_now,
            commands::set_keep_running,
            commands::stop_server,
            commands::stop_all_dev_servers,
            commands::force_stop,
            commands::open_project,
        ])
        .setup(|app| {
            // A menu bar app has no dock icon or app-switcher entry — only the tray
            // icon. Without this, macOS treats it as a regular windowed app and
            // shows a dock icon, which is not what F7's "resident menu bar
            // Indicator" describes.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(ActivationPolicy::Accessory);

            let app_data_dir = app.path().app_data_dir().map_err(|e| format!("could not resolve app data dir: {e}"))?;
            let scanner_state = Arc::new(Mutex::new(ScannerState::new(app_data_dir)));
            let waker = Arc::new(Waker::new());
            let source = make_process_source();

            app.manage(AppState { scanner: scanner_state.clone(), source, waker: waker.clone() });

            // The window ships `visible: false` (tauri.conf.json) — a menu bar app
            // must not flash a window at login. This menu item is the ONLY path that
            // ever shows it; without it the panel is unreachable and the app is just
            // a counter with a Quit button.
            let open = MenuItem::with_id(app, "open", "Open Portside", true, None::<&str>)?;
            // Settings and Help live HERE, in the menu bar, not as titlebar buttons —
            // the user's chosen home for them. Each shows the panel and tells the
            // webview which page to open (the "navigate" event, src/main.js).
            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let help = MenuItem::with_id(app, "help", "Help", true, None::<&str>)?;
            let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &settings, &help, &separator, &quit])?;

            // F7: tray id "main" so commands::emit_snapshot can find it again after a
            // scan. Starts at "0" — the real count arrives with the loop's first
            // completed scan, typically within one PANEL_CLOSED cadence of startup.
            // The menu bar mark is the user's clay-lighthouse silhouette as a macOS
            // template image (pure black + alpha; the OS recolors it for light/dark
            // bars and the pressed state). 44px = 22pt @2x, sharp on retina bars.
            // Falls back to the window icon if the PNG ever fails to decode, because
            // a tray app with no tray icon is unreachable.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray@2x.png"))
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .icon_as_template(true)
                .title("0")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        // "main" is Tauri's default label for the single configured
                        // window. Showing + focusing fires the webview's focus
                        // handler, which calls panel_opened and switches the scan
                        // cadence to 3s (src/main.js).
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "settings" | "help" => {
                        // Show first, then navigate: the event is only useful with
                        // the panel visible, and emit is fire-and-forget — if the
                        // webview is still booting and misses it, the user still
                        // gets the panel (the graceful half of the feature).
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        use tauri::Emitter;
                        let _ = app.emit("navigate", event.id.as_ref());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // The adaptive scan loop (phase 3) runs on its own thread for the life of
            // the app. It owns no Tauri types directly beyond the cloned AppHandle
            // needed to emit events and update the tray — everything else it touches
            // is the shared ScannerState behind its own mutex, so it never blocks on
            // (or is blocked by) a command handler for longer than one lock
            // acquisition.
            let app_handle = app.handle().clone();
            let loop_state = scanner_state.clone();
            let loop_waker = waker.clone();
            // `ProcessSource` for macOS shells out to `lsof`/`ps`, which is exactly
            // the kind of blocking I/O `std::thread::spawn` (not an async task) is
            // for — there is no tokio runtime in this crate to spawn an async task on
            // in the first place (see docs/PLAN.md: no async runtime dependency).
            std::thread::spawn(move || {
                let source = make_process_source();
                let deps = classify_deps();
                scanner::run_loop(
                    &loop_state,
                    source.as_ref(),
                    &deps,
                    &loop_waker,
                    |snapshot| commands::emit_snapshot(&app_handle, &snapshot),
                    || false,
                );
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
