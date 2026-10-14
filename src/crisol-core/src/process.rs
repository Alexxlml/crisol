//! Process supervision: the platform-independent contract.
//!
//! The unit of supervision is a **group**: one per service (or per PHP pool). Every process
//! of a service is spawned into its group, and killing the group kills the whole process
//! tree, descendants included. On Windows a group is a Job Object (F2.2); on Unix it will be
//! a process group or cgroup.
//!
//! The trait is deliberately small, synchronous and non-blocking so it can be driven from
//! any runtime and faked easily:
//!
//! - Graceful stops (`nginx -s quit`, grace period) belong to the service lifecycle (F2.5),
//!   which calls [`ProcessSupervisor::kill_group`] as the last resort.
//! - The watchdog polls [`ProcessSupervisor::status`]; there is no blocking wait.
//! - A group is used for **one run**. After [`kill_group`](ProcessSupervisor::kill_group)
//!   it refuses new processes; the next start creates a fresh group.
//!
//! [`fake::FakeSupervisor`] is the test double every consumer of the trait uses.

pub mod fake;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use crate::log::LogRotation;

/// Opaque handle of a supervision group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupId(u64);

impl GroupId {
    /// Wraps a raw id. Implementations of [`ProcessSupervisor`] mint these.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque handle of a supervised process. Unlike an OS pid it is never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(u64);

impl ProcessId {
    /// Wraps a raw id. Implementations of [`ProcessSupervisor`] mint these.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Options for a new group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupOptions {
    /// When the group is closed, or the supervising process dies, the OS kills everything
    /// in it. This is what guarantees no orphans. Turn it off only for services the user
    /// wants to survive Crisol.
    pub kill_on_close: bool,
}

impl Default for GroupOptions {
    fn default() -> Self {
        Self {
            kill_on_close: true,
        }
    }
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Executable to launch.
    pub program: PathBuf,
    /// Arguments, one element each (never a joined command line).
    pub args: Vec<OsString>,
    /// Working directory; `None` inherits the supervisor's.
    pub cwd: Option<PathBuf>,
    /// Variables added to (or overriding) the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// File that receives stdout and stderr, appended. `None` discards the output.
    /// Processes of the same supervisor that name the same file share one writer, so their
    /// output interleaves instead of fighting over rotation.
    pub log_file: Option<PathBuf>,
    /// When to rotate `log_file`. Ignored when there is no log file.
    pub log_rotation: LogRotation,
}

impl ProcessSpec {
    /// A spec that runs `program` with no arguments.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            log_file: None,
            log_rotation: LogRotation::default(),
        }
    }

    /// Adds one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Adds several arguments.
    #[must_use]
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Adds an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Sends stdout and stderr to `file`.
    #[must_use]
    pub fn log_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.log_file = Some(file.into());
        self
    }

    /// Sets when the log file rotates.
    #[must_use]
    pub fn log_rotation(mut self, rotation: LogRotation) -> Self {
        self.log_rotation = rotation;
        self
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// It exited by itself with this code.
    Code(i32),
    /// It was terminated from outside: by [`ProcessSupervisor::kill_group`], by closing
    /// its group, or by a signal.
    Killed,
}

/// State of a supervised process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Still running.
    Running,
    /// Finished.
    Exited(ExitReason),
}

/// Why a supervisor call failed.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// The group does not exist or was already closed.
    #[error("unknown process group {}", .0.get())]
    UnknownGroup(GroupId),
    /// The process does not exist or its group was already closed.
    #[error("unknown process {}", .0.get())]
    UnknownProcess(ProcessId),
    /// The group was killed; start a new one.
    #[error("process group {} was killed and cannot take new processes", .0.get())]
    GroupKilled(GroupId),
    /// The OS refused to start the program.
    #[error("cannot start `{}`: {source}", program.display())]
    Spawn {
        /// Program that failed to start.
        program: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Any other operating-system failure.
    #[error("{0}")]
    Os(String),
}

/// Starts, observes and kills processes, grouped per service.
///
/// Implementations are `Send + Sync` and may be shared as `Arc<dyn ProcessSupervisor>`.
pub trait ProcessSupervisor: Send + Sync {
    /// Creates an empty group. `name` is for diagnostics only (e.g. `"nginx"`).
    fn create_group(&self, name: &str, options: GroupOptions) -> Result<GroupId, SupervisorError>;

    /// Starts a process inside `group`. Its children join the group automatically.
    fn spawn(&self, group: GroupId, spec: &ProcessSpec) -> Result<ProcessId, SupervisorError>;

    /// The operating-system pid of a process. Useful for logs and port reports only; use
    /// [`ProcessId`] to refer to the process, as pids are reused.
    fn pid(&self, process: ProcessId) -> Result<u32, SupervisorError>;

    /// Whether a process is still running. Never blocks.
    fn status(&self, process: ProcessId) -> Result<ProcessStatus, SupervisorError>;

    /// How many processes of the group are alive, **descendants included**. Zero means the
    /// whole tree is gone.
    fn active_processes(&self, group: GroupId) -> Result<usize, SupervisorError>;

    /// Kills every process of the group and its descendants. Idempotent: killing a group
    /// that is already dead succeeds. The group refuses new processes afterwards.
    fn kill_group(&self, group: GroupId) -> Result<(), SupervisorError>;

    /// Releases the group. If it was created with `kill_on_close`, whatever still runs in
    /// it is killed. All ids of the group are invalid afterwards.
    fn close_group(&self, group: GroupId) -> Result<(), SupervisorError>;
}
