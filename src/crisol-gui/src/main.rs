//! The tray and the window of Crisol (ADR-0026). This program draws what `crisol-client` says
//! and turns clicks into calls; it decides nothing about Crisol.
//!
//! - The tray shows the general state and offers Start/Stop all, Open, Quit, and Stop all and
//!   quit. Closing the window hides it; the daemon is independent of this program.
//! - One instance per user: opening it again shows the window that exists, and opening it
//!   again with `--quit` closes it (what `crisol uninstall --all` does before deleting it).
//! - The page (`ui/`) has no bundler: plain HTML, CSS and JavaScript over the global `__TAURI__`.

// No console window: this is a program of the tray.
#![windows_subsystem = "windows"]

mod i18n;
mod tray;

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use crisol_client::{
    clean_env, connect, credential_services, current_terminal_options, fetch_credentials,
    fetch_doctor, fetch_log, fetch_setup, fetch_sites, fetch_uninstall, fetch_versions,
    log_services, open_site as open_in_browser, run_doctor_action, run_setup, run_uninstall,
    run_version_act, suggested_site_name, trust_preview, Act, ActionDone, Context, CredentialsView,
    DoctorView, GuiSettings, Language, LeaveAction, LiveConfig, LiveState, LiveView, LogUpdate,
    Opened, Place, SetupDone, SetupView, SitesView, Summary, TerminalOptions, TrustPreview,
    UninstallDone, UninstallView, VersionAct, VersionsView, QUIT_ARG,
};
use crisol_protocol::{Call, CheckAction, Reply, RpcError};
use serde::Serialize;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};

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
    // The rows know which data services are switched on from `crisol.toml`; with no home to
    // read (a test) they simply have no switch.
    let summary = match crisol_platform::resolve_paths() {
        Ok(paths) => state.summary_at(&paths),
        Err(_) => state.summary(),
    };
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

/// Draws the state again without waiting for an event: for what changes without one (the
/// switch of a service lives in `crisol.toml`).
fn publish(app: &AppHandle) {
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

/// What a call that worked says, for the page to show.
#[derive(Debug, Serialize)]
struct Done {
    message: String,
    warnings: Vec<String>,
}

/// The folder a person picked, with the site name its folder suggests.
#[derive(Debug, Serialize)]
struct Picked {
    path: String,
    name: String,
}

fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        fix: None,
    }
}

/// Runs `work` on a thread that may block: a call can start the daemon, and a dialog waits for
/// a person.
async fn blocking<T, F>(work: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce(&Context) -> Result<T, Failure> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let context = Context::windows(CLIENT, VERSION).map_err(|e| failure(e.to_string()))?;
        work(&context)
    })
    .await
    .map_err(|e| failure(e.to_string()))?
}

/// A click of the services or sites screens: the call it stands for, as the command line
/// would make it. When it worked, the state is published again: a switch (`enable`) changes
/// `crisol.toml`, and no event tells anybody.
#[tauri::command]
async fn act(app: AppHandle, act: Act) -> Result<Done, Failure> {
    let call = act.call();
    let done =
        blocking(move |context| connect::call(context, &call, &mut |_| {}).map_err(Failure::from))
            .await?;
    publish(&app);
    Ok(match done {
        Reply::Done(info) => Done {
            message: info.message,
            warnings: info.warnings,
        },
        // `start` answers with the status; what it has to say (a database that could not start)
        // is a warning, as the command line prints it.
        Reply::Status(info) => Done {
            message: String::new(),
            warnings: info.warnings,
        },
        _ => Done {
            message: String::new(),
            warnings: Vec::new(),
        },
    })
}

#[tauri::command]
async fn sites() -> Result<SitesView, Failure> {
    blocking(|context| fetch_sites(context).map_err(Failure::from)).await
}

/// What is installed and what could be, with the buttons of each row.
#[tauri::command]
async fn versions() -> Result<VersionsView, Failure> {
    blocking(|context| fetch_versions(context).map_err(Failure::from)).await
}

/// A click of the versions screen. An installation tells the page how far it is (the
/// `install_progress` event) while the daemon downloads; the answer is the daemon's own
/// (`installed`, `removed` or `used`), for the page to word. The daemon finishes an installation
/// whatever becomes of this window.
#[tauri::command]
async fn version_act(app: AppHandle, act: VersionAct) -> Result<Reply, Failure> {
    let progress_to = app.clone();
    let reply = blocking(move |context| {
        run_version_act(context, &act, &mut |progress| {
            let _ = progress_to.emit("install_progress", progress);
        })
        .map_err(Failure::from)
    })
    .await?;
    // `use` writes `crisol.toml`, and no event tells anybody.
    publish(&app);
    Ok(reply)
}

