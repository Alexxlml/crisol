//! Which installed version of a tool applies in a directory (F4.2).
//!
//! This is the logic behind the shims (`php`, `node`, `npm`, `npx`, `composer`): the shim
//! only asks "which one here?" and runs the answer. The order is the one in
//! `docs/arquitectura.md`, first match wins:
//!
//! 1. the environment variable `CRISOL_PHP` / `CRISOL_NODE` (a temporary override);
//! 2. the nearest trusted `.crisol.toml` that pins the tool;
//! 3. PHP: `.php-version`. Node: `.nvmrc`, `.node-version`, or `engines.node` of a
//!    `package.json`;
//! 4. PHP: `require.php` of a `composer.json`, as a constraint: the **default** version if it
//!    satisfies it, else the highest installed version that does (ADR-0012);
//! 5. the default of `crisol.toml` (`[defaults]`);
//! 6. the catalog's recommended line if installed (ADR-0012), else the highest installed
//!    stable version that still receives security fixes.
//!
//! The *default* is the `[defaults]` entry of `crisol.toml` or, without one, the line the
//! catalog recommends (`[recommended]`).
//!
//! Everything found by walking up from the current directory follows the rules of
//! ADR-0010: files of an untrusted owner are skipped with a warning, and the walk stops at
//! the first `.git`. A request that names a version that is not installed is an **error**,
//! not a reason to try the next source: silently running another version would be worse
//! than saying so. Only a file that cannot be understood is skipped, with a warning.
//!
//! Nothing here touches the system: owners come through [`FileOwners`] and the environment
//! through a closure, so it is all tested with temporary folders.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::catalog::Catalog;
use crate::config::{Config, ConfigError};
use crate::constraint::{Constraint, Flavor};
use crate::manage::{list_installed, resolve_installed, InstalledVersion, ManageError};
use crate::paths::CrisolPaths;
use crate::project::{find_files_upward, load_project, FileOwners};
use crate::version::VersionReq;

/// A tool Crisol installs versions of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// PHP.
    Php,
    /// Node.js.
    Node,
}

impl Tool {
    /// The service name in the catalog and under `bin/`.
    #[must_use]
    pub fn service(self) -> &'static str {
        match self {
            Self::Php => "php",
            Self::Node => "node",
        }
    }

    /// The variable that overrides the version for this terminal.
    #[must_use]
    pub fn env_var(self) -> &'static str {
        match self {
            Self::Php => "CRISOL_PHP",
            Self::Node => "CRISOL_NODE",
        }
    }
}

/// A command a shim stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `php`.
    Php,
    /// `node`.
    Node,
    /// `npm`, which ships with Node.
    Npm,
    /// `npx`, which ships with Node.
    Npx,
    /// `composer`, which runs on the resolved PHP.
    Composer,
}

impl Command {
    /// The command called `name` (without extension, any case).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "php" => Self::Php,
            "node" => Self::Node,
            "npm" => Self::Npm,
            "npx" => Self::Npx,
            "composer" => Self::Composer,
            _ => return None,
        })
    }

    /// The tool whose version this command follows.
    #[must_use]
    pub fn tool(self) -> Tool {
        match self {
            Self::Php | Self::Composer => Tool::Php,
            Self::Node | Self::Npm | Self::Npx => Tool::Node,
        }
    }
}

/// Where the chosen version came from, for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The override variable of this terminal.
    Env(&'static str),
    /// A `.crisol.toml`.
    ProjectFile(PathBuf),
    /// A `.php-version`, `.nvmrc` or `.node-version`.
    VersionFile(PathBuf),
    /// `require.php` of a `composer.json`.
    ComposerJson(PathBuf),
    /// `engines.node` of a `package.json`.
    PackageJson(PathBuf),
    /// `[defaults]` in `crisol.toml`.
    GlobalDefault,
    /// Nothing asked for a version: the line the catalog recommends.
    CatalogRecommendation,
    /// Nothing asked for a version: the highest one installed.
    HighestInstalled,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(var) => write!(f, "the {var} variable"),
            Self::ProjectFile(p) | Self::VersionFile(p) => write!(f, "{}", p.display()),
            Self::ComposerJson(p) => write!(f, "require.php in {}", p.display()),
            Self::PackageJson(p) => write!(f, "engines.node in {}", p.display()),
            Self::GlobalDefault => f.write_str("the defaults in crisol.toml"),
            Self::CatalogRecommendation => f.write_str("the line the catalog recommends"),
            Self::HighestInstalled => f.write_str("the highest installed version"),
        }
    }
}

