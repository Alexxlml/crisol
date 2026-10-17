//! The data services as services (F9; ADR-0006, ADR-0007, ADR-0016, ADR-0017): MySQL, MariaDB,
//! PostgreSQL, MongoDB, Mailpit, Meilisearch, Redis, Garnet and Memcached.
//!
//! What each needs besides a [`Service`]:
//!
//! - a **configuration** Crisol writes (`etc/<service>/<series>/my.ini`, `postgresql.conf`), or a
//!   command line, that makes it listen on `127.0.0.1` only, on the port of `crisol.toml`, with
//!   the data and the log inside the home; the user's own settings go in
//!   `etc/<service>/overrides/` and are never touched;
//! - its **data**, apart for each series (`data/mysql/8.4/`, `data/mariadb/11.8/`), so two
//!   versions never share (and never corrupt) a data directory (F9.5);
//! - **one-time initialization** of that directory when a program makes it. It is done into
//!   `<series>.init` and moved to its place only when the initializer succeeded, so a failed or
//!   interrupted initialization never leaves a half-made data directory that would pass for a
//!   good one;
//! - a **password**. `127.0.0.1` keeps the network out, not the other accounts of this computer,
//!   and a database superuser can run programs as whoever runs the server. So when the data is
//!   made, a random password is generated and kept in `secrets/` ([`crate::secrets`]); it never
//!   goes into `crisol.toml`, a configuration anyone could share, a command line, or a log. Each
//!   service takes it the way it can without showing it: a file read once at initialization
//!   (PostgreSQL `--pwfile`, MySQL/MariaDB `--init-file`), a private configuration file (Redis,
//!   Garnet), or the environment of the process (Meilisearch);
//! - a **graceful stop** where one exists, because killing a database is a recovery at the next
//!   start.
//!
//! [`DbService`] is the state machine that goes from "stopped" through initialization (when the
//! data directory is new) to a running [`Service`]; like `Service`, it never blocks and is
//! advanced with [`tick`](DbService::tick).

mod specs;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::manage::{list_installed, InstalledVersion};
use crate::paths::{CrisolPaths, PathsError};
use crate::ports::PortProbe;
use crate::process::{
    ExitReason, GroupId, GroupOptions, ProcessId, ProcessStatus, ProcessSupervisor,
};
use crate::secrets::{self, Secret, SecretsError};
use crate::service::{FailureReason, Service, ServiceEvent, ServiceState};

pub use specs::{
    credentials, init_spec, render_ini, service_spec, write_ini, Credentials, MEMCACHED_NOTE,
};

/// How the data of a service is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Initializer {
    /// Nobody: the folder is made empty and the server fills it.
    None,
    /// `mysqld --initialize-insecure`, with an `--init-file` that sets the password.
    Mysqld,
    /// `mariadb-install-db`; the password is set by an `--init-file` on the first launch.
    MariadbInstall,
    /// `initdb --pwfile`.
    Initdb,
}

/// Where the password of a service lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// The service has no password to give (Mailpit, Memcached).
    None,
    /// It is stored in the data when that is made (MySQL, MariaDB, PostgreSQL): generated at
    /// initialization, and data made before passwords existed has none.
    Stored,
    /// It is given to the server every time it starts (Redis, Garnet, Meilisearch).
    Applied,
    /// It is stored in the data, but the server makes the data itself: the account is made
    /// through the server the first time it runs (MongoDB).
    Created,
}

/// What differs between the services, as data: one entry each.
struct Descriptor {
    service: &'static str,
    /// The server, relative to the install folder.
    server: &'static str,
    /// The configuration file Crisol generates, if the service reads one.
    config_file: Option<&'static str>,
    /// How many numbers of a version decide whether two data directories are compatible.
    series_parts: usize,
    initializer: Initializer,
    auth: Auth,
    enabled: fn(&Config) -> bool,
    port: fn(&Config) -> u16,
    extra_port: Option<fn(&Config) -> u16>,
    pinned: fn(&Config) -> Option<&str>,
}

macro_rules! descriptor {
    ($field:ident, $service:literal, $server:literal, $config_file:expr, $parts:literal,
     $init:expr, $auth:expr, $extra:expr) => {
        Descriptor {
            service: $service,
            server: $server,
            config_file: $config_file,
            series_parts: $parts,
            initializer: $init,
            auth: $auth,
            enabled: |c: &Config| c.services.$field.enabled,
            port: |c: &Config| c.services.$field.port,
            extra_port: $extra,
            pinned: |c: &Config| c.services.$field.version.as_deref(),
        }
    };
}

