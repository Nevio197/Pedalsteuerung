mod actions;
mod gesture;
mod hardware;
mod media;
mod model;
mod runtime;
mod updates;

use crate::actions::StreamDeckAction;
use crate::model::{Action, Config};
use crate::runtime::{AppSnapshot, Controller, Message};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WebviewWindowBuilder};

#[tauri::command]
async fn get_state(controller: State<'_, Controller>) -> Result<AppSnapshot, String> {
    controller.request(Message::State)
}

#[tauri::command]
async fn save_config(
    config: Config,
    controller: State<'_, Controller>,
) -> Result<AppSnapshot, String> {
    controller.request(|reply| Message::Save(config, reply))
}

#[tauri::command]
async fn set_paused(
    paused: bool,
    controller: State<'_, Controller>,
) -> Result<AppSnapshot, String> {
    controller.request(|reply| Message::Pause(paused, reply))
}

#[tauri::command]
async fn set_preview(
    preview: bool,
    controller: State<'_, Controller>,
) -> Result<AppSnapshot, String> {
    controller.request(|reply| Message::Preview(preview, reply))
}

#[tauri::command]
async fn set_manual_profile(
    profile_id: Option<String>,
    controller: State<'_, Controller>,
) -> Result<AppSnapshot, String> {
    controller.request(|reply| Message::Pin(profile_id, reply))
}

#[tauri::command]
async fn test_action(
    action: Action,
    controller: State<'_, Controller>,
) -> Result<AppSnapshot, String> {
    controller.request(|reply| Message::Test(action, reply))
}

#[tauri::command]
async fn list_elgato_actions(port: u16) -> Result<Vec<StreamDeckAction>, String> {
    tauri::async_runtime::spawn_blocking(move || actions::list_stream_deck_actions(port))
        .await
        .map_err(|_| "Die Stream-Deck-Verbindung ist nicht verfügbar.".to_string())?
}

#[tauri::command]
fn get_shortcut_permission() -> bool {
    actions::shortcut_permission()
}

#[tauri::command]
fn request_shortcut_permission() -> bool {
    actions::request_shortcut_permission()
}

fn legacy_path(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PEDALSTEUERUNG_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }
    let root = if cfg!(target_os = "macos") {
        app.path().data_dir()
    } else {
        app.path().local_data_dir()
    };
    root.ok()
        .map(|path| path.join("Pedalsteuerung").join("Einstellungen.json"))
}

fn start_foreground_watcher(app: &AppHandle, controller: &Controller) {
    let app = app.clone();
    let sender = controller.sender();
    thread::spawn(move || {
        loop {
            let sender = sender.clone();
            let (ack, received) = std::sync::mpsc::sync_channel(1);
            if app
                .run_on_main_thread(move || {
                    let result =
                        sender.send(Message::Foreground(actions::frontmost_bundle_identifier()));
                    let _ = ack.send(result.is_ok());
                })
                .is_err()
                || received.recv_timeout(Duration::from_secs(3)) != Ok(true)
            {
                break;
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Pedalsteuerung öffnen", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause umschalten", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pause, &quit])?;
    let mut builder = TrayIconBuilder::with_id("pedalsteuerung")
        .menu(&menu)
        .tooltip("Pedalsteuerung 2")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_window(app),
            "pause" => {
                let _ = app
                    .state::<Controller>()
                    .sender()
                    .send(Message::TogglePause);
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let handle = app.handle();
            let path = match std::env::var_os("PEDALSTEUERUNG_V2_CONFIG_PATH") {
                Some(path) => PathBuf::from(path),
                None => handle.path().app_config_dir()?.join("Einstellungen.json"),
            };
            let controller = Controller::start(handle.clone(), path, legacy_path(handle));
            start_foreground_watcher(handle, &controller);
            app.manage(controller);
            create_tray(handle)?;
            app.manage(updates::Updates::new(
                handle.package_info().version.to_string(),
            ));
            let window = handle
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == "main")
                .ok_or("Das Hauptfenster fehlt in der App-Konfiguration.")?;
            WebviewWindowBuilder::from_config(app, window)?
                .on_web_resource_request(media::serve_video_range)
                .build()?;
            updates::start(handle.clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_config,
            set_paused,
            set_preview,
            set_manual_profile,
            test_action,
            list_elgato_actions,
            get_shortcut_permission,
            request_shortcut_permission,
            updates::get_update_status,
            updates::check_for_update,
            updates::install_update,
            updates::open_feedback,
        ])
        .build(tauri::generate_context!())
        .expect("Pedalsteuerung konnte nicht gestartet werden");

    app.run(|app, event| match event {
        RunEvent::ExitRequested { .. } => {
            if let Some(controller) = app.try_state::<Controller>() {
                controller.stop();
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => show_window(app),
        _ => {}
    });
}