/// What `doctor` found, with the button each finding offers (ADR-0021 §5).
#[tauri::command]
async fn doctor() -> Result<DoctorView, Failure> {
    blocking(|context| fetch_doctor(context).map_err(Failure::from)).await
}

/// What the CA is and what Windows will ask about it, before the second click of the trust
/// button. It makes the CA if there is none, as `crisol trust` does; nothing is added to the
/// store.
#[tauri::command]
async fn doctor_trust_preview() -> Result<TrustPreview, Failure> {
    blocking(|context| trust_preview(context).map_err(Failure::from)).await
}

/// A button of the diagnostic screen. The action is the one the screen was given, checked again
/// here (`run_doctor_action`); the dialogs of Windows and the permission prompt come up from
/// this process. An installation tells the page how far it is, as in the versions screen.
#[tauri::command]
async fn doctor_act(app: AppHandle, action: CheckAction) -> Result<ActionDone, Failure> {
    let progress_to = app.clone();
    let done = blocking(move |context| {
        run_doctor_action(context, &action, &mut |progress| {
            let _ = progress_to.emit("install_progress", progress);
        })
        .map_err(Failure::from)
    })
    .await?;
    // Trusting, the PATH, the sign-in entry or a version change files, not events.
    publish(&app);
    Ok(done)
}

/// What is left to set up, and whether the screen opens by itself now. It opens by itself once;
/// after that the page only shows a banner (F13.8). Asking marks it as offered.
#[derive(Debug, Serialize)]
struct SetupScreen {
    view: SetupView,
    open: bool,
}

#[tauri::command]
async fn setup_plan(first: bool) -> Result<SetupScreen, Failure> {
    blocking(move |context| {
        let view = fetch_setup(context);
        let mut settings = GuiSettings::load(&context.paths);
        let open = first && settings.offers_setup(&view);
        if open {
            settings.setup_offered = true;
            // A failure to remember only means it opens once more.
            let _ = settings.save(&context.paths, context.dirs.as_ref());
        }
        Ok(SetupScreen { view, open })
    })
    .await
}

/// The button of the setup screen. `accepted` are the optional steps whose switch was on; the
/// core plans again and does only what its own plan lists. The dialogs of Windows and the
/// permission prompt come up from this process.
#[tauri::command]
async fn setup_run(app: AppHandle, accepted: Vec<String>) -> Result<SetupDone, Failure> {
    let done =
        blocking(move |context| run_setup(context, &accepted).map_err(Failure::from)).await?;
    // The PATH, the sign-in entry and the shims are files and keys, not events.
    publish(&app);
    Ok(done)
}

/// What `uninstall --all` would do, with or without `--purge`.
#[tauri::command]
async fn uninstall_plan(purge: bool) -> Result<UninstallView, Failure> {
    blocking(move |context| Ok(fetch_uninstall(context, purge))).await
}

/// How long the app stays open after an uninstall that went through: its programs are deleted
/// once it ends, and the deletion tries only for a few seconds.
const CLOSE_AFTER_UNINSTALL: Duration = Duration::from_secs(5);

/// The button of the uninstall screen. `backups` is the person's answer to whether they have a
/// backup; the core refuses a purge without it. When Crisol is out of Windows the app closes by
/// itself after a moment, so that its own files can go.
#[tauri::command]
async fn uninstall_run(
    app: AppHandle,
    purge: bool,
    backups: bool,
) -> Result<UninstallDone, Failure> {
    let done = blocking(move |context| {
        run_uninstall(context, purge, backups, &mut |_| {}).map_err(Failure::from)
    })
    .await?;
    if done.close_app {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(CLOSE_AFTER_UNINSTALL);
            app.exit(0);
        });
    }
    Ok(done)
}

/// The logs the viewer offers: the services the daemon knows, and the daemon's own.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn log_choices(app: AppHandle) -> Vec<String> {
    log_services(&live_state(&app))
}

/// One read of a log: its end when `from` is `None`, what came after `from` otherwise. The
/// daemon reads a bounded window of the file, never all of it.
#[tauri::command]
async fn log_read(service: String, from: Option<u64>) -> Result<LogUpdate, Failure> {
    blocking(move |context| fetch_log(context, &service, from).map_err(Failure::from)).await
}

