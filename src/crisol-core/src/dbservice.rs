//! MySQL, MariaDB and PostgreSQL as services (F9.1, F9.2, F9.5; ADR-0006, ADR-0007, ADR-0016),
//! and the simpler data services that follow the same pattern (F9.4): MongoDB, Mailpit and
//! Meilisearch. They have no initializer (their data folder is made empty), no configuration file
//! of ours (everything goes in the command line) and no graceful-stop command; they listen on
//! `127.0.0.1` only and send nothing to anybody (ADR-0004: Meilisearch's analytics and Mailpit's
//! update check are switched off).
//!
//! What a database service needs besides a [`Service`]:
//!
//! - a **configuration** Crisol writes (`etc/<service>/<series>/my.ini`): it listens on
//!   `127.0.0.1` only, on the port of `crisol.toml`, with the data and the log inside the home;
//!   the user's own settings go in `etc/<service>/overrides/*.ini` and are never touched;
//! - its **data**, apart for each series (`data/mysql/8.4/`, `data/mariadb/11.8/`), so two
//!   versions never share (and never corrupt) a data directory (F9.5);
//! - **one-time initialization** of that directory. It is done into `<series>.init` and moved to
//!   its place only when the initializer succeeded, so a failed or interrupted initialization
//!   never leaves a half-made data directory that would pass for a good one;
//! - a **graceful stop** (`mysqladmin shutdown`), because killing a database is a recovery at
//!   the next start.
//!
//! The root account has **no password and can only be reached from this computer**
//! (`bind-address = 127.0.0.1`), as ADR-0006 says: nothing here puts a secret in a file.
//! [`DbService`] is the state machine that goes from "stopped" through initialization (when the
//! data directory is new) to a running [`Service`]; like `Service`, it never blocks and is
//! advanced with [`tick`](DbService::tick).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::manage::{list_installed, InstalledVersion};
use crate::paths::{CrisolPaths, PathsError};
use crate::ports::PortProbe;
use crate::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessSpec, ProcessStatus, ProcessSupervisor,
};
use crate::service::{
    FailureReason, HealthCheck, HealthCommand, LifecyclePolicy, Service, ServiceEvent, ServiceSpec,
    ServiceState,
};

/// The database servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DbKind {
    /// Oracle's MySQL.
    Mysql,
    /// MariaDB.
    Mariadb,
    /// PostgreSQL.
    Postgres,
    /// MongoDB.
    Mongodb,
    /// Mailpit: catches the mail the sites send, with a web page to read it.
    Mailpit,
    /// Meilisearch.
    Meilisearch,
    /// Redis (the port for Windows of tporadowski, 5.0.x).
    Redis,
    /// Garnet: Microsoft's server that speaks Redis's protocol. Not Redis (ADR-0017).
    Garnet,
}

impl DbKind {
    /// All of them, MySQL first.
    pub const ALL: [Self; 8] = [
        Self::Mysql,
        Self::Mariadb,
        Self::Postgres,
        Self::Mongodb,
        Self::Mailpit,
        Self::Meilisearch,
        Self::Redis,
        Self::Garnet,
    ];

