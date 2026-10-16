//! Model of the user's global configuration: `crisol.toml`.
//!
//! This module only **reads**. The file belongs to the user (ADR-0003): Crisol never
//! rewrites it on its own, and every edit goes through `toml_edit` (F1.3).
//!
//! Rules enforced here:
//! - An empty or missing file is a valid configuration made of defaults.
//! - Unknown fields are reported as warnings, never as errors, so a newer or hand-edited
//!   file keeps working.
//! - There are no secrets in this file (ADR-0006).
//! - Each setting has a single source of truth: this file. Generated files under `etc/`
//!   are derived from it and are never read back.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths::CrisolPaths;
use crate::version::VersionReq;

/// Top-level configuration with every default filled in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Versions used when a project does not pin its own.
    pub defaults: Defaults,
    /// Local sites: domain suffix and parked folders.
    pub sites: Sites,
    /// PHP `FastCGI` pool.
    pub php: PhpConfig,
    /// Every other service.
    pub services: Services,
}

/// Global default versions. `None` means "the highest installed version".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// Default PHP version, e.g. `"8.4"`.
    pub php: Option<String>,
    /// Default Node version, e.g. `"22"`.
    pub node: Option<String>,
}

/// Local sites settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Sites {
    /// Domain suffix without the leading dot. Default `test` (ADR-0007).
    pub tld: String,
    /// Parked folders: every subfolder is a site. Relative paths are resolved against
    /// `CRISOL_HOME`; the default is the `www` folder.
    pub parked: Vec<PathBuf>,
}

impl Default for Sites {
    fn default() -> Self {
        Self {
            tld: "test".to_owned(),
            parked: vec![PathBuf::from("www")],
        }
    }
}

/// PHP on Windows has no php-fpm: Crisol keeps a pool of `php-cgi.exe` processes, each on
/// its own local port, starting at `base_port`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PhpConfig {
    /// Whether the pool is started with the web server.
    pub enabled: bool,
    /// Port of the first worker; worker `n` listens on `base_port + n`.
    pub base_port: u16,
    /// Number of worker processes per active PHP version.
    pub workers: u16,
    /// Requests served by a worker before it is recycled (`PHP_FCGI_MAX_REQUESTS`).
    pub max_requests: u32,
    /// `date.timezone`. `None` means the system time zone.
    pub timezone: Option<String>,
}

impl Default for PhpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_port: 9100,
            workers: 4,
            max_requests: 500,
            timezone: None,
        }
    }
}

impl PhpConfig {
    /// Last port used by the pool.
    fn last_port(&self) -> u32 {
        u32::from(self.base_port) + u32::from(self.workers.max(1)) - 1
    }
}

/// Nginx listens on two ports.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NginxConfig {
    /// Whether Nginx starts with the suite.
    pub enabled: bool,
    /// HTTP port.
    pub http_port: u16,
    /// HTTPS port.
    pub https_port: u16,
    /// Pinned version; `None` means the highest installed.
    pub version: Option<String>,
}

impl Default for NginxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            http_port: 80,
            https_port: 443,
            version: None,
        }
    }
}

/// Defines a service with a single port. Each service gets its own type so that
/// `[services.redis]` with only `enabled = true` still gets port 6379.
macro_rules! single_port_service {
    ($(#[$doc:meta])* $name:ident, port: $port:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
        #[serde(default)]
        pub struct $name {
            /// Whether the service starts with the suite.
            pub enabled: bool,
            /// Listening port.
            pub port: u16,
            /// Pinned version; `None` means the highest installed.
            pub version: Option<String>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self { enabled: false, port: $port, version: None }
            }
        }
    };
}

single_port_service!(
    /// MySQL. Shares port 3306 with MariaDB; only one of the two may be active (ADR-0007).
    MysqlConfig,
    port: 3306
);
single_port_service!(
    /// MariaDB.
    MariadbConfig,
    port: 3306
);
single_port_service!(
    /// PostgreSQL.
    PostgresqlConfig,
    port: 5432
);
single_port_service!(
    /// Redis.
    RedisConfig,
    port: 6379
);
single_port_service!(
    /// MongoDB.
    MongodbConfig,
    port: 27017
);
single_port_service!(
    /// Memcached.
    MemcachedConfig,
    port: 11211
);
single_port_service!(
    /// Meilisearch.
    MeilisearchConfig,
    port: 7700
);