/// The version chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The tool.
    pub tool: Tool,
    /// The installed version to run.
    pub version: InstalledVersion,
    /// What was asked for (`8.3`, `^8.1`), as written.
    pub request: String,
    /// Where the request came from.
    pub source: Source,
}

/// The choice, and what to tell the user on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The choice.
    pub resolved: Resolved,
    /// Files skipped (strangers, unreadable) and anything else worth a line on stderr.
    pub warnings: Vec<String>,
}

/// Why no version could be chosen.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// A source asked for a version that is not installed.
    #[error(
        "{tool} {request} is required by {origin}, but it is not installed (installed: {})",
        installed_list(installed)
    )]
    NotInstalled {
        /// Tool name.
        tool: &'static str,
        /// What was asked for.
        request: String,
        /// Where it was asked.
        origin: Source,
        /// The versions that are installed.
        installed: Vec<String>,
    },
    /// Nothing is installed and nothing asked for a version.
    #[error("no {0} version is installed")]
    NothingInstalled(&'static str),
    /// The override variable does not hold a version.
    #[error("{var} is `{value}`, which is not a version: use numbers such as `8.3`")]
    BadEnvironment {
        /// The variable.
        var: &'static str,
        /// Its value.
        value: String,
    },
    /// `.crisol.toml` or `crisol.toml` is broken.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The installed versions could not be listed.
    #[error(transparent)]
    Manage(#[from] ManageError),
}

fn installed_list(installed: &[String]) -> String {
    if installed.is_empty() {
        "none".to_owned()
    } else {
        installed.join(", ")
    }
}

/// What [`resolve`] needs to know about where it runs.
pub struct ResolveContext<'a> {
    /// Where Crisol lives.
    pub paths: &'a CrisolPaths,
    /// The directory the command runs in.
    pub cwd: &'a Path,
    /// Stop climbing here (like `GIT_CEILING_DIRECTORIES`), if set.
    pub ceiling: Option<&'a Path>,
    /// Who owns files (ADR-0010).
    pub owners: &'a dyn FileOwners,
    /// Reads an environment variable.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The catalog: the recommended line and which versions no longer get security fixes.
    pub catalog: &'a Catalog,
}