    /// The name of the service (`mysql`, `mariadb`): in the catalog, in `crisol.toml` and in
    /// every folder.
    #[must_use]
    pub fn service(self) -> &'static str {
        match self {
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Postgres => "postgresql",
            Self::Mongodb => "mongodb",
            Self::Mailpit => "mailpit",
            Self::Meilisearch => "meilisearch",
            Self::Redis => "redis",
            Self::Garnet => "garnet",
        }
    }

    /// The kind a service name means.
    #[must_use]
    pub fn from_service(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.service() == name)
    }

    /// The server, relative to the install folder.
    fn server(self) -> &'static str {
        match self {
            Self::Mysql => "bin/mysqld.exe",
            Self::Mariadb => "bin/mariadbd.exe",
            Self::Postgres => "bin/postgres.exe",
            Self::Mongodb => "bin/mongod.exe",
            Self::Mailpit => "mailpit.exe",
            Self::Meilisearch => "meilisearch-windows-amd64.exe",
            Self::Redis => "redis-server.exe",
            Self::Garnet => "net10.0/GarnetServer.exe",
        }
    }

    /// The configuration file Crisol generates, if the service reads one.
    fn config_file(self) -> Option<&'static str> {
        match self {
            Self::Mysql | Self::Mariadb => Some("my.ini"),
            Self::Postgres => Some("postgresql.conf"),
            Self::Mongodb | Self::Mailpit | Self::Meilisearch | Self::Redis | Self::Garnet => None,
        }
    }

    /// Whether the data folder is made by a program (and so atomically, in a staging folder);
    /// the others start from an empty folder.
    fn has_initializer(self) -> bool {
        matches!(self, Self::Mysql | Self::Mariadb | Self::Postgres)
    }

    /// The program that creates the data directory of MariaDB (MySQL initializes itself).
    fn installer(self) -> Option<&'static str> {
        match self {
            Self::Mariadb => Some("bin/mariadb-install-db.exe"),
            _ => None,
        }
    }

    /// Whether `crisol.toml` enables it.
    #[must_use]
    pub fn enabled(self, config: &Config) -> bool {
        match self {
            Self::Mysql => config.services.mysql.enabled,
            Self::Mariadb => config.services.mariadb.enabled,
            Self::Postgres => config.services.postgresql.enabled,
            Self::Mongodb => config.services.mongodb.enabled,
            Self::Mailpit => config.services.mailpit.enabled,
            Self::Meilisearch => config.services.meilisearch.enabled,
            Self::Redis => config.services.redis.enabled,
            Self::Garnet => config.services.garnet.enabled,
        }
    }

    /// The port it listens on.
    #[must_use]
    pub fn port(self, config: &Config) -> u16 {
        match self {
            Self::Mysql => config.services.mysql.port,
            Self::Mariadb => config.services.mariadb.port,
            Self::Postgres => config.services.postgresql.port,
            Self::Mongodb => config.services.mongodb.port,
            Self::Mailpit => config.services.mailpit.port,
            Self::Meilisearch => config.services.meilisearch.port,
            Self::Redis => config.services.redis.port,
            Self::Garnet => config.services.garnet.port,
        }
    }

    /// A second port the service listens on (Mailpit's SMTP).
    fn extra_port(self, config: &Config) -> Option<u16> {
        (self == Self::Mailpit).then_some(config.services.mailpit.smtp_port)
    }

    fn pinned(self, config: &Config) -> Option<&str> {
        match self {
            Self::Mysql => config.services.mysql.version.as_deref(),
            Self::Mariadb => config.services.mariadb.version.as_deref(),
            Self::Postgres => config.services.postgresql.version.as_deref(),
            Self::Mongodb => config.services.mongodb.version.as_deref(),
            Self::Mailpit => config.services.mailpit.version.as_deref(),
            Self::Meilisearch => config.services.meilisearch.version.as_deref(),
            Self::Redis => config.services.redis.version.as_deref(),
            Self::Garnet => config.services.garnet.version.as_deref(),
        }
    }

    /// The part of a version that decides whether two data directories are compatible: the first
    /// two numbers for MySQL and MariaDB (`8.4`, `11.8`, `10.11`), the major for PostgreSQL
    /// (`17`, `18`). A new series is a new folder, never an upgrade in place.
    #[must_use]
    pub fn series(self, version: &str) -> String {
        let parts = if matches!(self, Self::Postgres | Self::Mailpit) {
            1
        } else {
            2
        };
        version.split('.').take(parts).collect::<Vec<_>>().join(".")
    }
}

/// Why a database service cannot be planned.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// No version of it is installed (or the pinned one is not).
    #[error("{0} is not installed; run `crisol install {0}`")]
    NotInstalled(String),
    /// The installed versions could not be listed.
    #[error("{0}")]
    List(String),
    /// A folder name is not allowed.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// A path holds a character the configuration cannot carry.
    #[error("{0}")]
    Unusable(String),
}