/// Mailpit has a web UI and an SMTP port.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MailpitConfig {
    /// Whether Mailpit starts with the suite.
    pub enabled: bool,
    /// Web UI port.
    pub port: u16,
    /// SMTP port.
    pub smtp_port: u16,
    /// Pinned version; `None` means the highest installed.
    pub version: Option<String>,
}

impl Default for MailpitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8025,
            smtp_port: 1025,
            version: None,
        }
    }
}

/// Settings of every service other than PHP.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Services {
    /// Web server.
    pub nginx: NginxConfig,
    /// MySQL.
    pub mysql: MysqlConfig,
    /// MariaDB.
    pub mariadb: MariadbConfig,
    /// PostgreSQL.
    pub postgresql: PostgresqlConfig,
    /// Redis.
    pub redis: RedisConfig,
    /// MongoDB.
    pub mongodb: MongodbConfig,
    /// Memcached.
    pub memcached: MemcachedConfig,
    /// Meilisearch.
    pub meilisearch: MeilisearchConfig,
    /// Mailpit.
    pub mailpit: MailpitConfig,
}

/// Something suspicious that does not stop the configuration from loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// A field Crisol does not know, with its dotted path (e.g. `services.redis.colour`).
    UnknownField(String),
    /// Two enabled services want the same port.
    PortConflict {
        /// The contested port.
        port: u16,
        /// Services that want it.
        services: Vec<String>,
    },
    /// `sites.tld` is not a name reserved for local use: it can be a real public domain.
    PublicTld(String),
    /// MySQL and MariaDB are alternatives and cannot be enabled together (ADR-0007).
    ExclusiveServices {
        /// First service.
        first: String,
        /// Second service.
        second: String,
    },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownField(path) => write!(f, "unknown field `{path}` is ignored"),
            Self::PortConflict { port, services } => {
                write!(
                    f,
                    "port {port} is used by more than one enabled service: {}",
                    services.join(", ")
                )
            }
            Self::PublicTld(tld) => write!(
                f,
                "`sites.tld = \"{tld}\"` is not reserved for local use (test, localhost, example,                  invalid, local, internal, home.arpa): it can collide with real domains, and no                  HTTPS is offered for it"
            ),
            Self::ExclusiveServices { first, second } => {
                write!(
                    f,
                    "`{first}` and `{second}` are alternatives; enable only one of them"
                )
            }
        }
    }
}

/// A configuration that could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("cannot read `{}`: {source}", path.display())]
    Read {
        /// File that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The text is not valid TOML or a value has the wrong type.
    #[error("invalid crisol.toml: {0}")]
    Parse(#[from] toml::de::Error),
    /// A value is well-formed but not acceptable.
    #[error("invalid value for `{field}`: {reason}")]
    Invalid {
        /// Dotted path of the field.
        field: String,
        /// Why it was rejected.
        reason: String,
    },
}

/// A configuration together with the warnings found while loading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    /// The configuration.
    pub config: Config,
    /// Non-fatal findings, in a stable order.
    pub warnings: Vec<ConfigWarning>,
}

impl Config {
    /// Parses `crisol.toml` text. Empty text yields the defaults.
    pub fn parse(text: &str) -> Result<LoadedConfig, ConfigError> {
        let (config, mut warnings): (Config, _) = deserialize_reporting_unknown(text)?;
        config.validate()?;
        warnings.extend(config.conflicts());
        Ok(LoadedConfig { config, warnings })
    }

