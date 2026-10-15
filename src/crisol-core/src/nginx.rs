//! Nginx configuration generated from the sites (F5.2).
//!
//! Everything under `etc/nginx/` is derived and rewritten whenever it is generated (ADR-0003):
//!
//! ```text
//! etc/nginx/
//! ├── nginx.conf          # the main file: events, http, the default server, the includes
//! ├── php_upstream.conf   # one `upstream` per PHP version in use, one server per worker
//! ├── sites/<name>.conf   # one `server` block per site
//! └── overrides/          # the user's: `*.conf` go inside `http`, `sites/<name>/*.conf`
//!                         #   go inside that site's `server` (never touched here)
//! ```
//!
//! Nothing is parsed or patched (patching `nginx.conf` with regular expressions is what lets
//! edits and generation fight over the port): the files are rendered from the
//! sites, the ports and the installation directory, so the same input is always the same text
//! and a file that already has that text is not rewritten.
//!
//! Paths in the files are absolute with forward slashes and always between quotes. A path that
//! could change meaning inside quotes (`"` or `$`, which Nginx would expand as a variable, or a
//! control character) keeps its site out, with a warning, instead of producing a file Nginx
//! would misread.
//!
//! The server listens on `127.0.0.1` only; HTTPS is added with the certificates (Phase 6). A
//! `default_server` answers 444 (closes the connection) for any host that is not a site. The
//! PHP workers are the pool of F5.3: [`PhpPools`] says which ports each version listens on.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::{CrisolPaths, PathsError};
use crate::phpini::{write_if_changed, PhpIniError};
use crate::sites::{DocRoot, Site};

/// The service name under `bin/`, `etc/` and `logs/`.
pub const SERVICE: &str = "nginx";

/// The ports of the `php-cgi` workers of each PHP version in use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PhpPools {
    ports: BTreeMap<String, Vec<u16>>,
}

impl PhpPools {
    /// Gives each version, in order, `workers` consecutive ports starting at `base_port`.
    ///
    /// The version list is sorted and de-duplicated first, so the layout depends only on which
    /// versions are in use. Ports past 65535 are not handed out: a version that does not fit
    /// gets no pool.
    #[must_use]
    pub fn allocate<I, S>(versions: I, base_port: u16, workers: u16) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let versions: BTreeSet<String> = versions.into_iter().map(Into::into).collect();
        let workers = u32::from(workers.max(1));
        let mut next = u32::from(base_port);
        let mut ports = BTreeMap::new();
        for version in versions {
            let Some(last) = next.checked_add(workers - 1).filter(|l| *l <= 65535) else {
                break;
            };
            ports.insert(
                version,
                (next..=last)
                    .filter_map(|p| u16::try_from(p).ok())
                    .collect(),
            );
            next = last + 1;
        }
        Self { ports }
    }

    /// The pool of a version: a pool laid out by hand (for tests and for F5.3).
    #[must_use]
    pub fn with(mut self, version: &str, ports: &[u16]) -> Self {
        self.ports.insert(version.to_owned(), ports.to_vec());
        self
    }

    /// The ports of `version`, if it has a pool.
    #[must_use]
    pub fn ports(&self, version: &str) -> Option<&[u16]> {
        self.ports.get(version).map(Vec::as_slice)
    }

    /// The name of the `upstream` of a version (`php_8_4_26`).
    #[must_use]
    pub fn upstream(version: &str) -> String {
        let clean: String = version
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("php_{clean}")
    }
}

/// A site and the PHP version it runs with (`None`: none is installed, so it is static only).
#[derive(Debug, Clone, Copy)]
pub struct NginxSite<'a> {
    /// The site.
    pub site: &'a Site,
    /// The exact PHP version that serves it, as resolved for its folder.
    pub php: Option<&'a str>,
}