/// Everything decided about one database service before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPlan {
    /// MySQL or MariaDB.
    pub kind: DbKind,
    /// The installed version that will run.
    pub version: InstalledVersion,
    /// `8.4`, `11.8`…
    pub series: String,
    /// Where it listens.
    pub port: u16,
    /// A second port (Mailpit's SMTP).
    pub extra_port: Option<u16>,
    /// `data/<service>/<series>/`.
    pub data: PathBuf,
    /// `data/<service>/<series>.init/`: where it is made first.
    pub staging: PathBuf,
    /// `etc/<service>/<series>/my.ini` (`postgresql.conf` for PostgreSQL).
    pub ini: PathBuf,
    /// `etc/<service>/overrides/`: the user's own `*.ini` (`*.conf` for PostgreSQL).
    pub overrides: PathBuf,
    /// `logs/<service>/error.log`.
    pub error_log: PathBuf,
    /// `logs/<service>/runtime.log`: what the process prints.
    pub runtime_log: PathBuf,
}

/// Chooses the version (the pinned one, or the highest installed) and lays out its folders.
pub fn plan(paths: &CrisolPaths, config: &Config, kind: DbKind) -> Result<DbPlan, DbError> {
    let installed: Vec<InstalledVersion> = list_installed(paths)
        .map_err(|e| DbError::List(e.to_string()))?
        .into_iter()
        .filter(|i| i.service == kind.service())
        .collect();
    let version = match kind.pinned(config) {
        Some(pin) => {
            let wanted = crate::version::VersionReq::parse(pin).map_err(|e| {
                DbError::Unusable(format!("services.{}.version: {e}", kind.service()))
            })?;
            installed.into_iter().rfind(|i| wanted.matches(&i.version))
        }
        None => installed.into_iter().next_back(),
    }
    .ok_or_else(|| DbError::NotInstalled(kind.service().to_owned()))?;
    let series = kind.series(&version.version.to_string());
    let service = kind.service();
    let etc = paths.etc_for(service)?;
    let plan = DbPlan {
        kind,
        series: series.clone(),
        port: kind.port(config),
        data: paths.data_for(service, &series)?,
        staging: paths.data_for(service, &format!("{series}.init"))?,
        ini: etc.join(&series).join(kind.config_file().unwrap_or("none")),
        extra_port: kind.extra_port(config),
        overrides: paths.overrides_for(service)?,
        error_log: paths.logs_for(service)?.join("error.log"),
        runtime_log: paths.runtime_log(service)?,
        version,
    };
    Ok(plan)
}

impl DbPlan {
    /// The name the service has in `status` (`mysql`, `mariadb`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.kind.service()
    }

    fn program(&self, exe: &str) -> PathBuf {
        self.version.path.join(exe)
    }
}

/// A path as it goes between the quotes of a `my.ini` value (forward slashes).
fn quote(path: &Path) -> Result<String, DbError> {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(bad) = text
        .chars()
        .find(|c| matches!(c, '"' | '#' | ';') || c.is_control())
    {
        return Err(DbError::Unusable(format!(
            "`{text}` has a character the database configuration would read differently ({bad:?}); \
             use a Crisol home without it"
        )));
    }
    Ok(format!("\"{text}\""))
}

/// A path as it goes between the single quotes of a `postgresql.conf` value.
fn quote_pg(path: &Path) -> Result<String, DbError> {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(bad) = text.chars().find(|c| *c == '\'' || c.is_control()) {
        return Err(DbError::Unusable(format!(
            "`{text}` has a character the database configuration would read differently ({bad:?}); \
             use a Crisol home without it"
        )));
    }
    Ok(format!("'{text}'"))
}