    /// Loads the file at `path`. A missing file is not an error: it yields the defaults and
    /// is **not** created (the file belongs to the user).
    pub fn load(path: &Path) -> Result<LoadedConfig, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LoadedConfig {
                config: Self::default(),
                warnings: Vec::new(),
            }),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Loads `crisol.toml` from the Crisol home.
    pub fn load_from(paths: &CrisolPaths) -> Result<LoadedConfig, ConfigError> {
        Self::load(&paths.config_file())
    }

    /// Absolute parked directories: relative entries are resolved against the Crisol home.
    #[must_use]
    pub fn parked_dirs(&self, paths: &CrisolPaths) -> Vec<PathBuf> {
        self.sites
            .parked
            .iter()
            .map(|dir| {
                if dir.is_absolute() {
                    dir.clone()
                } else {
                    paths.home().join(dir)
                }
            })
            .collect()
    }

    fn validate(&self) -> Result<(), ConfigError> {
        validate_tld(&self.sites.tld)?;
        for (field, version) in self.pinned_versions() {
            VersionReq::parse(version).map_err(|_| invalid(field, "not a valid version"))?;
        }
        if let Some(zone) = &self.php.timezone {
            if zone.trim().is_empty() {
                return Err(invalid("php.timezone", "must not be empty"));
            }
        }
        if self.php.workers == 0 {
            return Err(invalid("php.workers", "must be at least 1"));
        }
        if self.php.max_requests == 0 {
            return Err(invalid("php.max_requests", "must be at least 1"));
        }
        if self.php.last_port() > u32::from(u16::MAX) {
            return Err(invalid(
                "php.base_port",
                "base_port + workers exceeds 65535",
            ));
        }
        for (field, port) in self.single_ports() {
            if port == 0 {
                return Err(invalid(field, "port must be between 1 and 65535"));
            }
        }
        if self.services.nginx.http_port == self.services.nginx.https_port {
            return Err(invalid(
                "services.nginx.https_port",
                "must differ from http_port",
            ));
        }
        Ok(())
    }

    fn pinned_versions(&self) -> Vec<(&'static str, &str)> {
        let s = &self.services;
        let candidates = [
            ("defaults.php", &self.defaults.php),
            ("defaults.node", &self.defaults.node),
            ("services.nginx.version", &s.nginx.version),
            ("services.mysql.version", &s.mysql.version),
            ("services.mariadb.version", &s.mariadb.version),
            ("services.postgresql.version", &s.postgresql.version),
            ("services.redis.version", &s.redis.version),
            ("services.mongodb.version", &s.mongodb.version),
            ("services.memcached.version", &s.memcached.version),
            ("services.meilisearch.version", &s.meilisearch.version),
            ("services.mailpit.version", &s.mailpit.version),
        ];
        candidates
            .into_iter()
            .filter_map(|(field, v)| v.as_deref().map(|v| (field, v)))
            .collect()
    }

    /// The ports of every single-port service (Nginx, databases, Mailpit…), enabled or not:
    /// what a PHP pool must keep away from. The pool's own range is not in the list.
    #[must_use]
    pub fn reserved_ports(&self) -> std::collections::BTreeSet<u16> {
        self.single_ports()
            .into_iter()
            .filter(|(field, _)| *field != "php.base_port")
            .map(|(_, port)| port)
            .collect()
    }

    fn single_ports(&self) -> Vec<(&'static str, u16)> {
        let s = &self.services;
        vec![
            ("php.base_port", self.php.base_port),
            ("services.nginx.http_port", s.nginx.http_port),
            ("services.nginx.https_port", s.nginx.https_port),
            ("services.mysql.port", s.mysql.port),
            ("services.mariadb.port", s.mariadb.port),
            ("services.postgresql.port", s.postgresql.port),
            ("services.redis.port", s.redis.port),
            ("services.mongodb.port", s.mongodb.port),
            ("services.memcached.port", s.memcached.port),
            ("services.meilisearch.port", s.meilisearch.port),
            ("services.mailpit.port", s.mailpit.port),
            ("services.mailpit.smtp_port", s.mailpit.smtp_port),
        ]
    }

    /// Every port wanted by an enabled service, with the service name.
    fn enabled_ports(&self) -> Vec<(u16, &'static str)> {
        let s = &self.services;
        let mut ports = Vec::new();
        if self.php.enabled {
            let last = u16::try_from(self.php.last_port()).unwrap_or(u16::MAX);
            ports.extend((self.php.base_port..=last).map(|p| (p, "php")));
        }
        if s.nginx.enabled {
            ports.push((s.nginx.http_port, "nginx"));
            ports.push((s.nginx.https_port, "nginx"));
        }
        let simple = [
            ("mysql", s.mysql.enabled, s.mysql.port),
            ("mariadb", s.mariadb.enabled, s.mariadb.port),
            ("postgresql", s.postgresql.enabled, s.postgresql.port),
            ("redis", s.redis.enabled, s.redis.port),
            ("mongodb", s.mongodb.enabled, s.mongodb.port),
            ("memcached", s.memcached.enabled, s.memcached.port),
            ("meilisearch", s.meilisearch.enabled, s.meilisearch.port),
            ("mailpit", s.mailpit.enabled, s.mailpit.port),
            ("mailpit", s.mailpit.enabled, s.mailpit.smtp_port),
        ];
        ports.extend(
            simple
                .into_iter()
                .filter(|(_, on, _)| *on)
                .map(|(n, _, p)| (p, n)),
        );
        ports
    }

    fn conflicts(&self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();
        if !crate::ca::is_special_use(&self.sites.tld) {
            warnings.push(ConfigWarning::PublicTld(self.sites.tld.clone()));
        }
        if self.services.mysql.enabled && self.services.mariadb.enabled {
            warnings.push(ConfigWarning::ExclusiveServices {
                first: "mysql".to_owned(),
                second: "mariadb".to_owned(),
            });
        }
        let mut by_port: BTreeMap<u16, Vec<&str>> = BTreeMap::new();
        for (port, name) in self.enabled_ports() {
            let owners = by_port.entry(port).or_default();
            if !owners.contains(&name) {
                owners.push(name);
            }
        }
        for (port, owners) in by_port {
            let only_the_exclusive_pair =
                owners.len() == 2 && owners.contains(&"mysql") && owners.contains(&"mariadb");
            if owners.len() > 1 && !only_the_exclusive_pair {
                warnings.push(ConfigWarning::PortConflict {
                    port,
                    services: owners.into_iter().map(str::to_owned).collect(),
                });
            }
        }
        warnings
    }
}