/// What the configuration is generated from.
#[derive(Debug, Clone, Copy)]
pub struct NginxInput<'a> {
    /// Where Crisol lives.
    pub paths: &'a CrisolPaths,
    /// The Nginx installation (`bin/nginx/<version>`): its `mime.types` and `fastcgi_params`.
    pub nginx_dir: &'a Path,
    /// The HTTP port to listen on.
    pub http_port: u16,
    /// The sites to serve.
    pub sites: &'a [NginxSite<'a>],
    /// The PHP pools.
    pub pools: &'a PhpPools,
}

/// The generated files, by path relative to `etc/nginx/`, and what was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedNginx {
    /// File name (with `/`) to text.
    pub files: BTreeMap<String, String>,
    /// Sites left out and other things worth saying.
    pub warnings: Vec<String>,
}

/// Why the configuration could not be written.
#[derive(Debug, thiserror::Error)]
pub enum NginxError {
    /// A path is not allowed.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// A file could not be written.
    #[error(transparent)]
    Write(#[from] PhpIniError),
    /// A folder could not be read, created or cleaned.
    #[error("{action} `{}`: {source}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The path.
        path: PathBuf,
        /// The error.
        source: io::Error,
    },
}

/// What [`write_nginx_config`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenNginx {
    /// `etc/nginx/nginx.conf`: what `nginx -c` is given.
    pub main: PathBuf,
    /// Files created or whose text changed.
    pub changed: Vec<PathBuf>,
    /// Site files of sites that no longer exist, deleted.
    pub removed: Vec<PathBuf>,
    /// Things worth telling the user.
    pub warnings: Vec<String>,
}

/// A path as it goes between the quotes of an Nginx value.
fn quote(path: &Path) -> Result<String, String> {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(bad) = text
        .chars()
        .find(|c| matches!(c, '"' | '$') || c.is_control())
    {
        return Err(format!(
            "`{text}` has a character Nginx would read differently inside quotes ({bad:?})"
        ));
    }
    Ok(format!("\"{text}\""))
}

/// The text of every file, without touching the disk.
#[must_use]
pub fn render(input: &NginxInput<'_>) -> RenderedNginx {
    let mut warnings = Vec::new();
    let mut files = BTreeMap::new();
    let mut used_pools: BTreeSet<&str> = BTreeSet::new();

    let mut names = BTreeSet::new();
    for entry in input.sites {
        if !names.insert(entry.site.name.as_str()) {
            warnings.push(format!(
                "the site `{}` appears twice; the first is used",
                entry.site.name
            ));
            continue;
        }
        match render_site(input, entry) {
            Ok(text) => {
                files.insert(format!("sites/{}.conf", entry.site.name), text);
                if let Some(version) = entry.php.filter(|v| input.pools.ports(v).is_some()) {
                    used_pools.insert(version);
                }
            }
            Err(why) => warnings.push(format!(
                "the site `{}` is not served: {why}",
                entry.site.name
            )),
        }
    }

    match (
        render_upstreams(input.pools, &used_pools),
        render_main(input),
    ) {
        (upstreams, Ok(main)) => {
            files.insert("php_upstream.conf".to_owned(), upstreams);
            files.insert("nginx.conf".to_owned(), main);
        }
        (_, Err(why)) => warnings.push(format!("nginx.conf could not be generated: {why}")),
    }
    RenderedNginx { files, warnings }
}

fn render_upstreams(pools: &PhpPools, used: &BTreeSet<&str>) -> String {
    let mut text = String::from(HEADER);
    text.push_str("# One upstream per PHP version in use; one server per php-cgi worker.\n");
    for version in used {
        let _ = writeln!(text, "\nupstream {} {{", PhpPools::upstream(version));
        for port in pools.ports(version).unwrap_or_default() {
            let _ = writeln!(text, "    server 127.0.0.1:{port};");
        }
        text.push_str("}\n");
    }
    text
}