/// The text of `postgresql.conf`: this computer only, the port of `crisol.toml`, the log to
/// standard error (Crisol keeps it in `runtime.log`) and the user's own `*.conf`.
fn render_postgres(plan: &DbPlan) -> Result<String, DbError> {
    let mut text = String::from(
        "# Generated by Crisol; it is rewritten whenever the plan changes. Your own settings go in\n\
         # the `overrides` folder next to it, which is never touched.\n\n",
    );
    let mut line = |key: &str, value: &str| {
        text.push_str(key);
        text.push_str(" = ");
        text.push_str(value);
        text.push('\n');
    };
    // Only this computer: the superuser has no password (ADR-0006).
    line("listen_addresses", "'127.0.0.1'");
    line("port", &plan.port.to_string());
    line("log_destination", "'stderr'");
    line("logging_collector", "off");
    line("include_dir", &quote_pg(&plan.overrides)?);
    Ok(text)
}

/// The text of `my.ini` (`postgresql.conf` for PostgreSQL).
pub fn render_ini(plan: &DbPlan) -> Result<String, DbError> {
    if plan.kind.config_file().is_none() {
        return Ok(String::new());
    }
    if plan.kind == DbKind::Postgres {
        return render_postgres(plan);
    }
    let mut text = String::from(
        "# Generated by Crisol; it is rewritten whenever the plan changes. Your own settings go in\n\
         # the `overrides` folder next to it, which is never touched.\n\n[mysqld]\n",
    );
    let mut line = |key: &str, value: &str| {
        text.push_str(key);
        text.push_str(" = ");
        text.push_str(value);
        text.push('\n');
    };
    line("basedir", &quote(&plan.version.path)?);
    line("datadir", &quote(&plan.data)?);
    line("port", &plan.port.to_string());
    // Only this computer: root has no password (ADR-0006).
    line("bind-address", "127.0.0.1");
    line("log-error", &quote(&plan.error_log)?);
    line("character-set-server", "utf8mb4");
    if plan.kind == DbKind::Mysql {
        // The X protocol would open 33060 for nobody. (No `skip-name-resolve`: the only account
        // `--initialize-insecure` makes is `root@localhost`, and a connection from 127.0.0.1 is
        // that account only while the server maps the loopback address to `localhost`. Found
        // with the real MySQL 8.4.)
        line("mysqlx", "OFF");
    } else {
        // MariaDB makes `root@127.0.0.1` and `root@::1` as well.
        line("skip-name-resolve", "ON");
    }
    text.push_str("\n!includedir ");
    text.push_str(&quote(&plan.overrides)?);
    text.push('\n');
    Ok(text)
}

/// Writes `my.ini` (and the empty `overrides` folder it includes) if it changed.
pub fn write_ini(plan: &DbPlan) -> Result<(), DbError> {
    if plan.kind.config_file().is_none() {
        return Ok(());
    }
    let text = render_ini(plan)?;
    let io = |e: std::io::Error| DbError::Unusable(e.to_string());
    fs::create_dir_all(&plan.overrides).map_err(io)?;
    if let Some(dir) = plan.ini.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    if fs::read_to_string(&plan.ini).is_ok_and(|current| current == text) {
        return Ok(());
    }
    let tmp = plan.ini.with_extension("ini.new");
    fs::write(&tmp, text).map_err(io)?;
    fs::rename(&tmp, &plan.ini).map_err(io)
}

/// The one-time initializer, writing into the staging folder.
#[must_use]
pub fn init_spec(plan: &DbPlan) -> ProcessSpec {
    if plan.kind == DbKind::Postgres {
        // The superuser is `postgres`, with no password, reachable from this computer only
        // (`trust` in a `pg_hba.conf` that listens nowhere else).
        return ProcessSpec::new(plan.program("bin/initdb.exe"))
            .arg("-D")
            .arg(&plan.staging)
            .args(["-U", "postgres", "-A", "trust", "-E", "UTF8"])
            .cwd(&plan.version.path)
            .log_file(&plan.runtime_log);
    }
    let spec = match plan.kind.installer() {
        // MariaDB makes a root without a password.
        Some(installer) => ProcessSpec::new(plan.program(installer))
            .arg(format!("--datadir={}", plan.staging.display())),
        None => ProcessSpec::new(plan.program(plan.kind.server()))
            .arg(format!("--defaults-file={}", plan.ini.display()))
            .arg("--initialize-insecure")
            .arg(format!("--datadir={}", plan.staging.display()))
            .arg("--console"),
    };
    spec.cwd(&plan.version.path).log_file(&plan.runtime_log)
}

