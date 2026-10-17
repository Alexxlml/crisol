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
//! The server listens on `127.0.0.1` and `[::1]` only, on the same ports: the DNS responder and
//! the `hosts` block answer both addresses (ADR-0014), and clients try IPv6 first, so a program
//! listening on `[::1]:80` would otherwise receive the requests meant for a site. A site marked
//! `secure` (ADR-0013) also gets an HTTPS server block with the certificate in [`NginxTls`], and its HTTP block only redirects
//! to it; a secured site without a certificate is served over HTTP, with a warning. A
//! `default_server` answers 444 (closes the connection) for any host that is not a site. The
//! PHP workers are the pool of F5.3: [`PhpPools`] says which ports each version listens on.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::paths::{require_private_home, CrisolPaths, HomeError, PathsError, PrivateDirs};
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

    /// Gives `version` a block of `workers` ports if it has none, in the first place from
    /// `base_port` on that overlaps no other pool and none of the `reserved` ports (Nginx's,
    /// the databases'…), and returns whether it was added. Pools
    /// that already exist keep their ports, so adding a version never moves a running one.
    /// A version that does not fit below 65535 gets none.
    pub fn ensure(
        &mut self,
        version: &str,
        base_port: u16,
        workers: u16,
        reserved: &BTreeSet<u16>,
    ) -> bool {
        if self.ports.contains_key(version) {
            return false;
        }
        let workers = u32::from(workers.max(1));
        let mut start = u32::from(base_port);
        loop {
            let end = start + workers - 1;
            if end > 65535 {
                return false;
            }
            let inside = |p: &u16| (start..=end).contains(&u32::from(*p));
            let clash = self.ports.values().flatten().any(inside) || reserved.iter().any(inside);
            if !clash {
                let block = (start..=end)
                    .filter_map(|p| u16::try_from(p).ok())
                    .collect();
                self.ports.insert(version.to_owned(), block);
                return true;
            }
            start += workers;
        }
    }

    /// Forgets every version not in `keep`, freeing its ports.
    pub fn retain(&mut self, keep: &BTreeSet<String>) {
        self.ports.retain(|version, _| keep.contains(version));
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

/// The certificate that secured sites use, and the port they listen on.
#[derive(Debug, Clone, Copy)]
pub struct NginxTls<'a> {
    /// `certs/sites.pem`.
    pub cert: &'a Path,
    /// `certs/sites.key`.
    pub key: &'a Path,
    /// The HTTPS port.
    pub https_port: u16,
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
    /// The certificate for the secured sites; `None` serves everything over HTTP.
    pub tls: Option<NginxTls<'a>>,
    /// Mailpit's page and API as a name of their own, when Mailpit is enabled.
    pub mail: Option<&'a MailRoute>,
}

/// Mailpit behind Nginx (review of phase 9, finding 2): `mail.<tld>` is served by Nginx, which
/// forwards to the port Mailpit listens on. Nginx answers only the names it knows, and Mailpit
/// itself is told to answer only to this one (`--allowed-hosts`), so a page that makes the
/// browser go to `127.0.0.1:8025` under another name gets nothing (DNS rebinding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailRoute {
    /// `mail.test`.
    pub domain: String,
    /// The port of Mailpit's web server.
    pub port: u16,
}

/// The generated files, by path relative to `etc/nginx/`, and what was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedNginx {
    /// File name (with `/`) to text.
    pub files: BTreeMap<String, String>,
    /// Sites left out and other things worth saying.
    pub warnings: Vec<String>,
    /// Identifies this configuration: it is part of the text of the default server as
    /// `/.crisol-config-<token>` (a path only the computer itself may ask for), so that asking
    /// for it tells whether a running Nginx has already loaded this configuration.
    pub token: String,
}

/// The path that answers `204` to this computer once Nginx runs the configuration with `token`.
#[must_use]
pub fn probe_path(token: &str) -> String {
    format!("/.crisol-config-{token}")
}