static MYSQL: Descriptor = descriptor!(
    mysql,
    "mysql",
    "bin/mysqld.exe",
    Some("my.ini"),
    2,
    Initializer::Mysqld,
    Auth::Stored,
    None
);
static MARIADB: Descriptor = descriptor!(
    mariadb,
    "mariadb",
    "bin/mariadbd.exe",
    Some("my.ini"),
    2,
    Initializer::MariadbInstall,
    Auth::Stored,
    None
);
static POSTGRES: Descriptor = descriptor!(
    postgresql,
    "postgresql",
    "bin/postgres.exe",
    Some("postgresql.conf"),
    1,
    Initializer::Initdb,
    Auth::Stored,
    None
);
static MONGODB: Descriptor = descriptor!(
    mongodb,
    "mongodb",
    "bin/mongod.exe",
    None,
    2,
    Initializer::None,
    Auth::Created,
    None
);
static MAILPIT: Descriptor = descriptor!(
    mailpit,
    "mailpit",
    "mailpit.exe",
    None,
    1,
    Initializer::None,
    Auth::None,
    Some(|c: &Config| c.services.mailpit.smtp_port)
);
static MEILISEARCH: Descriptor = descriptor!(
    meilisearch,
    "meilisearch",
    "meilisearch-windows-amd64.exe",
    None,
    2,
    Initializer::None,
    Auth::Applied,
    None
);
static REDIS: Descriptor = descriptor!(
    redis,
    "redis",
    "redis-server.exe",
    None,
    2,
    Initializer::None,
    Auth::Applied,
    None
);
static MEMCACHED: Descriptor = descriptor!(
    memcached,
    "memcached",
    "bin/memcached.exe",
    None,
    2,
    Initializer::None,
    Auth::None,
    None
);
static GARNET: Descriptor = descriptor!(
    garnet,
    "garnet",
    "net10.0/GarnetServer.exe",
    None,
    2,
    Initializer::None,
    Auth::Applied,
    None
);

/// The data services.
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
    /// Memcached: the third-party Windows build (ADR-0017). It has no login without SASL.
    Memcached,
}

impl DbKind {
    /// All of them, MySQL first.
    pub const ALL: [Self; 9] = [
        Self::Mysql,
        Self::Mariadb,
        Self::Postgres,
        Self::Mongodb,
        Self::Mailpit,
        Self::Meilisearch,
        Self::Redis,
        Self::Garnet,
        Self::Memcached,
    ];

