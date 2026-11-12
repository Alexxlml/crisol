//! The sites and the folders they come from, as a window draws them (F13.4).
//!
//! The daemon says which sites there are (`sites`); `crisol.toml` says which folders are
//! parked and on which ports Nginx listens. This puts both together, decides which buttons each
//! row offers and where "open in the browser" goes, and turns a click into a call.

use std::path::Path;

use crisol_core::config::Config;
use crisol_core::i18n::Localized;
use crisol_core::msg;
use crisol_core::CrisolPaths;
use crisol_protocol::{Call, ErrorCode, Reply, RpcError, SiteInfo};
use serde::{Deserialize, Serialize};

use crate::connect;
use crate::context::Context;
use crate::summary::{RowAction, Toggle};

/// A button of a site's row that changes something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteAction {
    /// `secure <site>`: also served over HTTPS.
    Secure,
    /// `unsecure <site>`.
    Unsecure,
    /// `unlink <site>`: only a linked site can be (a parked folder's sites go with the folder).
    Unlink,
}

impl SiteAction {
    /// The call the button makes for site `name`.
    #[must_use]
    pub fn call(self, name: &str) -> Call {
        let name = name.to_owned();
        match self {
            Self::Secure => Call::Secure { name },
            Self::Unsecure => Call::Unsecure { name },
            Self::Unlink => Call::Unlink { name },
        }
    }
}

/// Which buttons a site offers: the opposite of its HTTPS, and unlink for the ones that were
/// linked by hand. Opening it and the terminal are always there, so they are not listed.
#[must_use]
pub fn site_actions(kind: &str, secure: bool) -> Vec<SiteAction> {
    let mut actions = vec![if secure {
        SiteAction::Unsecure
    } else {
        SiteAction::Secure
    }];
    if kind == "linked" {
        actions.push(SiteAction::Unlink);
    }
    actions
}

/// The address of a site in the browser: `https` when it is secured, and the port only when
/// it is not the usual one (a test home runs Nginx on another).
#[must_use]
pub fn site_url(domain: &str, secure: bool, config: &Config) -> String {
    let nginx = &config.services.nginx;
    let (scheme, port, usual) = if secure {
        ("https", nginx.https_port, 443)
    } else {
        ("http", nginx.http_port, 80)
    };
    if port == usual {
        format!("{scheme}://{domain}/")
    } else {
        format!("{scheme}://{domain}:{port}/")
    }
}

/// One site as a list draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteRow {
    /// `shop`.
    pub name: String,
    /// `shop.test`.
    pub domain: String,
    /// The project folder.
    pub root: String,
    /// The folder served.
    pub docroot: String,
    /// `parked` or `linked`.
    pub kind: String,
    /// Also served over HTTPS.
    pub secure: bool,
    /// Where "open in the browser" goes.
    pub url: String,
    /// The buttons that change something.
    pub actions: Vec<SiteAction>,
}

/// A parked folder: every subfolder of it is a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkedRow {
    /// The folder, absolute.
    pub path: String,
    /// What `unpark` is given: the entry as `crisol.toml` writes it.
    pub entry: String,
    /// Whether the folder is there; a parked folder that was deleted serves nothing.
    pub exists: bool,
    /// Whether "unpark" is offered. The folder that is parked only because nothing else is
    /// (the default `www`) has nothing to remove.
    pub removable: bool,
}

/// What the sites screen draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SitesView {
    /// Every site, as the daemon lists them.
    pub sites: Vec<SiteRow>,
    /// The parked folders.
    pub parked: Vec<ParkedRow>,
    /// Folders and entries the daemon left out, with why, as it said it.
    pub warnings: Vec<String>,
    /// The suffix of the domains (`test`), for the form that links a folder.
    pub tld: String,
}

/// The parked folders of `config`.
#[must_use]
pub fn parked_rows(paths: &CrisolPaths, config: &Config) -> Vec<ParkedRow> {
    let defaults = Config::default().sites.parked;
    let only_the_default = config.sites.parked == defaults;
    config
        .sites
        .parked
        .iter()
        .zip(config.parked_dirs(paths))
        .map(|(entry, path)| ParkedRow {
            exists: path.is_dir(),
            path: path.display().to_string(),
            entry: entry.display().to_string(),
            removable: !only_the_default,
        })
        .collect()
}