/// Deserializes TOML text, collecting unknown fields as warnings instead of failing.
///
/// Shared by every Crisol TOML model. A leading byte-order mark is ignored: Windows editors
/// may save UTF-8 with one, and TOML itself has none.
pub(crate) fn deserialize_reporting_unknown<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<(T, Vec<ConfigWarning>), toml::de::Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let deserializer = toml::de::Deserializer::parse(text)?;
    let mut unknown = Vec::new();
    let value: T = serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()))?;
    let warnings = unknown
        .into_iter()
        .map(ConfigWarning::UnknownField)
        .collect();
    Ok((value, warnings))
}

pub(crate) fn invalid(field: &str, reason: &str) -> ConfigError {
    ConfigError::Invalid {
        field: field.to_owned(),
        reason: reason.to_owned(),
    }
}

/// A domain suffix: lowercase labels of letters, digits and hyphens, separated by dots.
fn validate_tld(tld: &str) -> Result<(), ConfigError> {
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if tld.split('.').all(label_ok) {
        Ok(())
    } else {
        Err(invalid(
            "sites.tld",
            "use lowercase letters, digits and hyphens, without leading or trailing dots",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn load(text: &str) -> LoadedConfig {
        Config::parse(text).expect("valid configuration")
    }

    fn unknown(loaded: &LoadedConfig) -> Vec<&str> {
        loaded
            .warnings
            .iter()
            .filter_map(|w| match w {
                ConfigWarning::UnknownField(p) => Some(p.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_file_is_a_valid_configuration_of_defaults() {
        let loaded = load("");
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings, Vec::new());
    }

    #[test]
    fn comments_only_file_is_also_defaults() {
        let loaded = load("# my crisol\n\n  # nothing here yet\n");
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn defaults_match_the_plan() {
        let c = Config::default();
        assert_eq!(c.sites.tld, "test");
        assert_eq!(c.sites.parked, vec![PathBuf::from("www")]);
        assert_eq!(c.defaults.php, None);
        assert_eq!(c.defaults.node, None);
        assert!(c.php.enabled && c.services.nginx.enabled);
        assert_eq!(
            (c.services.nginx.http_port, c.services.nginx.https_port),
            (80, 443)
        );
        assert_eq!(
            (c.php.base_port, c.php.workers, c.php.max_requests),
            (9100, 4, 500)
        );
        assert!(c.php.timezone.is_none());
        assert_eq!(c.services.mysql.port, 3306);
        assert_eq!(c.services.mariadb.port, 3306);
        assert_eq!(c.services.postgresql.port, 5432);
        assert_eq!(c.services.redis.port, 6379);
        assert_eq!(c.services.mongodb.port, 27017);
        assert_eq!(c.services.memcached.port, 11211);
        assert_eq!(c.services.meilisearch.port, 7700);
        assert_eq!(
            (c.services.mailpit.port, c.services.mailpit.smtp_port),
            (8025, 1025)
        );
        assert!(!c.services.mysql.enabled && !c.services.redis.enabled);
    }

    #[test]
    fn a_partial_table_keeps_the_other_defaults_of_that_service() {
        let loaded = load("[services.redis]\nenabled = true\n");
        assert!(loaded.config.services.redis.enabled);
        assert_eq!(
            loaded.config.services.redis.port, 6379,
            "port keeps redis default"
        );

        let loaded = load("[services.postgresql]\nport = 5433\n");
        assert_eq!(loaded.config.services.postgresql.port, 5433);
        assert!(!loaded.config.services.postgresql.enabled);
    }

    #[test]
    fn user_values_override_defaults() {
        let loaded = load(
            r#"
            [defaults]
            php = "8.3"
            node = "22"

            [sites]
            tld = "localhost"
            parked = ["www", "D:\\Proyectos"]

            [php]
            workers = 2
            timezone = "America/Mexico_City"

            [services.nginx]
            http_port = 8080
            https_port = 8443
            "#,
        );
        let c = loaded.config;
        assert_eq!(c.defaults.php.as_deref(), Some("8.3"));
        assert_eq!(c.defaults.node.as_deref(), Some("22"));
        assert_eq!(c.sites.tld, "localhost");
        assert_eq!(c.sites.parked.len(), 2);
        assert_eq!(c.php.workers, 2);
        assert_eq!(
            c.php.base_port, 9100,
            "untouched php field keeps its default"
        );
        assert_eq!(c.php.timezone.as_deref(), Some("America/Mexico_City"));
        assert_eq!(
            (c.services.nginx.http_port, c.services.nginx.https_port),
            (8080, 8443)
        );
        assert_eq!(loaded.warnings, Vec::new());
    }

    #[test]
    fn unknown_fields_warn_but_do_not_fail() {
        let loaded = load(
            r#"
            colour = "blue"

            [defaults]
            php = "8.4"
            ruby = "3.3"

            [services.redis]
            enabled = true
            maxmemory = "1gb"

            [services.rabbitmq]
            enabled = true
            "#,
        );
        assert_eq!(loaded.config.defaults.php.as_deref(), Some("8.4"));
        assert!(
            loaded.config.services.redis.enabled,
            "known fields still apply"
        );
        let mut found = unknown(&loaded);
        found.sort_unstable();
        assert_eq!(
            found,
            vec![
                "colour",
                "defaults.ruby",
                "services.rabbitmq",
                "services.redis.maxmemory"
            ]
        );
        let text = loaded.warnings[0].to_string();
        assert!(text.contains("unknown field"), "{text}");
    }

    #[test]
    fn wrong_types_and_bad_toml_are_errors_with_a_location() {
        let err = Config::parse("[services.redis]\nport = \"abc\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
        assert!(err.to_string().contains("line 2"), "{err}");

        let err = Config::parse("this is = = not toml").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));

        let err = Config::parse("[services.redis]\nport = 70000\n").unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse(_)),
            "u16 overflow is a parse error"
        );
    }

    #[test]
    fn unacceptable_values_are_rejected_with_the_field_name() {
        let cases = [
            ("[sites]\ntld = \"Test\"\n", "sites.tld"),
            ("[sites]\ntld = \".test\"\n", "sites.tld"),
            ("[sites]\ntld = \"a..b\"\n", "sites.tld"),
            ("[sites]\ntld = \"\"\n", "sites.tld"),
            ("[services.redis]\nport = 0\n", "services.redis.port"),
            ("[php]\nworkers = 0\n", "php.workers"),
            ("[php]\nmax_requests = 0\n", "php.max_requests"),
            ("[php]\nbase_port = 65535\nworkers = 4\n", "php.base_port"),
            ("[php]\ntimezone = \"  \"\n", "php.timezone"),
            ("[defaults]\nphp = \"../8.4\"\n", "defaults.php"),
            (
                "[services.mysql]\nversion = \"8 4\"\n",
                "services.mysql.version",
            ),
            (
                "[services.nginx]\nhttp_port = 80\nhttps_port = 80\n",
                "services.nginx.https_port",
            ),
        ];
        for (text, field) in cases {
            match Config::parse(text) {
                Err(ConfigError::Invalid { field: got, .. }) => assert_eq!(got, field, "{text}"),
                other => panic!("`{text}` should be invalid for {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn nested_tld_is_allowed() {
        assert_eq!(
            load("[sites]\ntld = \"dev.test\"\n").config.sites.tld,
            "dev.test"
        );
    }

    #[test]
    fn mysql_and_mariadb_together_warn_as_exclusive_not_as_a_port_clash() {
        let loaded = load("[services.mysql]\nenabled = true\n[services.mariadb]\nenabled = true\n");
        assert_eq!(
            loaded.warnings,
            vec![ConfigWarning::ExclusiveServices {
                first: "mysql".to_owned(),
                second: "mariadb".to_owned()
            }]
        );
    }

    #[test]
    fn mysql_with_postgresql_is_fine() {
        let loaded =
            load("[services.mysql]\nenabled = true\n[services.postgresql]\nenabled = true\n");
        assert_eq!(loaded.warnings, Vec::new());
    }

    #[test]
    fn port_clashes_between_enabled_services_warn() {
        let loaded = load("[services.redis]\nenabled = true\nport = 80\n");
        assert_eq!(
            loaded.warnings,
            vec![ConfigWarning::PortConflict {
                port: 80,
                services: vec!["nginx".to_owned(), "redis".to_owned()]
            }]
        );
    }

    #[test]
    fn disabled_services_do_not_clash() {
        let loaded = load("[services.redis]\nport = 80\n");
        assert_eq!(loaded.warnings, Vec::new());
    }

    #[test]
    fn the_php_pool_range_is_checked_against_other_services() {
        let loaded = load("[php]\nbase_port = 9100\nworkers = 4\n[services.memcached]\nenabled = true\nport = 9102\n");
        assert_eq!(
            loaded.warnings,
            vec![ConfigWarning::PortConflict {
                port: 9102,
                services: vec!["php".to_owned(), "memcached".to_owned()]
            }]
        );
    }

    #[test]
    fn missing_file_yields_defaults_and_is_not_created() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("home")).unwrap();
        let loaded = Config::load_from(&paths).unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(
            !paths.config_file().exists(),
            "loading must never create the user's file"
        );
        assert!(!paths.home().exists());
    }

    #[test]
    fn existing_file_is_read_and_left_untouched() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path()).unwrap();
        let text = "# keep me\n[defaults]\nphp = \"8.2\" # inline\n";
        fs::write(paths.config_file(), text).unwrap();
        let loaded = Config::load_from(&paths).unwrap();
        assert_eq!(loaded.config.defaults.php.as_deref(), Some("8.2"));
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), text);
    }

    #[test]
    fn unreadable_path_is_a_read_error() {
        let dir = TempDir::new().unwrap();
        // A directory where the file should be.
        let err = Config::load(dir.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    #[test]
    fn parked_dirs_resolve_relative_paths_against_the_home() {
        let dir = TempDir::new().unwrap();
        let paths = CrisolPaths::new(dir.path().join("home")).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        let mut config = Config::default();
        config.sites.parked = vec![PathBuf::from("www"), elsewhere.clone()];
        assert_eq!(
            config.parked_dirs(&paths),
            vec![paths.home().join("www"), elsewhere]
        );
    }

    #[test]
    fn a_tld_that_is_not_reserved_for_local_use_is_a_warning_not_an_error() {
        for tld in [
            "test",
            "localhost",
            "example",
            "invalid",
            "local",
            "internal",
            "home.arpa",
            "dev.test",
        ] {
            let loaded = load(&format!(
                "[sites]
tld = \"{tld}\"
"
            ));
            assert_eq!(loaded.warnings, vec![], "{tld}");
        }
        for tld in ["dev", "app", "com", "lan", "test.com"] {
            let loaded = load(&format!(
                "[sites]
tld = \"{tld}\"
"
            ));
            assert_eq!(loaded.config.sites.tld, tld, "it still loads");
            assert_eq!(
                loaded.warnings,
                vec![ConfigWarning::PublicTld(tld.to_owned())],
                "{tld}"
            );
            let text = loaded.warnings[0].to_string();
            assert!(
                text.contains("collide with real domains") && text.contains("HTTPS"),
                "{text}"
            );
        }
    }
}