    fn descriptor(self) -> &'static Descriptor {
        match self {
            Self::Mysql => &MYSQL,
            Self::Mariadb => &MARIADB,
            Self::Postgres => &POSTGRES,
            Self::Mongodb => &MONGODB,
            Self::Mailpit => &MAILPIT,
            Self::Meilisearch => &MEILISEARCH,
            Self::Redis => &REDIS,
            Self::Garnet => &GARNET,
            Self::Memcached => &MEMCACHED,
        }
    }

    /// The name of the service (`mysql`, `mariadb`): in the catalog, in `crisol.toml` and in
    /// every folder.
    #[must_use]
    pub fn service(self) -> &'static str {
        self.descriptor().service
    }

    /// The kind a service name means.
    #[must_use]
    pub fn from_service(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.service() == name)
    }

    /// Where its password lives.
    #[must_use]
    pub fn auth(self) -> Auth {
        self.descriptor().auth
    }

    /// Whether `crisol.toml` enables it.
    #[must_use]
    pub fn enabled(self, config: &Config) -> bool {
        (self.descriptor().enabled)(config)
    }

    /// The port it listens on.
    #[must_use]
    pub fn port(self, config: &Config) -> u16 {
        (self.descriptor().port)(config)
    }

    /// The part of a version that decides whether two data directories are compatible: the first
    /// two numbers for MySQL and MariaDB (`8.4`, `11.8`, `10.11`), the major for PostgreSQL
    /// (`17`, `18`). A new series is a new folder, never an upgrade in place.
    #[must_use]
    pub fn series(self, version: &str) -> String {
        version
            .split('.')
            .take(self.descriptor().series_parts)
            .collect::<Vec<_>>()
            .join(".")
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
    /// The passwords could not be made, kept or read.
    #[error(transparent)]
    Secrets(#[from] SecretsError),
}

/// Everything decided about one database service before it runs. It holds no password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPlan {
    /// MySQL, MariaDB, …
    pub kind: DbKind,
    /// The installed version that will run.
    pub version: InstalledVersion,
    /// `8.4`, `11.8`…
    pub series: String,
    /// Where it listens.
    pub port: u16,
    /// A second port (Mailpit's SMTP).
    pub extra_port: Option<u16>,
    /// The name Nginx serves the service under (`mail.test` for Mailpit); the service itself
    /// answers to no other.
    pub host: Option<String>,
    /// `data/<service>/<series>/`.
    pub data: PathBuf,
    /// `data/<service>/<series>.init/`: where it is made first.
    pub staging: PathBuf,
    /// `data/<service>/<series>-files/`: the only place the SQL of MySQL and MariaDB may read
    /// and write files (`secure_file_priv`).
    pub files: PathBuf,
    /// `etc/<service>/<series>/my.ini` (`postgresql.conf` for PostgreSQL).
    pub ini: PathBuf,
    /// `etc/<service>/overrides/`: the user's own `*.ini` (`*.conf` for PostgreSQL).
    pub overrides: PathBuf,
    /// `secrets/`.
    pub secrets: PathBuf,
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
    let version = match (kind.descriptor().pinned)(config) {
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
        files: paths.data_for(service, &format!("{series}-files"))?,
        ini: etc
            .join(&series)
            .join(kind.descriptor().config_file.unwrap_or("none")),
        extra_port: kind.descriptor().extra_port.map(|port| port(config)),
        host: (kind == DbKind::Mailpit).then(|| format!("mail.{}", config.sites.tld)),
        overrides: paths.overrides_for(service)?,
        secrets: paths.secrets_dir(),
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

    /// A file of this service and series in `secrets/`, by suffix: `""` is the password itself.
    #[must_use]
    pub fn secret_path(&self, suffix: &str) -> PathBuf {
        self.secrets
            .join(format!("{}-{}{suffix}", self.kind.service(), self.series))
    }
}

/// What a project needs to connect to `kind`, from the password kept for the series that would
/// run. Reads files; starts nothing.
pub fn connection_details(
    paths: &CrisolPaths,
    config: &Config,
    kind: DbKind,
) -> Result<Credentials, DbError> {
    let plan = plan(paths, config, kind)?;
    let secret = secrets::read_secret(&plan.secret_path(""))?;
    Ok(credentials(&plan, secret.as_ref()))
}

/// What Crisol asks of a running MongoDB besides starting it: to make its first user, and to shut
/// it down by talking to it. A trait so that the tests do not need a server.
pub trait MongoAdmin: Send + Sync {
    /// Makes the administrator through the localhost exception.
    fn create_root_user(&self, port: u16, password: &Secret) -> Result<(), String>;
    /// Shuts the server down: logged in as the administrator, or with no login if the server was
    /// made before there were passwords.
    fn shutdown(&self, port: u16, password: Option<&Secret>) -> Result<(), String>;
}

/// [`MongoAdmin`] over TCP ([`crate::mongowire`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct WireMongoAdmin;

impl MongoAdmin for WireMongoAdmin {
    fn create_root_user(&self, port: u16, password: &Secret) -> Result<(), String> {
        crate::mongowire::create_root_user(port, password.expose()).map_err(|e| e.to_string())
    }

    fn shutdown(&self, port: u16, password: Option<&Secret>) -> Result<(), String> {
        match password {
            Some(password) => crate::mongowire::shutdown(port, password.expose()),
            None => crate::mongowire::shutdown_without_login(port),
        }
        .map_err(|e| e.to_string())
    }
}

/// How many times the first user is tried before the start is given up.
const FIRST_USER_ATTEMPTS: u32 = 5;

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
    /// The password of this run, if the service has one. Never printed (`Secret`).
    secret: Option<Secret>,
    mongo: Arc<dyn MongoAdmin>,
    /// Tries made at the first user of MongoDB.
    first_user_attempts: u32,
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
            secret: None,
            mongo: Arc::new(WireMongoAdmin),
            first_user_attempts: 0,
        }
    }

    /// Talks to MongoDB through `admin` instead of over TCP (tests).
    #[must_use]
    pub fn with_mongo(mut self, admin: Arc<dyn MongoAdmin>) -> Self {
        self.mongo = admin;
        self
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

    /// Makes a new password and keeps it in `secrets/`, replacing any older one.
    fn new_secret(&self) -> Result<Secret, DbError> {
        fs::create_dir_all(&self.plan.secrets)
            .map_err(|e| DbError::Unusable(format!("{}: {e}", self.plan.secrets.display())))?;
        let new = secrets::generate_password()?;
        secrets::write_file(&self.plan.secret_path(""), new.expose())?;
        Ok(new)
    }

    /// Chooses the password of this run. A service whose password is stored in its data gets a
    /// new one only when that data is about to be made; data made before passwords existed has
    /// none and runs as it was made. A service that is given its password at every start gets
    /// one the first time and keeps it.
    fn choose_secret(&mut self, will_make_data: bool) -> Result<(), DbError> {
        let file = self.plan.secret_path("");
        self.secret = match self.plan.kind.auth() {
            Auth::None => None,
            Auth::Stored if will_make_data => Some(self.new_secret()?),
            // The account is made after the first run: until then a marker says it is owed.
            Auth::Created if will_make_data => {
                let new = self.new_secret()?;
                secrets::write_file(
                    &self.plan.secret_path(".pending"),
                    "the first user is owed\n",
                )?;
                Some(new)
            }
            Auth::Stored | Auth::Created => secrets::read_secret(&file)?,
            Auth::Applied => match secrets::read_secret(&file)? {
                Some(existing) => Some(existing),
                None => Some(self.new_secret()?),
            },
        };
        Ok(())
    }

    /// Starts it. From `Stopped` or `Failed`; anything else changes nothing.
    pub fn start(&mut self, now: Instant) -> Vec<ServiceEvent> {
        let mut events = Vec::new();
        if !matches!(self.state, ServiceState::Stopped | ServiceState::Failed(_)) {
            return events;
        }
        let initializes = !self.plan.data.is_dir()
            && self.plan.kind.descriptor().initializer != Initializer::None;
        self.first_user_attempts = 0;
        let prepared =
            write_ini(&self.plan).and_then(|()| self.choose_secret(!self.plan.data.is_dir()));
        if let Err(e) = prepared {
            self.fail(e.to_string(), &mut events);
            return events;
        }
        if self.plan.data.is_dir() {
            self.launch(now, &mut events);
        } else if initializes {
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
        if let Err(e) = specs::write_runtime_files(&self.plan, self.secret.as_ref()) {
            self.fail(e.to_string(), events);
            return;
        }
        let mut service = Service::new(
            service_spec(&self.plan, self.secret.as_ref(), &self.mongo),
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
            specs::write_init_files(&self.plan, self.secret.as_ref()).map_err(|e| e.to_string())?;
            let group = self
                .supervisor
                .create_group("init", GroupOptions::default())
                .map_err(|e| e.to_string())?;
            match self
                .supervisor
                .spawn(group, &init_spec(&self.plan, self.secret.as_ref()))
            {
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

    /// Ends an initialization (killing it and deleting its staging folder and the files it was
    /// given) and forgets the phase.
    fn close_phase(&mut self) {
        if let Phase::Initializing { group, .. } = self.phase {
            let _ = self.supervisor.kill_group(group);
            let _ = self.supervisor.close_group(group);
            let _ = fs::remove_dir_all(&self.plan.staging);
            specs::remove_init_files(&self.plan, true);
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
                match self.state {
                    ServiceState::Stopped => self.phase = Phase::Idle,
                    // MariaDB has taken its password: the file that carried it is not needed.
                    ServiceState::Running => {
                        specs::remove_first_launch_file(&self.plan);
                        self.make_first_user(&mut events);
                    }
                    _ => {}
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
                        // What the initializer was given is spent (MariaDB's is kept: it
                        // carries the password to the first launch).
                        specs::remove_init_files(&self.plan, false);
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

    /// MongoDB is running for the first time with its data empty: makes the administrator with
    /// the generated password, the way the localhost exception allows. Until it is done the
    /// service is open to whoever asks first, for the moment this takes. A server that refuses
    /// it is a failed start, and says so.
    fn make_first_user(&mut self, events: &mut Vec<ServiceEvent>) {
        let pending = self.plan.secret_path(".pending");
        let (true, Some(secret)) = (pending.is_file(), self.secret.clone()) else {
            return;
        };
        match self.mongo.create_root_user(self.plan.port, &secret) {
            Ok(()) => secrets::remove_file(&pending),
            Err(why) => {
                self.first_user_attempts += 1;
                if self.first_user_attempts >= FIRST_USER_ATTEMPTS {
                    if let Phase::Running(service) = &mut self.phase {
                        service.abort();
                    }
                    self.fail(
                        format!(
                            "the administrator of MongoDB could not be made ({why}); stop it, delete {} and start it again",
                            self.plan.data.display()
                        ),
                        events,
                    );
                }
            }
        }
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
