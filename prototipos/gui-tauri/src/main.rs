//! F13.2 prototype, Tauri 2: the services in live view, start/stop, and the tray.

// No console window: this is a program of the tray.
#![windows_subsystem = "windows"]

use std::sync::Mutex;

use crisol_client::{connect, Connection, LiveConfig, LiveState, LiveView};
use crisol_protocol::Call;
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

const CLIENT: &str = "crisol-gui-tauri-proto";

/// The last state sent to the page, for a page that opens after it.
struct Last(Mutex<Value>);

/// Makes `call` on a thread of its own (starting the daemon if the call is a start).
fn send(call: Call, then: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        if let Ok(context) = crisol_client::Context::windows(CLIENT, env!("CARGO_PKG_VERSION")) {
            if let Ok(mut opened) = connect::open(&context, &call, &mut |_| {}) {
                let _ = opened.link.call(&call, &mut |_| {});
            }
        }
        then();
    });
}

fn any_running(state: &LiveState) -> bool {
    state
        .services
        .iter()
        .any(|s| s.state == "running" || s.state == "starting")
}

fn connection_text(state: &LiveState) -> String {
    match &state.connection {
        Connection::Connected => state
            .daemon
            .as_ref()
            .map_or_else(String::new, |d| format!("crisold {}", d.daemon)),
        Connection::Disconnected => "No daemon".to_owned(),
        Connection::Refused(why) => why.clone(),
    }
}

/// `LiveState` is not serializable (the page needs JSON), so the shell turns it into JSON.
fn payload(state: &LiveState) -> Value {
    json!({
        "text": connection_text(state),
        "any_running": any_running(state),
        "services": state.services.iter().map(|s| json!({
            "name": s.name, "state": s.state, "detail": s.detail,
        })).collect::<Vec<_>>(),
    })
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn state(last: State<Last>) -> Value {
    last.0.lock().map(|v| v.clone()).unwrap_or(Value::Null)
}

#[tauri::command]
fn start(service: Option<String>) {
    send(Call::Start { service }, || {});
}

#[tauri::command]
fn stop(service: Option<String>) {
    send(Call::Stop { service }, || {});
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(Last(Mutex::new(Value::Null)))
        .invoke_handler(tauri::generate_handler![state, start, stop])
        .on_window_event(|window, event| {
            // Closing hides the window: the program lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let toggle = MenuItem::with_id(app, "toggle", "Start all", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Open Crisol", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let stop_quit =
                MenuItem::with_id(app, "stop_quit", "Stop all and quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &toggle,
                    &open,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                    &stop_quit,
                ],
            )?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().ok_or("no icon")?)
                .tooltip("Crisol")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => {
                        let running = app
                            .state::<Last>()
                            .0
                            .lock()
                            .is_ok_and(|v| v["any_running"].as_bool().unwrap_or(false));
                        send(
                            if running {
                                Call::Stop { service: None }
                            } else {
                                Call::Start { service: None }
                            },
                            || {},
                        );
                    }
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    "stop_quit" => {
                        let app = app.clone();
                        send(Call::Shutdown, move || app.exit(0));
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            let context = crisol_client::Context::windows(CLIENT, env!("CARGO_PKG_VERSION"))?;
            let handle = app.handle().clone();
            let view = LiveView::start(LiveConfig::for_context(&context), move |live| {
                let payload = payload(live);
                if let Ok(mut last) = handle.state::<Last>().0.lock() {
                    last.clone_from(&payload);
                }
                let _ = toggle.set_text(if any_running(live) {
                    "Stop all"
                } else {
                    "Start all"
                });
                let _ = handle.emit("state", payload);
            });
            app.manage(view);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the application runs");
}