const HEADER: &str = "# Generated by Crisol. Do not edit: it is rewritten whenever the sites change.\n# Your own settings go in the overrides folder (see nginx.conf).\n";

fn render_main(input: &NginxInput<'_>) -> Result<String, String> {
    let paths = input.paths;
    let etc = paths.etc_for(SERVICE).map_err(|e| e.to_string())?;
    let overrides = paths.overrides_for(SERVICE).map_err(|e| e.to_string())?;
    let logs = paths.logs_for(SERVICE).map_err(|e| e.to_string())?;
    let temp = paths.state_dir().join(SERVICE);
    let port = input.http_port;

    let mut text = String::from(HEADER);
    let _ = write!(
        text,
        "# User settings: {overrides_note}/*.conf (inside http) and\n\
         # {overrides_note}/sites/<site>/*.conf (inside that site's server).\n\n\
         worker_processes 1;\n\
         error_log {error_log} warn;\n\
         pid {pid};\n\n\
         events {{\n    worker_connections 1024;\n}}\n\n\
         http {{\n\
         \x20   include {mime};\n\
         \x20   default_type application/octet-stream;\n\
         \x20   access_log {access_log};\n\
         \x20   sendfile on;\n\
         \x20   keepalive_timeout 65;\n\
         \x20   client_max_body_size 128m;\n\
         \x20   server_names_hash_bucket_size 64;\n\
         \x20   client_body_temp_path {client_body};\n\
         \x20   proxy_temp_path {proxy};\n\
         \x20   fastcgi_temp_path {fastcgi};\n\
         \x20   uwsgi_temp_path {uwsgi};\n\
         \x20   scgi_temp_path {scgi};\n\n\
         \x20   include {upstream};\n\n\
         \x20   # Any host that is not a site: close the connection.\n\
         \x20   server {{\n\
         \x20       listen 127.0.0.1:{port} default_server;\n\
         \x20       server_name _;\n\
         \x20       return 444;\n\
         \x20   }}\n\n\
         \x20   include {sites};\n\
         \x20   include {user};\n\
         }}\n",
        overrides_note = overrides.to_string_lossy().replace('\\', "/"),
        error_log = quote(&logs.join("error.log"))?,
        pid = quote(&temp.join("nginx.pid"))?,
        mime = quote(&input.nginx_dir.join("conf").join("mime.types"))?,
        access_log = quote(&logs.join("access.log"))?,
        client_body = quote(&temp.join("client_body_temp"))?,
        proxy = quote(&temp.join("proxy_temp"))?,
        fastcgi = quote(&temp.join("fastcgi_temp"))?,
        uwsgi = quote(&temp.join("uwsgi_temp"))?,
        scgi = quote(&temp.join("scgi_temp"))?,
        upstream = quote(&etc.join("php_upstream.conf"))?,
        sites = quote(&etc.join("sites").join("*.conf"))?,
        user = quote(&overrides.join("*.conf"))?,
    );
    Ok(text)
}

fn rule_label(rule: DocRoot) -> &'static str {
    match rule {
        DocRoot::Explicit => "document root set in sites.toml",
        DocRoot::Laravel => "Laravel",
        DocRoot::Symfony => "Symfony",
        DocRoot::WordPress => "WordPress",
        DocRoot::PublicFolder => "public folder",
        DocRoot::WebFolder => "web folder",
        DocRoot::Root => "plain folder",
    }
}