/// Why the configuration could not be written.
#[derive(Debug, thiserror::Error)]
pub enum NginxError {
    /// A path is not allowed.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// The home is open to other accounts: no configuration is generated from it.
    #[error(transparent)]
    Home(#[from] HomeError),
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
    match input.paths.etc_for(SERVICE) {
        Ok(etc) => {
            // The token is the hash of everything else, so it changes with every change of the
            // configuration and with nothing else.
            let bare = render_in(input, &etc, "");
            let token = token_of(&bare.files);
            render_in(input, &etc, &token)
        }
        Err(e) => RenderedNginx {
            files: BTreeMap::new(),
            warnings: vec![e.to_string()],
            token: String::new(),
        },
    }
}

fn token_of(files: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (name, text) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(text.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// [`render`] for files that will live under `etc` (the real folder, or a staging copy of it).
fn render_in(input: &NginxInput<'_>, etc: &Path, token: &str) -> RenderedNginx {
    let mut warnings = Vec::new();
    let mut files = BTreeMap::new();
    let mut used_pools: BTreeSet<&str> = BTreeSet::new();
    let mut any_secure = false;

    let mut names = BTreeSet::new();
    for entry in input.sites {
        if !names.insert(entry.site.name.as_str()) {
            warnings.push(format!(
                "the site `{}` appears twice; the first is used",
                entry.site.name
            ));
            continue;
        }
        let secure = entry.site.secure && input.tls.is_some();
        if entry.site.secure && input.tls.is_none() {
            warnings.push(format!(
                "{} is secured but there is no certificate for it: it is served over HTTP",
                entry.site.domain
            ));
        }
        any_secure |= secure;
        match render_site(input, entry, secure) {
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

    if let Some(mail) = input.mail {
        match render_mail(input, mail) {
            Ok(text) => {
                files.insert("sites/_mail.conf".to_owned(), text);
            }
            Err(why) => warnings.push(format!("{} is not served: {why}", mail.domain)),
        }
    }

    match (
        render_upstreams(input.pools, &used_pools),
        render_main(input, etc, any_secure, token),
    ) {
        (upstreams, Ok(main)) => {
            files.insert("php_upstream.conf".to_owned(), upstreams);
            files.insert("nginx.conf".to_owned(), main);
        }
        (_, Err(why)) => warnings.push(format!("nginx.conf could not be generated: {why}")),
    }
    RenderedNginx {
        files,
        warnings,
        token: token.to_owned(),
    }
}

/// The server block of `mail.<tld>`: plain HTTP, forwarding everything (the live updates of the
/// page are a WebSocket) to Mailpit with the name the browser used.
fn render_mail(input: &NginxInput<'_>, mail: &MailRoute) -> Result<String, String> {
    if !mail
        .domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err(format!("`{}` is not a plain host name", mail.domain));
    }
    Ok(format!(
        "{HEADER}
         # {domain}: Mailpit, the page and the API.
         server {{
         {listen}             server_name {domain};

             location / {{
                 proxy_pass http://127.0.0.1:{port};
                 proxy_http_version 1.1;
                 proxy_set_header Host $host;
                 proxy_set_header Upgrade $http_upgrade;
                 proxy_set_header Connection \"upgrade\";
                 proxy_read_timeout 1h;
             }}
         }}
",
        domain = mail.domain,
        port = mail.port,
        listen = listen_loopbacks("    ", input.http_port, ""),
    ))
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

fn render_main(
    input: &NginxInput<'_>,
    etc: &Path,
    any_secure: bool,
    token: &str,
) -> Result<String, String> {
    let paths = input.paths;
    let overrides = paths.overrides_for(SERVICE).map_err(|e| e.to_string())?;
    let logs = paths.logs_for(SERVICE).map_err(|e| e.to_string())?;
    let temp = paths.state_dir().join(SERVICE);
    let port = input.http_port;
    let tls_default = match input.tls {
        Some(tls) if any_secure => format!(
            "    # Any host that is not a site, over HTTPS: refuse the handshake.\n\
             \x20   server {{\n\
             \x20       listen 127.0.0.1:{port} ssl default_server;\n\
             \x20       listen [::1]:{port} ssl default_server;\n\
             \x20       server_name _;\n\
             \x20       ssl_reject_handshake on;\n\
             \x20   }}\n\n",
            port = tls.https_port
        ),
        _ => String::new(),
    };

    // Only this computer may ask: whoever else gets the same closed connection as for any
    // unknown name.
    let probe = if token.is_empty() {
        String::new()
    } else {
        format!(
            "        # Whether Nginx runs this configuration yet (the name changes with it).\n\
             \x20       location = {} {{\n\
             \x20           allow 127.0.0.1;\n\
             \x20           allow ::1;\n\
             \x20           deny all;\n\
             \x20           return 204;\n\
             \x20       }}\n",
            probe_path(token)
        )
    };

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
         \x20   server_names_hash_bucket_size 128;\n\
         \x20   client_body_temp_path {client_body};\n\
         \x20   proxy_temp_path {proxy};\n\
         \x20   fastcgi_temp_path {fastcgi};\n\
         \x20   uwsgi_temp_path {uwsgi};\n\
         \x20   scgi_temp_path {scgi};\n\n\
         \x20   include {upstream};\n\n\
         \x20   # Any host that is not a site: close the connection.\n\
         \x20   server {{\n\
         \x20       listen 127.0.0.1:{port} default_server;\n\
         \x20       listen [::1]:{port} default_server;\n\
         \x20       server_name _;\n\
         {probe}\
         \x20       location / {{\n\
         \x20           return 444;\n\
         \x20       }}\n\
         \x20   }}\n\n\
         {tls_default}\
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

const PHP_NOTE: &str =
    "    # Windows ignores the case of a name and a trailing dot or space, so `.php` is\n\
     \x20   # matched without regard to case and with those endings: `/wp-config.PHP` and\n\
     \x20   # `/wp-config.php.` run the script, they are never sent as text.\n";

/// The `listen` lines for `port` on both loopback addresses (`127.0.0.1` and `[::1]`), with
/// `options` (such as ` ssl default_server`) after the address.
fn listen_loopbacks(indent: &str, port: u16, options: &str) -> String {
    format!("{indent}listen 127.0.0.1:{port}{options};\n{indent}listen [::1]:{port}{options};\n")
}

fn render_site(
    input: &NginxInput<'_>,
    entry: &NginxSite<'_>,
    secure: bool,
) -> Result<String, String> {
    let site = entry.site;
    let paths = input.paths;
    let overrides = paths.overrides_for(SERVICE).map_err(|e| e.to_string())?;
    let logs = paths.logs_for(SERVICE).map_err(|e| e.to_string())?;
    let port = input.http_port;
    let root = quote(&site.docroot)?;
    let error_log = quote(&logs.join(format!("{}.error.log", site.name)))?;
    let user = quote(&overrides.join("sites").join(&site.name).join("*.conf"))?;
    let https_param = if secure {
        "        fastcgi_param HTTPS on;\n"
    } else {
        ""
    };

    let php = match entry.php {
        Some(version) if input.pools.ports(version).is_some() => {
            let fastcgi_params = quote(&input.nginx_dir.join("conf").join("fastcgi_params"))?;
            format!(
                "    location ~* \"\\.php[. ]*$\" {{\n\
                 \x20       try_files $uri =404;\n\
                 \x20       include {fastcgi_params};\n\
                 \x20       fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;\n\
                 {https_param}\
                 \x20       fastcgi_pass {};\n\
                 \x20       fastcgi_read_timeout 300;\n\
                 \x20   }}\n",
                PhpPools::upstream(version)
            )
        }
        Some(version) => format!(
            "    # PHP {version} has no workers running.\n\
             \x20   location ~* \"\\.php[. ]*$\" {{\n        return 503;\n    }}\n"
        ),
        None => "    # No PHP version is installed for this site.\n    \
                 location ~* \"\\.php[. ]*$\" {\n        return 503;\n    }\n"
            .to_owned(),
    };

    // Windows ignores the case of a name and a trailing dot or space, so `.php` is matched
    // without regard to case and with those endings: `/wp-config.PHP` and `/wp-config.php.` run
    // the script, they are never sent as text.
    let php = format!("{PHP_NOTE}{php}");

    // A secured site listens with TLS, and its plain HTTP block only redirects to it.
    let (listen, redirect) = match input.tls {
        Some(tls) if secure => {
            let suffix = if tls.https_port == 443 {
                String::new()
            } else {
                format!(":{}", tls.https_port)
            };
            (
                format!(
                    "{listens}\
                     \x20   http2 on;\n\
                     \x20   ssl_certificate {};\n\
                     \x20   ssl_certificate_key {};\n\
                     \x20   ssl_protocols TLSv1.2 TLSv1.3;\n",
                    quote(tls.cert)?,
                    quote(tls.key)?,
                    listens = listen_loopbacks("    ", tls.https_port, " ssl"),
                ),
                format!(
                    "# {domain}: plain HTTP goes to HTTPS. A 302, not a 301: browsers keep a 301 for\n\
                     # good, and the site would look broken after being unsecured.\n\
                     server {{\n\
                     {listens}\
                     \x20   server_name {domain};\n\
                     \x20   return 302 https://$host{suffix}$request_uri;\n\
                     }}\n\n",
                    domain = site.domain,
                    listens = listen_loopbacks("    ", port, ""),
                ),
            )
        }
        _ => (listen_loopbacks("    ", port, ""), String::new()),
    };

    Ok(format!(
        "{HEADER}\n\
         {redirect}\
         # {domain}: {kind} site, {rule}\n\
         server {{\n\
         {listen}\
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
pub fn write_nginx_config(
    input: &NginxInput<'_>,
    dirs: &dyn PrivateDirs,
) -> Result<WrittenNginx, NginxError> {
    require_private_home(input.paths, dirs)?;
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

/// The folder where a configuration is prepared and tested before it replaces the real one.
#[must_use]
pub fn staging_dir(paths: &CrisolPaths) -> Option<PathBuf> {
    Some(paths.etc_for(SERVICE).ok()?.join(STAGING))
}

const STAGING: &str = ".next";

/// Whether writing this configuration would change anything in `etc/nginx/`: a file with
/// other text or no file at all, or a `sites/*.conf` left from a site that is gone.
pub fn config_differs(input: &NginxInput<'_>) -> Result<bool, NginxError> {
    let rendered = render(input);
    let etc = input.paths.etc_for(SERVICE)?;
    for (name, text) in &rendered.files {
        if fs::read(etc.join(name)).ok().as_deref() != Some(text.as_bytes()) {
            return Ok(true);
        }
    }
    let Ok(entries) = fs::read_dir(etc.join("sites")) else {
        return Ok(false);
    };
    Ok(entries.flatten().any(|entry| {
        let file = entry.file_name().to_string_lossy().into_owned();
        is_conf(&file) && !rendered.files.contains_key(&format!("sites/{file}"))
    }))
}

fn is_conf(file: &str) -> bool {
    Path::new(file)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("conf"))
}

/// A configuration written in the staging folder, ready for `nginx -t -c <main>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedNginx {
    /// `etc/nginx/.next/nginx.conf`.
    pub main: PathBuf,
    /// Sites left out and other things worth saying.
    pub warnings: Vec<String>,
}

/// Writes the whole configuration into `etc/nginx/.next/`, with its own paths for the
/// generated includes, so Nginx can test it **without** touching the files a running or
/// restarting server reads. The previous contents of the staging folder are discarded. Only
/// after `nginx -t` accepts it is [`write_nginx_config`] called, with the same input, to
/// make it the real one.
pub fn stage_nginx_config(
    input: &NginxInput<'_>,
    dirs: &dyn PrivateDirs,
) -> Result<StagedNginx, NginxError> {
    require_private_home(input.paths, dirs)?;
    let stage = staging_dir(input.paths)
        .ok_or_else(|| NginxError::Paths(PathsError::InvalidComponent(SERVICE.to_owned())))?;
    let real = render(input);
    let rendered = RenderedNginx {
        files: render_in(input, &stage, &real.token).files,
        ..real
    };
    let io = |action: &'static str, path: &Path| {
        let path = path.to_path_buf();
        move |source| NginxError::Io {
            action,
            path,
            source,
        }
    };
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(io("cannot clear", &stage))?;
    }
    for dir in [
        input.paths.logs_for(SERVICE)?,
        input.paths.state_dir().join(SERVICE),
        stage.join("sites"),
    ] {
        fs::create_dir_all(&dir).map_err(io("cannot create", &dir))?;
    }
    for (name, text) in &rendered.files {
        let path = stage.join(name);
        fs::write(&path, text).map_err(io("cannot write", &path))?;
    }
    Ok(StagedNginx {
        main: stage.join("nginx.conf"),
        warnings: rendered.warnings,
    })
}

#[cfg(test)]
mod tests;