/// Puts the answer of `sites` together with the configuration.
#[must_use]
pub fn sites_view(
    sites: Vec<SiteInfo>,
    warnings: Vec<String>,
    paths: &CrisolPaths,
    config: &Config,
) -> SitesView {
    SitesView {
        sites: sites
            .into_iter()
            .map(|site| SiteRow {
                url: site_url(&site.domain, site.secure, config),
                actions: site_actions(&site.kind, site.secure),
                name: site.name,
                domain: site.domain,
                root: site.root,
                docroot: site.docroot,
                kind: site.kind,
                secure: site.secure,
            })
            .collect(),
        parked: parked_rows(paths, config),
        warnings,
        tld: config.sites.tld.clone(),
    }
}

fn load_config(context: &Context) -> Result<Config, RpcError> {
    Config::load_from(&context.paths)
        .map(|loaded| loaded.config)
        .map_err(|e| {
            RpcError::new(
                ErrorCode::InvalidParams,
                e.message().render(context.language),
            )
        })
}

fn fetch(context: &Context) -> Result<(Vec<SiteInfo>, Vec<String>), RpcError> {
    match connect::call(context, &Call::Sites, &mut |_| {})? {
        Reply::Sites { sites, warnings } => Ok((sites, warnings)),
        other => Err(RpcError::new(
            ErrorCode::Internal,
            msg!(
                "client.unexpected_answer",
                call = "sites",
                answer = format!("{other:?}")
            )
            .render(context.language),
        )),
    }
}

/// Asks for the sites (the daemon answers, or this process when there is none: listing starts
/// nothing) and puts them with the configuration.
pub fn fetch_sites(context: &Context) -> Result<SitesView, RpcError> {
    let config = load_config(context)?;
    let (sites, warnings) = fetch(context)?;
    Ok(sites_view(sites, warnings, &context.paths, &config))
}

/// Opens site `name` in the browser. The address is made here from the sites as they are now:
/// it never comes from the page.
pub fn open_site(context: &Context, name: &str) -> Result<(), RpcError> {
    let config = load_config(context)?;
    let (sites, _) = fetch(context)?;
    let site = sites.iter().find(|site| site.name == name).ok_or_else(|| {
        RpcError::new(
            ErrorCode::NotFound,
            msg!("terminal.no_site", name = name, command = "crisol sites")
                .render(context.language),
        )
    })?;
    let url = site_url(&site.domain, site.secure, &config);
    context.desktop.open_url(&url).map_err(|e| {
        RpcError::new(
            ErrorCode::Io,
            msg!("sites.cannot_open", url = url, error = e).render(context.language),
        )
    })
}

/// A site name that the core accepts, from the name of a folder: lowercase letters, digits and
/// inner hyphens. What does not fit becomes a hyphen. It is a suggestion for the form; the core
/// has the last word. Empty when nothing is left (`___`).
#[must_use]
pub fn suggested_site_name(folder: &Path) -> String {
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(63);
    out.trim_end_matches('-').to_owned()
}

/// A click of the services or sites screens, as the page sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Act {
    /// A button of a service's row.
    Service {
        /// Which button.
        action: RowAction,
        /// Which service.
        name: String,
    },
    /// "Start all" or "Stop all".
    All {
        /// Which of the two.
        toggle: Toggle,
    },
    /// A button of a site's row.
    Site {
        /// Which button.
        action: SiteAction,
        /// Which site.
        name: String,
    },
    /// Serve one folder under a name.
    Link {
        /// The site name.
        name: String,
        /// The folder, absolute.
        path: String,
    },
    /// Serve every subfolder of this folder.
    Park {
        /// The folder, absolute.
        path: String,
    },
    /// Stop serving the subfolders of a parked folder.
    Unpark {
        /// The entry, as `crisol.toml` writes it ([`ParkedRow::entry`]).
        entry: String,
    },
    /// A new password for a data service (the credentials screen).
    ResetPassword {
        /// The service.
        service: String,
    },
}

impl Act {
    /// The call this click makes: the same one the command line makes.
    #[must_use]
    pub fn call(&self) -> Call {
        match self {
            Self::Service { action, name } => action.call(name),
            Self::All { toggle } => toggle.call(),
            Self::Site { action, name } => action.call(name),
            Self::Link { name, path } => Call::Link {
                name: name.clone(),
                path: path.clone(),
                docroot: None,
            },
            Self::Park { path } => Call::Park { path: path.clone() },
            Self::Unpark { entry } => Call::Unpark {
                path: entry.clone(),
            },
            Self::ResetPassword { service } => Call::ResetPassword {
                service: service.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