/// The long-running service.
#[must_use]
pub fn service_spec(plan: &DbPlan) -> ServiceSpec {
    match plan.kind {
        DbKind::Postgres => return postgres_spec(plan),
        DbKind::Mongodb
        | DbKind::Mailpit
        | DbKind::Meilisearch
        | DbKind::Redis
        | DbKind::Garnet => return simple_spec(plan),
        DbKind::Mysql | DbKind::Mariadb => {}
    }
    let admin = ProcessSpec::new(plan.program("bin/mysqladmin.exe"))
        .arg("--protocol=tcp")
        .arg("--host=127.0.0.1")
        .arg(format!("--port={}", plan.port))
        .arg("--user=root")
        .arg("shutdown");
    ServiceSpec::new(plan.name())
        .process(
            ProcessSpec::new(plan.program(plan.kind.server()))
                .arg(format!("--defaults-file={}", plan.ini.display()))
                .arg("--console")
                .cwd(&plan.version.path)
                .log_file(&plan.runtime_log),
        )
        .port(plan.port)
        .health(HealthCheck::Ports(vec![plan.port]))
        .graceful_stop(admin)
        .policy(LifecyclePolicy {
            // A first start, or one after a crash, replays the redo log.
            startup_timeout: Duration::from_secs(90),
            // A clean shutdown flushes the buffer pool.
            stop_grace: Duration::from_secs(30),
            ..LifecyclePolicy::default()
        })
}

/// MongoDB, Mailpit and Meilisearch: one process, everything in its command line, ready when
/// its ports listen. They have no shutdown command Crisol can use (`mongod` has none on
/// Windows), so a stop ends the group: all three recover their data from their own log or
/// transaction file at the next start.
fn simple_spec(plan: &DbPlan) -> ServiceSpec {
    let data = &plan.data;
    let mut process = ProcessSpec::new(plan.program(plan.kind.server()))
        .cwd(data)
        .log_file(&plan.runtime_log);
    match plan.kind {
        DbKind::Mongodb => {
            process = process.arg("--dbpath").arg(data).args([
                "--bind_ip",
                "127.0.0.1",
                "--port",
                &plan.port.to_string(),
            ]);
        }
        DbKind::Mailpit => {
            process = process
                .args(["--listen", &format!("127.0.0.1:{}", plan.port)])
                .args([
                    "--smtp",
                    &format!("127.0.0.1:{}", plan.extra_port.unwrap_or(1025)),
                ])
                .arg("--database")
                .arg(data.join("mailpit.db"))
                // Nothing leaves this computer, not even a look for a newer version.
                .arg("--disable-version-check");
        }
        DbKind::Meilisearch => {
            process = process
                .args(["--http-addr", &format!("127.0.0.1:{}", plan.port)])
                .arg("--db-path")
                .arg(data.join("db"))
                .arg("--dump-dir")
                .arg(data.join("dumps"))
                .arg("--snapshot-dir")
                .arg(data.join("snapshots"))
                // ADR-0004: no telemetry.
                .arg("--no-analytics");
        }
        DbKind::Redis => {
            // Loopback only, with the protection Redis has for that; the data (the RDB file) in
            // its own folder. `--maxmemory` is what the port for Windows needs to size the heap
            // it uses to save in the background.
            process = process
                .args(["--bind", "127.0.0.1", "--port", &plan.port.to_string()])
                .args(["--protected-mode", "yes", "--maxmemory", "512mb"])
                // Started without a configuration file the Windows port saves nothing, not even
                // on `SHUTDOWN`: the usual save points are asked for (found with the real one).
                .args([
                    "--save", "900", "1", "--save", "300", "100", "--save", "60", "10000",
                ])
                .arg("--dir")
                .arg(data);
        }
        DbKind::Garnet => {
            // Garnet keeps its keys in memory unless it is told to persist (checkpoints/AOF):
            // a cache is what it is used for here. It sends no telemetry.
            process = process
                .args(["--bind", "127.0.0.1", "--port", &plan.port.to_string()])
                .arg("--checkpointdir")
                .arg(data.join("checkpoints"))
                // Lua is off by default, and Laravel's queues use it.
                .arg("--lua");
        }
        DbKind::Mysql | DbKind::Mariadb | DbKind::Postgres => {}
    }
    let ports: Vec<u16> = std::iter::once(plan.port).chain(plan.extra_port).collect();
    let mut spec = ServiceSpec::new(plan.name())
        .process(process)
        .health(HealthCheck::Ports(ports.clone()))
        .policy(LifecyclePolicy {
            startup_timeout: Duration::from_secs(60),
            ..LifecyclePolicy::default()
        });
    for port in ports {
        spec = spec.port(port);
    }
    if plan.kind == DbKind::Redis {
        // `SHUTDOWN` saves the data if there are save points, then leaves.
        spec = spec.graceful_stop(ProcessSpec::new(plan.program("redis-cli.exe")).args([
            "-h",
            "127.0.0.1",
            "-p",
            &plan.port.to_string(),
            "shutdown",
        ]));
    }
    spec
}

