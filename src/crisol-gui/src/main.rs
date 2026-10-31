//! The tray and the window of Crisol (ADR-0026). This program draws what `crisol-client` says
//! and turns clicks into calls; it decides nothing about Crisol.
//!
//! - The tray shows the general state and offers Start/Stop all, Open, Quit, and Stop all and
//!   quit. Closing the window hides it; the daemon is independent of this program.
//! - One instance per user: opening it again shows the window that exists.
//! - The page (`ui/`) has no bundler: plain HTML, CSS and JavaScript over the global `__TAURI__`.

// No console window: this is a program of the tray.
#![windows_subsystem = "windows"]

mod i18n;
mod tray;

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use crisol_client::{
    connect, Context, GuiSettings, Language, LeaveAction, LiveConfig, LiveState, LiveView, Summary,
};
use crisol_protocol::{Call, RpcError};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use crate::i18n::Dictionary;
use crate::tray::{id, Tray};

/// Who this program is, as the daemon's `hello` hears it.
const CLIENT: &str = "crisol-gui";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often to look at the taskbar's theme: Windows tells nobody when it changes.
const THEME_POLL: Duration = Duration::from_secs(2);

/// The handle of the app, for the thread of the live view.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// How many times the state was published. It is taken, and the state read, under this lock:
/// two threads that publish (the live view, `setup`, `bootstrap`) cannot leave an older state
/// drawn after a newer one, and the page keeps the highest `revision` it saw.
static PUBLISHED: Mutex<u64> = Mutex::new(0);

/// What the page draws: the state and what was decided from it.
#[derive(Debug, Clone, Serialize)]
struct Payload {
    revision: u64,
    state: LiveState,
    summary: Summary,
}

/// An error as the page shows it: the words of the core, with the fix when there is one.
#[derive(Debug, Clone, Serialize)]
struct Failure {
    message: String,
    fix: Option<String>,
}

impl From<RpcError> for Failure {
    fn from(error: RpcError) -> Self {
        Self {
            message: error.message,
            fix: error.fix,
        }
    }
}

/// Everything the page needs on opening: the words, and the state as it is now.
#[derive(Debug, Serialize)]
struct Bootstrap<'a> {
    words: &'a Dictionary,
    #[serde(flatten)]
    now: Payload,
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The page shows what failed, with the window brought up so that a click on the tray is not
/// left without an answer.
fn report(app: &AppHandle, failure: Failure) {
    show_main(app);
    let _ = app.emit("call_failed", failure);
}

/// The state as it is now, numbered; the tray is set to it on the way. `None` while the view
/// is not managed yet (its first change can come before `setup` manages it).
fn take_picture(app: &AppHandle) -> Option<(MutexGuard<'static, u64>, Payload)> {
    let view = app.try_state::<LiveView>()?;
    let mut published = PUBLISHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *published += 1;
    let state = view.state();
    let summary = state.summary();
    if let Some(tray) = app.try_state::<Tray>() {
        tray.show(&summary);
    }
    let now = Payload {
        revision: *published,
        state,
        summary,
    };
    Some((published, now))
}

/// The live state changed: the page and the tray follow it. What is sent is read again here,
/// not the `_changed` the view passed, so the last thing sent is never older than the view.
fn on_state(_changed: &LiveState) {
    let Some(app) = APP.get() else {
        return;
    };
    if let Some((_published, now)) = take_picture(app) {
        let _ = app.emit("state", now);
    }
}

/// Makes `call` on a thread of its own: it may start the daemon, which takes a moment.
fn send(app: &AppHandle, call: Call) {
    let app = app.clone();
    std::thread::spawn(move || {
        let result = Context::windows(CLIENT, VERSION)
            .map_err(|e| Failure {
                message: e.to_string(),
                fix: None,
            })
            .and_then(|context| connect::call(&context, &call, &mut |_| {}).map_err(Failure::from));
        if let Err(failure) = result {
            report(&app, failure);
        }
    });
}