/// The services that have credentials.
#[tauri::command]
fn credential_choices() -> Vec<&'static str> {
    credential_services()
}

/// What a project needs to connect to `service`, and whether its password can be reset now.
#[tauri::command]
async fn credentials(app: AppHandle, service: String) -> Result<CredentialsView, Failure> {
    let live = live_state(&app);
    blocking(move |context| fetch_credentials(context, &service, &live).map_err(Failure::from))
        .await
}

#[tauri::command]
async fn open_site(name: String) -> Result<(), Failure> {
    blocking(move |context| open_in_browser(context, &name).map_err(Failure::from)).await
}

/// The dialog of the system to choose a folder. `None` when it is cancelled.
#[tauri::command]
async fn pick_folder(app: AppHandle) -> Result<Option<Picked>, Failure> {
    let owner = app
        .get_webview_window("main")
        .and_then(|window| window.hwnd().ok())
        .map(|handle| handle.0 as isize);
    blocking(move |context| {
        let picked = context
            .desktop
            .pick_folder(owner)
            .map_err(|e| failure(format!("cannot show the folder dialog: {e}")))?;
        Ok(picked.map(|path| Picked {
            name: suggested_site_name(&path),
            path: path.display().to_string(),
        }))
    })
    .await
}

/// A terminal of Crisol in a window of its own, in the folder of a project or where the
/// projects are.
#[tauri::command]
async fn open_terminal(place: Place) -> Result<Opened, Failure> {
    blocking(move |context| {
        let env = clean_env(std::env::vars_os());
        crisol_client::open_terminal(context, &env, &place).map_err(Failure::from)
    })
    .await
}

/// The terminals this computer has and the one chosen.
#[tauri::command]
async fn terminal_options() -> Result<TerminalOptions, Failure> {
    blocking(|context| current_terminal_options(context).map_err(Failure::from)).await
}

/// The person's choice of terminal (`None`: automatic), written to `crisol.toml`.
#[tauri::command]
async fn choose_terminal(name: Option<String>) -> Result<(), Failure> {
    blocking(move |context| {
        crisol_client::choose_terminal(&context.paths, context.dirs.as_ref(), name.as_deref())
            .map_err(Failure::from)
    })
    .await
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
    if let Ok(context) = Context::windows(CLIENT, VERSION) {
        let mut settings = GuiSettings::load(&context.paths);
        settings.exit_notice_shown = true;
        let _ = settings.save(&context.paths, context.dirs.as_ref());
    }
    app.exit(0);
}

/// One instance per user. A second run shows the window of the first, or, with `--quit`, ends
/// it: the first plugin, so that a second run is answered before anything else starts.
fn single_instance() -> TauriPlugin<Wry> {
    tauri_plugin_single_instance::init(|app, args, _cwd| {
        if args.iter().any(|arg| arg == QUIT_ARG) {
            app.exit(0);
        } else {
            show_main(app);
        }
    })
}

/// `crisol-gui.exe --quit`: the plugin hands it to the window that runs, and this process ends
/// there. With none running it ends too, without a window or a tray.
fn quit_running(mut tauri_context: tauri::Context) {
    tauri_context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .plugin(single_instance())
        .setup(|_app| std::process::exit(0))
        .run(tauri_context)
        .expect("the application runs");
}

fn main() {
    let tauri_context = tauri::generate_context!();
    if std::env::args().skip(1).any(|arg| arg == QUIT_ARG) {
        quit_running(tauri_context);
        return;
    }

    let words = Dictionary::of(Language::from_preferences(crisol_platform::ui_languages()));

    // Without WebView2 there is no window to say anything in. Asked the way Tauri asks
    // (the WebView2 loader), not through the registry: in Windows Sandbox the runtime can be
    // registered and still not be available, and then Tauri would show its own English box.
    if tauri::webview_version().is_err() {
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
        .plugin(single_instance())
        .manage(words.clone())
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            quit_confirmed,
            act,
            sites,
            versions,
            version_act,
            doctor,
            doctor_trust_preview,
            doctor_act,
            setup_plan,
            setup_run,
            uninstall_plan,
            uninstall_run,
            log_choices,
            log_read,
            credential_choices,
            credentials,
            open_site,
            pick_folder,
            open_terminal,
            terminal_options,
            choose_terminal
        ])
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
        .run(tauri_context)
        .expect("the application runs");
}
