//! What the command line and the GUI share to talk to Crisol (ADR-0026 §5).
//!
//! Neither of them decides anything about Crisol; this crate holds the parts that are not the
//! terminal and not a window:
//!
//! - [`Context`]: the wiring of Windows (home, pipe, registry, certificate store, helper),
//!   built once by [`Context::windows`] and replaced by fakes in the tests;
//! - [`Link`], [`Remote`] and [`Local`]: a way to make calls, to the daemon or to this process
//!   when there is none, and [`connect`], which opens one (starting the daemon when the call
//!   should) and says `hello`;
//! - [`LiveView`]: the state of the daemon kept up to date from its events, over a connection
//!   of its own, which reconnects when the daemon goes away and comes back.
//!
//! Everything here is tested without an interface.

pub mod connect;
mod context;
mod credentials;
mod doctor;
mod firstrun;
mod link;
mod live;
mod logs;
mod settings;
mod sites;
mod summary;
mod terminal;
mod text;
mod versions;
#[cfg(windows)]
mod windows;

pub use context::{Context, RemovePrograms};
pub use credentials::{
    credential_services, credentials_view, fetch_credentials, is_secret, reset_offer,
    CredentialLine, CredentialsView, ResetOffer,
};
/// The argument that closes the window that is running (`crisol-gui.exe --quit`).
pub use crisol_core::appwindow::QUIT_ARG;
pub use crisol_core::i18n::Language;
/// The `id` of every step the setup and uninstall screens can name (the page translates each).
pub use crisol_core::onboarding::STEP_IDS as SETUP_STEP_IDS;
pub use crisol_core::teardown::STEP_IDS as UNINSTALL_STEP_IDS;
pub use doctor::{
    doctor_button, doctor_view, fetch_doctor, inside_home, run_doctor_action, trust_preview,
    ActionDone, ButtonKind, DoctorButton, DoctorRow, DoctorView, TrustPreview, Words,
};
pub use firstrun::{
    fetch_setup, fetch_uninstall, run_setup, run_setup_on, run_uninstall, run_uninstall_on,
    setup_view, uninstall_view, with_setup_system, with_teardown_system, SetupDone, SetupProblem,
    SetupStep, SetupView, UninstallDone, UninstallView,
};
pub use link::{Connector, Link, Local, Remote};
pub use live::{Connection, LiveConfig, LiveState, LiveView};
pub use logs::{
    fetch_log, log_call, log_services, log_update, LogUpdate, DAEMON_LOG, FIRST_LINES, KEEP_LINES,
};
pub use settings::GuiSettings;
pub use sites::{
    fetch_sites, open_site, parked_rows, site_actions, site_url, sites_view, suggested_site_name,
    Act, ParkedRow, SiteAction, SiteRow, SitesView,
};
pub use summary::{
    enabled_in, is_data_service, metal_of, row_actions, LeaveAction, Overall, RowAction,
    ServiceRow, Summary, Toggle,
};
pub use terminal::{
    choose_terminal, clean_env, current_terminal_options, launch_terminal, open_terminal,
    projects_folder, start_folder, terminal_options, Opened, Place, TerminalOption,
    TerminalOptions,
};
pub use text::{localize_error, localize_reply, say};
pub use versions::{
    fetch_versions, run_version_act, tool_of, version_actions, versions_view, InUse, Progress,
    ProgressGate, ServiceVersions, VersionAct, VersionAction, VersionRow, VersionsView,
};
#[cfg(windows)]
pub use windows::WindowsError;
