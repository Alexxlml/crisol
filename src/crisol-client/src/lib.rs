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
mod language;
mod link;
mod live;
mod settings;
mod sites;
mod summary;
mod terminal;
#[cfg(windows)]
mod windows;

pub use context::{Context, RemovePrograms};
pub use language::Language;
pub use link::{Connector, Link, Local, Remote};
pub use live::{Connection, LiveConfig, LiveState, LiveView};
pub use settings::GuiSettings;
pub use sites::{
    fetch_sites, open_site, parked_rows, site_actions, site_url, sites_view, suggested_site_name,
    Act, ParkedRow, SiteAction, SiteRow, SitesView,
};
pub use summary::{
    enabled_in, metal_of, row_actions, LeaveAction, Overall, RowAction, ServiceRow, Summary, Toggle,
};
pub use terminal::{
    choose_terminal, clean_env, launch_terminal, open_terminal, projects_folder, start_folder,
    terminal_options, Opened, Place, TerminalOption, TerminalOptions,
};
#[cfg(windows)]
pub use windows::WindowsError;