fn render_site(input: &NginxInput<'_>, entry: &NginxSite<'_>) -> Result<String, String> {
    let site = entry.site;
    let paths = input.paths;
    let overrides = paths.overrides_for(SERVICE).map_err(|e| e.to_string())?;
    let logs = paths.logs_for(SERVICE).map_err(|e| e.to_string())?;
    let port = input.http_port;
    let root = quote(&site.docroot)?;
    let error_log = quote(&logs.join(format!("{}.error.log", site.name)))?;
    let user = quote(&overrides.join("sites").join(&site.name).join("*.conf"))?;

    let php = match entry.php {
        Some(version) if input.pools.ports(version).is_some() => {
            let fastcgi_params = quote(&input.nginx_dir.join("conf").join("fastcgi_params"))?;
            format!(
                "    location ~ \\.php$ {{\n\
                 \x20       try_files $uri =404;\n\
                 \x20       include {fastcgi_params};\n\
                 \x20       fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;\n\
                 \x20       fastcgi_pass {};\n\
                 \x20       fastcgi_read_timeout 300;\n\
                 \x20   }}\n",
                PhpPools::upstream(version)
            )
        }
        Some(version) => format!(
            "    # PHP {version} has no workers running.\n\
             \x20   location ~ \\.php$ {{\n        return 503;\n    }}\n"
        ),
        None => "    # No PHP version is installed for this site.\n    \
                 location ~ \\.php$ {\n        return 503;\n    }\n"
            .to_owned(),
    };

    Ok(format!(
        "{HEADER}\n\
         # {domain}: {kind} site, {rule}\n\
         server {{\n\
         \x20   listen 127.0.0.1:{port};\n\
         \x20   server_name {domain};\n\
         \x20   root {root};\n\
         \x20   index index.php index.html;\n\
         \x20   charset utf-8;\n\
         \x20   error_log {error_log} warn;\n\n\
         \x20   location / {{\n\
         \x20       try_files $uri $uri/ /index.php?$query_string;\n\
         \x20   }}\n\n\
         \x20   location = /favicon.ico {{\n        access_log off;\n        log_not_found off;\n    }}\n\n\
         \x20   # Hidden files (.env, .git…) are never served.\n\
         \x20   location ~ /\\.(?!well-known) {{\n        deny all;\n    }}\n\n\
         {php}\n\
         \x20   include {user};\n\
         }}\n",
        domain = site.domain,
        kind = match site.kind {
            crate::sites::SiteKind::Parked => "parked",
            crate::sites::SiteKind::Linked => "linked",
        },
        rule = rule_label(site.docroot_rule),
    ))
}

/// Writes the files of [`render`] under `etc/nginx/`, replacing only what changed, and deletes
/// the `sites/*.conf` of sites that are gone. The folders for logs and temporary files are
/// created, because Nginx does not create their parents.
pub fn write_nginx_config(input: &NginxInput<'_>) -> Result<WrittenNginx, NginxError> {
    let rendered = render(input);
    let etc = input.paths.etc_for(SERVICE)?;
    let io = |action: &'static str, path: &Path| {
        let path = path.to_path_buf();
        move |source| NginxError::Io {
            action,
            path,
            source,
        }
    };

    for dir in [
        input.paths.logs_for(SERVICE)?,
        input.paths.state_dir().join(SERVICE),
        etc.join("sites"),
    ] {
        fs::create_dir_all(&dir).map_err(io("cannot create", &dir))?;
    }

    let mut changed = Vec::new();
    for (name, text) in &rendered.files {
        let path = etc.join(name);
        if write_if_changed(&path, text.as_bytes())? {
            changed.push(path);
        }
    }

    let mut removed = Vec::new();
    let sites_dir = etc.join("sites");
    for entry in fs::read_dir(&sites_dir)
        .map_err(io("cannot read", &sites_dir))?
        .flatten()
    {
        let file = entry.file_name().to_string_lossy().into_owned();
        let wanted = rendered.files.contains_key(&format!("sites/{file}"));
        if Path::new(&file)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("conf"))
            && !wanted
        {
            let path = entry.path();
            fs::remove_file(&path).map_err(io("cannot delete", &path))?;
            removed.push(path);
        }
    }
    removed.sort();

    Ok(WrittenNginx {
        main: etc.join("nginx.conf"),
        changed,
        removed,
        warnings: rendered.warnings,
    })
}

#[cfg(test)]
mod tests;