impl fmt::Debug for ResolveContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolveContext")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// Chooses the installed version of `tool` that applies in `ctx.cwd`.
pub fn resolve(tool: Tool, ctx: &ResolveContext<'_>) -> Result<Resolution, ResolveError> {
    let mut warnings = Vec::new();
    let installed: Vec<InstalledVersion> = list_installed(ctx.paths)?
        .into_iter()
        .filter(|i| i.service == tool.service())
        .collect();
    let by_request = |request: &str, source: Source, warnings: Vec<String>| {
        pick_request(tool, request, source, &installed, ctx, warnings)
    };

    // 1. the variable of this terminal
    if let Some(value) = (ctx.env)(tool.env_var()).map(|v| v.trim().to_owned()) {
        if !value.is_empty() {
            if VersionReq::parse(&value).is_err() {
                return Err(ResolveError::BadEnvironment {
                    var: tool.env_var(),
                    value,
                });
            }
            return by_request(&value, Source::Env(tool.env_var()), warnings);
        }
    }

    // 2. a pin in the nearest trusted .crisol.toml
    let lookup = load_project(ctx.cwd, ctx.ceiling, ctx.owners)?;
    warnings.extend(lookup.ignored.iter().map(ToString::to_string));
    if let Some(project) = lookup.project {
        warnings.extend(project.warnings.iter().map(ToString::to_string));
        let pin = match tool {
            Tool::Php => project.config.php,
            Tool::Node => project.config.node,
        };
        if let Some(request) = pin {
            return by_request(&request, Source::ProjectFile(project.file), warnings);
        }
    }

    // 3. version files, and engines for Node
    let version_files: &[&str] = match tool {
        Tool::Php => &[".php-version"],
        Tool::Node => &[".nvmrc", ".node-version"],
    };
    for name in version_files {
        let found = find_files_upward(name, ctx.cwd, ctx.ceiling, ctx.owners);
        warnings.extend(found.ignored.iter().map(ToString::to_string));
        for file in found.files {
            match read_version_file(&file) {
                Ok(request) => {
                    return by_request(&request, Source::VersionFile(file), warnings);
                }
                Err(why) => warnings.push(format!("ignoring {}: {why}", file.display())),
            }
        }
    }

    let defaults = Defaults::load(tool, ctx, &mut warnings)?;

    // 3 and 4. a constraint in composer.json / package.json
    let (manifest, flavor) = match tool {
        Tool::Php => ("composer.json", Flavor::Composer),
        Tool::Node => ("package.json", Flavor::Npm),
    };
    let found = find_files_upward(manifest, ctx.cwd, ctx.ceiling, ctx.owners);
    warnings.extend(found.ignored.iter().map(ToString::to_string));
    for file in found.files {
        match read_constraint(&file, tool) {
            Ok(Some(text)) => match Constraint::parse(&text, flavor) {
                Ok(constraint) => {
                    let source = match tool {
                        Tool::Php => Source::ComposerJson(file),
                        Tool::Node => Source::PackageJson(file),
                    };
                    return pick_constraint(
                        tool,
                        &text,
                        &constraint,
                        source,
                        &installed,
                        defaults.installed.as_ref(),
                        warnings,
                    );
                }
                Err(error) => warnings.push(format!("ignoring {}: {error}", file.display())),
            },
            Ok(None) => {}
            Err(why) => warnings.push(format!("ignoring {}: {why}", file.display())),
        }
    }

    // 5. the global default
    if let Some(request) = defaults.user {
        return by_request(&request, Source::GlobalDefault, warnings);
    }

    // 6. nothing asked for a version
    fallback(
        tool,
        &installed,
        defaults.recommended.zip(defaults.installed),
        ctx.catalog,
        warnings,
    )
}

/// The default version of a tool (ADR-0012): the user's `[defaults]` entry or, without one,
/// the line the catalog recommends.
struct Defaults {
    /// `[defaults]` in `crisol.toml`, as written.
    user: Option<String>,
    /// The catalog's recommended line.
    recommended: Option<String>,
    /// The installed version the default stands for, if it is installed.
    installed: Option<InstalledVersion>,
}

