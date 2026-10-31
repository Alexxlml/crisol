//! F13.2 prototype, Slint: the services in live view, start/stop, and the tray.

// No console window: this is a program of the tray.
#![windows_subsystem = "windows"]

mod single;

use crisol_client::{connect, Connection, Context, LiveConfig, LiveState, LiveView};
use crisol_protocol::Call;
use slint::{ComponentHandle, ModelRc, VecModel};

slint::include_modules!();

const CLIENT: &str = "crisol-gui-slint-proto";

/// Makes `call` on a thread of its own (starting the daemon if the call is a start).
fn send(call: Call, then: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        if let Ok(context) = Context::windows(CLIENT, env!("CARGO_PKG_VERSION")) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let waiter = match single::acquire()? {
        single::Instance::Second => return Ok(()),
        single::Instance::First(waiter) => waiter,
    };
    let context = Context::windows(CLIENT, env!("CARGO_PKG_VERSION"))?;
    let window = MainWindow::new()?;
    let tray = AppTray::new()?;

    window.on_start(|name| {
        send(
            Call::Start {
                service: Some(name.into()),
            },
            || {},
        );
    });
    window.on_stop(|name| {
        send(
            Call::Stop {
                service: Some(name.into()),
            },
            || {},
        );
    });
    let toggle = {
        let tray = tray.as_weak();
        move || {
            let running = tray.upgrade().is_some_and(|t| t.get_any_running());
            send(
                if running {
                    Call::Stop { service: None }
                } else {
                    Call::Start { service: None }
                },
                || {},
            );
        }
    };
    window.on_toggle_all(toggle.clone());
    tray.on_toggle_all(toggle);
    let open = {
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                let _ = window.show();
            }
        }
    };
    tray.on_open(open);
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    tray.on_stop_and_quit(|| {
        send(Call::Shutdown, || {
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
        });
    });
    {
        let window = window.as_weak();
        waiter.on_second(move || {
            let window = window.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = window.upgrade() {
                    let _ = window.show();
                }
            });
        });
    }

    let (weak_window, weak_tray) = (window.as_weak(), tray.as_weak());
    let _view = LiveView::start(LiveConfig::for_context(&context), move |state| {
        let state = state.clone();
        let (window, tray) = (weak_window.clone(), weak_tray.clone());
        let _ = slint::invoke_from_event_loop(move || {
            let rows: Vec<ServiceRow> = state
                .services
                .iter()
                .map(|s| ServiceRow {
                    name: s.name.clone().into(),
                    state: s.state.clone().into(),
                    detail: s.detail.clone().unwrap_or_default().into(),
                })
                .collect();
            let running = any_running(&state);
            if let Some(window) = window.upgrade() {
                window.set_services(ModelRc::new(VecModel::from(rows)));
                window.set_connection(connection_text(&state).into());
                window.set_any_running(running);
            }
            if let Some(tray) = tray.upgrade() {
                tray.set_any_running(running);
                tray.set_summary(connection_text(&state).into());
            }
        });
    });

    tray.show()?;
    window.show()?;
    // Closing the window hides it (the default for Slint); the tray keeps the loop alive.
    slint::run_event_loop_until_quit()?;
    Ok(())
}