fn postgres_spec(plan: &DbPlan) -> ServiceSpec {
    let stop = ProcessSpec::new(plan.program("bin/pg_ctl.exe"))
        .arg("stop")
        .arg("-D")
        .arg(&plan.data)
        .args(["-m", "fast"]);
    let ready = ProcessSpec::new(plan.program("bin/pg_isready.exe")).args([
        "-h",
        "127.0.0.1",
        "-p",
        &plan.port.to_string(),
        "-q",
    ]);
    ServiceSpec::new(plan.name())
        .process(
            ProcessSpec::new(plan.program(plan.kind.server()))
                .arg("-D")
                .arg(&plan.data)
                .arg("-c")
                .arg(format!("config_file={}", plan.ini.display()))
                .cwd(&plan.version.path)
                .log_file(&plan.runtime_log),
        )
        .port(plan.port)
        // The port opens before the server accepts connections; `pg_isready` says when it does.
        .health(HealthCheck::Command(HealthCommand {
            spec: ready,
            timeout: Duration::from_secs(5),
        }))
        .graceful_stop(stop)
        .policy(LifecyclePolicy {
            startup_timeout: Duration::from_secs(90),
            stop_grace: Duration::from_secs(30),
            ..LifecyclePolicy::default()
        })
}

/// How long the initializer may take.
const INIT_TIMEOUT: Duration = Duration::from_secs(180);

enum Phase {
    Idle,
    Initializing {
        group: GroupId,
        process: ProcessId,
        since: Instant,
    },
    Running(Box<Service>),
}

/// One database: initializes its data the first time, then runs it.
pub struct DbService {
    plan: DbPlan,
    supervisor: Arc<dyn ProcessSupervisor>,
    ports: Arc<dyn PortProbe>,
    phase: Phase,
    state: ServiceState,
}