impl Defaults {
    fn load(
        tool: Tool,
        ctx: &ResolveContext<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Self, ResolveError> {
        let config = Config::load_from(ctx.paths)?;
        warnings.extend(config.warnings.iter().map(ToString::to_string));
        let user = match tool {
            Tool::Php => config.config.defaults.php,
            Tool::Node => config.config.defaults.node,
        };
        let recommended = ctx.catalog.recommended(tool.service()).map(str::to_owned);
        let installed = user.as_deref().or(recommended.as_deref()).and_then(|line| {
            resolve_installed(ctx.paths, tool.service(), line)
                .ok()
                .flatten()
        });
        Ok(Self {
            user,
            recommended,
            installed,
        })
    }
}

/// Step 6: the recommended line if it is installed, else the highest installed stable version.
/// A version without security fixes is only a last resort.
fn fallback(
    tool: Tool,
    installed: &[InstalledVersion],
    recommended: Option<(String, InstalledVersion)>,
    catalog: &Catalog,
    warnings: Vec<String>,
) -> Result<Resolution, ResolveError> {
    if let Some((line, best)) = recommended {
        return Ok(Resolution {
            resolved: Resolved {
                tool,
                request: line,
                version: best,
                source: Source::CatalogRecommendation,
            },
            warnings: dedup(warnings),
        });
    }
    let supported = |i: &&InstalledVersion| !catalog.is_end_of_life(tool.service(), &i.version);
    let best = installed
        .iter()
        .rfind(|i| !i.version.is_prerelease() && supported(i))
        .or_else(|| installed.iter().rfind(|i| !i.version.is_prerelease()))
        .or_else(|| installed.last())
        .cloned()
        .ok_or(ResolveError::NothingInstalled(tool.service()))?;
    Ok(Resolution {
        resolved: Resolved {
            tool,
            request: best.version.to_string(),
            version: best,
            source: Source::HighestInstalled,
        },
        warnings: dedup(warnings),
    })
}

/// The Composer to run: the highest installed version of the `composer` package (F4.3).
///
/// Composer is a single PHAR file that PHP runs, so unlike PHP or Node it is not chosen per
/// project: what varies from project to project is the PHP that runs it, and that is
/// [`resolve`] with [`Tool::Php`].
pub fn resolve_composer(paths: &CrisolPaths) -> Result<InstalledVersion, ResolveError> {
    list_installed(paths)?
        .into_iter()
        .rfind(|i| i.service == COMPOSER_SERVICE)
        .ok_or(ResolveError::NothingInstalled(COMPOSER_SERVICE))
}

/// The catalog service that holds `composer.phar`.
pub const COMPOSER_SERVICE: &str = "composer";

/// The file inside a Composer installation that PHP runs.
pub const COMPOSER_PHAR: &str = "composer.phar";

fn dedup(warnings: Vec<String>) -> Vec<String> {
    let mut seen = Vec::new();
    for warning in warnings {
        if !seen.contains(&warning) {
            seen.push(warning);
        }
    }
    seen
}

fn names(installed: &[InstalledVersion]) -> Vec<String> {
    installed.iter().map(|i| i.version.to_string()).collect()
}

fn pick_request(
    tool: Tool,
    request: &str,
    source: Source,
    installed: &[InstalledVersion],
    ctx: &ResolveContext<'_>,
    warnings: Vec<String>,
) -> Result<Resolution, ResolveError> {
    match resolve_installed(ctx.paths, tool.service(), request) {
        Ok(Some(version)) => Ok(Resolution {
            resolved: Resolved {
                tool,
                version,
                request: request.to_owned(),
                source,
            },
            warnings: dedup(warnings),
        }),
        // Not installed, or the request is not a version at all (a pin of `x`): the same
        // answer, with what is installed.
        Ok(None) | Err(ManageError::Version(_)) => Err(ResolveError::NotInstalled {
            tool: tool.service(),
            request: request.to_owned(),
            origin: source,
            installed: names(installed),
        }),
        Err(other) => Err(other.into()),
    }
}

fn pick_constraint(
    tool: Tool,
    text: &str,
    constraint: &Constraint,
    source: Source,
    installed: &[InstalledVersion],
    default: Option<&InstalledVersion>,
    warnings: Vec<String>,
) -> Result<Resolution, ResolveError> {
    // A project that accepts the default gets the default, not whatever is newest.
    let chosen = default
        .filter(|d| constraint.matches(&d.version))
        .or_else(|| installed.iter().rfind(|i| constraint.matches(&i.version)));
    match chosen {
        Some(version) => Ok(Resolution {
            resolved: Resolved {
                tool,
                version: version.clone(),
                request: text.to_owned(),
                source,
            },
            warnings: dedup(warnings),
        }),
        None => Err(ResolveError::NotInstalled {
            tool: tool.service(),
            request: text.to_owned(),
            origin: source,
            installed: names(installed),
        }),
    }
}

/// The version in a `.php-version`, `.nvmrc` or `.node-version`: the first line that is not
/// blank or a comment, without a `v` or `php-` prefix.
fn read_version_file(file: &Path) -> Result<String, String> {
    let text = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let line = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or("the file is empty")?;
    let version = line.strip_prefix("php-").unwrap_or(line);
    let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
    match VersionReq::parse(version) {
        Ok(_) => Ok(version.to_owned()),
        Err(_) => Err(format!(
            "`{line}` is not a version Crisol can use (aliases such as `lts/*` or `node` are not supported)"
        )),
    }
}

/// `require.php` of a `composer.json` or `engines.node` of a `package.json`, if the file
/// has it. A file without it is simply not about this tool.
fn read_constraint(file: &Path, tool: Tool) -> Result<Option<String>, String> {
    let text = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let value = match tool {
        Tool::Php => json.pointer("/require/php"),
        Tool::Node => json.pointer("/engines/node"),
    };
    match value {
        None => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err("the constraint is not a string".to_owned()),
    }
}

#[cfg(test)]
mod tests;
