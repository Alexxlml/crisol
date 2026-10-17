//! crisold: the resident process that owns the services (F8.2).
//!
//! The logic is in `crisol-core` ([`crisol_core::daemon::Daemon`]); this crate gives it a pipe to
//! be reached by, a thread to live in, and the DNS responder of the local sites.
//!
//! ```text
//!   clients ──pipe──▶ connection tasks ──Msg──▶ actor thread (owns the Daemon, ticks it)
//!                              ▲                        │
//!                              └────── events ──────────┘
//! ```
//!
//! Async is `tokio`, as everywhere else in the project, but only here: the core is plain
//! synchronous code, and the actor is an ordinary thread.

mod actor;
mod connection;
mod logging;
mod server;

pub use actor::Timing;
pub use connection::{serve as serve_connection, Shared};
pub use server::{run, DnsOptions, Options};