/// The state of the view; the empty one ("no daemon") in the instant before `setup` starts it,
/// when the tray already answers clicks.
fn live_state(app: &AppHandle) -> LiveState {
    app.try_state::<LiveView>()
        .map(|view| view.state())
        .unwrap_or_default()
}

fn toggle_all(app: &AppHandle) {
    let toggle = live_state(app).summary().toggle;
    send(app, toggle.call());
}

/// "Quit": the interface goes, the services stay, and the first time it is said.
fn request_quit(app: &AppHandle) {
    let told = crisol_platform::resolve_paths()
        .is_ok_and(|paths| GuiSettings::load(&paths).exit_notice_shown);
    match live_state(app).leave_action(told) {
        LeaveAction::Leave => app.exit(0),
        LeaveAction::WarnFirst => {
            show_main(app);
            let _ = app.emit("exit_notice", ());
        }
    }
}

/// "Stop all and quit": `crisol shutdown`, and then the interface goes. It does not ask the
/// view whether there is a daemon: the view may not have found it yet, or may be refused by
/// one that runs; with no daemon `stop_daemon` answers "nothing was running" by itself.
fn stop_all_and_quit(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let stopped = Context::windows(CLIENT, VERSION)
            .map_err(|e| e.to_string())
            .and_then(|context| connect::stop_daemon(&context, &mut |_| {}));
        match stopped {
            Ok(()) => app.exit(0),
            Err(message) => report(&app, Failure { message, fix: None }),
        }
    });
}

fn on_entry(app: &AppHandle, entry: &str) {
    match entry {
        id::TOGGLE => toggle_all(app),
        id::OPEN => show_main(app),
        id::QUIT => request_quit(app),
        id::STOP_QUIT => stop_all_and_quit(app),
        _ => {}
    }
}

// Tauri hands its arguments to a command by value.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn bootstrap(app: AppHandle, words: State<Dictionary>) -> Result<serde_json::Value, String> {
    let (_published, now) = take_picture(&app).ok_or("the live view is not running yet")?;
    serde_json::to_value(Bootstrap { words: &words, now }).map_err(|e| e.to_string())
}

/// The page's "Quit anyway" on the one-time notice.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn quit_confirmed(app: AppHandle) {
    if let Ok(paths) = crisol_platform::resolve_paths() {
        let _ = GuiSettings {
            exit_notice_shown: true,
        }
        .save(&paths);
    }
    app.exit(0);
}

fn main() {
    let words = Dictionary::of(Language::from_preferences(crisol_platform::ui_languages()));

    // Without WebView2 there is no window to say anything in.
    if crisol_platform::webview2_version().is_none() {
        crisol_platform::error_box(&words.get("webview2.title"), &words.get("webview2.body"));
        std::process::exit(1);
    }

    let context = match Context::windows(CLIENT, VERSION) {
        Ok(context) => context,
        Err(error) => {
            crisol_platform::error_box("Crisol", &error.to_string());
            std::process::exit(1);
        }
    };
    let live = LiveConfig::for_context(&context);

    tauri::Builder::default()
        // The first plugin, so that a second run is answered before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(words.clone())
        .invoke_handler(tauri::generate_handler![bootstrap, quit_confirmed])
        .on_window_event(|window, event| {
            // Closing hides the window: the program lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // A pending "the services keep running" question is not asked to a hidden window.
                let _ = window.emit("exit_notice_dismiss", ());
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let light = crisol_platform::taskbar_uses_light_theme();
            let tray = Tray::build(&handle, &words, light, show_main, on_entry)?;
            app.manage(tray);
            let _ = APP.set(handle.clone());
            // The view starts here, after the single-instance plugin: a second run, which only
            // shows the first one's window, never connects to the daemon. A change before
            // `manage` is not lost: the `on_state` below reads the view again.
            app.manage(LiveView::start(live, on_state));
            on_state(&LiveState::default());
            std::thread::spawn(move || loop {
                std::thread::sleep(THEME_POLL);
                if let Some(tray) = handle.try_state::<Tray>() {
                    tray.set_light_taskbar(crisol_platform::taskbar_uses_light_theme());
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the application runs");
}