impl std::fmt::Debug for DbService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbService")
            .field("name", &self.plan.name())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl DbService {
    /// A stopped database.
    #[must_use]
    pub fn new(
        plan: DbPlan,
        supervisor: Arc<dyn ProcessSupervisor>,
        ports: Arc<dyn PortProbe>,
    ) -> Self {
        Self {
            plan,
            supervisor,
            ports,
            phase: Phase::Idle,
            state: ServiceState::Stopped,
        }
    }

    /// The plan it runs.
    #[must_use]
    pub fn plan(&self) -> &DbPlan {
        &self.plan
    }

    /// Its state; initializing counts as starting.
    #[must_use]
    pub fn state(&self) -> &ServiceState {
        &self.state
    }

    /// Whether it is neither starting nor stopping.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        matches!(
            self.state,
            ServiceState::Running | ServiceState::Stopped | ServiceState::Failed(_)
        )
    }

    fn change(&mut self, to: ServiceState, events: &mut Vec<ServiceEvent>) {
        if self.state != to {
            let from = std::mem::replace(&mut self.state, to.clone());
            events.push(ServiceEvent::StateChanged { from, to });
        }
    }

    fn fail(&mut self, reason: String, events: &mut Vec<ServiceEvent>) {
        self.close_phase();
        self.change(
            ServiceState::Failed(FailureReason::Internal(reason)),
            events,
        );
    }

    /// Starts it. From `Stopped` or `Failed`; anything else changes nothing.
    pub fn start(&mut self, now: Instant) -> Vec<ServiceEvent> {
        let mut events = Vec::new();
        if !matches!(self.state, ServiceState::Stopped | ServiceState::Failed(_)) {
            return events;
        }
        if let Err(e) = write_ini(&self.plan) {
            self.fail(e.to_string(), &mut events);
            return events;
        }
        if self.plan.data.is_dir() {
            self.launch(now, &mut events);
        } else if self.plan.kind.has_initializer() {
            self.begin_init(now, &mut events);
        } else if let Err(e) = fs::create_dir_all(&self.plan.data) {
            self.fail(
                format!("cannot make {}: {e}", self.plan.data.display()),
                &mut events,
            );
        } else {
            self.launch(now, &mut events);
        }
        events
    }

    fn launch(&mut self, now: Instant, events: &mut Vec<ServiceEvent>) {
        let mut service = Service::new(
            service_spec(&self.plan),
            self.supervisor.clone(),
            self.ports.clone(),
        );
        if let Ok(started) = service.start(now) {
            events.extend(started);
        }
        self.state = ServiceState::Starting;
        self.mirror(&service, events);
        self.phase = Phase::Running(Box::new(service));
    }

    /// Takes over the state of the inner service, if it changed.
    fn mirror(&mut self, service: &Service, events: &mut Vec<ServiceEvent>) {
        let inner = service.state().clone();
        if inner != self.state {
            let from = std::mem::replace(&mut self.state, inner.clone());
            events.push(ServiceEvent::StateChanged { from, to: inner });
        }
    }

    fn begin_init(&mut self, now: Instant, events: &mut Vec<ServiceEvent>) {
        let go = || -> Result<(GroupId, ProcessId), String> {
            // A staging folder left by an interrupted initialization is ours and is not data.
            if self.plan.staging.exists() {
                fs::remove_dir_all(&self.plan.staging).map_err(|e| e.to_string())?;
            }
            if let Some(parent) = self.plan.staging.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // MySQL wants the folder to be empty or missing; MariaDB creates it.
            let group = self
                .supervisor
                .create_group("init", GroupOptions::default())
                .map_err(|e| e.to_string())?;
            match self.supervisor.spawn(group, &init_spec(&self.plan)) {
                Ok(process) => Ok((group, process)),
                Err(e) => {
                    let _ = self.supervisor.kill_group(group);
                    let _ = self.supervisor.close_group(group);
                    Err(e.to_string())
                }
            }
        };
        match go() {
            Ok((group, process)) => {
                self.phase = Phase::Initializing {
                    group,
                    process,
                    since: now,
                };
                self.change(ServiceState::Starting, events);
            }
            Err(reason) => self.fail(format!("cannot initialize the data: {reason}"), events),
        }
    }

    /// Stops it: an initialization is cancelled, a running server is asked to shut down.
    pub fn stop(&mut self, now: Instant) -> Vec<ServiceEvent> {
        let mut events = Vec::new();
        match &mut self.phase {
            Phase::Idle => {
                if matches!(self.state, ServiceState::Failed(_)) {
                    self.change(ServiceState::Stopped, &mut events);
                }
            }
            Phase::Initializing { .. } => {
                self.close_phase();
                self.change(ServiceState::Stopped, &mut events);
            }
            Phase::Running(service) => {
                events.extend(service.stop(now));
                let inner = service.state().clone();
                if inner != self.state {
                    let from = std::mem::replace(&mut self.state, inner.clone());
                    events.push(ServiceEvent::StateChanged { from, to: inner });
                }
            }
        }
        events
    }

    /// Kills everything now.
    pub fn abort(&mut self) {
        if let Phase::Running(service) = &mut self.phase {
            service.abort();
        }
        self.close_phase();
        self.state = ServiceState::Stopped;
    }

    /// Ends an initialization (killing it and deleting its staging folder) and forgets the
    /// phase.
    fn close_phase(&mut self) {
        if let Phase::Initializing { group, .. } = self.phase {
            let _ = self.supervisor.kill_group(group);
            let _ = self.supervisor.close_group(group);
            let _ = fs::remove_dir_all(&self.plan.staging);
        }
        self.phase = Phase::Idle;
    }

    /// Advances it. Call it every 150 to 500 ms.
    pub fn tick(&mut self, now: Instant) -> Vec<ServiceEvent> {
        let mut events = Vec::new();
        match &mut self.phase {
            Phase::Idle => {}
            Phase::Running(service) => {
                events.extend(service.tick(now));
                let inner = service.state().clone();
                if inner != self.state {
                    let from = std::mem::replace(&mut self.state, inner.clone());
                    events.push(ServiceEvent::StateChanged { from, to: inner });
                }
                if self.state == ServiceState::Stopped {
                    self.phase = Phase::Idle;
                }
            }
            Phase::Initializing {
                group,
                process,
                since,
            } => {
                let (group, process, since) = (*group, *process, *since);
                match self.supervisor.status(process) {
                    Ok(ProcessStatus::Running) => {
                        if now.saturating_duration_since(since) >= INIT_TIMEOUT {
                            self.fail(
                                format!(
                                    "the initialization of the data did not finish in {} seconds; see {}",
                                    INIT_TIMEOUT.as_secs(),
                                    self.plan.runtime_log.display()
                                ),
                                &mut events,
                            );
                        }
                    }
                    Ok(ProcessStatus::Exited(ExitReason::Code(0))) => {
                        let _ = self.supervisor.close_group(group);
                        self.phase = Phase::Idle;
                        match self.promote_staging() {
                            Ok(()) => self.launch(now, &mut events),
                            Err(reason) => self.fail(reason, &mut events),
                        }
                    }
                    Ok(ProcessStatus::Exited(other)) => {
                        let code = match other {
                            ExitReason::Code(code) => format!("code {code}"),
                            ExitReason::Killed => "being killed".to_owned(),
                        };
                        self.fail(
                            format!(
                                "the initialization of the data ended with {code}; see {}",
                                self.plan.runtime_log.display()
                            ),
                            &mut events,
                        );
                    }
                    Err(e) => self.fail(
                        format!("cannot tell how the initialization went: {e}"),
                        &mut events,
                    ),
                }
            }
        }
        events
    }

    /// The initializer succeeded: the staging folder becomes the data.
    fn promote_staging(&self) -> Result<(), String> {
        if !self.plan.staging.is_dir() {
            return Err(format!(
                "the initialization ended well but made no data in {}",
                self.plan.staging.display()
            ));
        }
        fs::rename(&self.plan.staging, &self.plan.data).map_err(|e| {
            let _ = fs::remove_dir_all(&self.plan.staging);
            format!("cannot put the new data in place: {e}")
        })
    }
}

#[cfg(test)]
mod tests;
