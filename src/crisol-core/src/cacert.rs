//! The CA bundle for PHP's cURL (ADR-0011).
//!
//! The cURL that ships with PHP for Windows needs a file of certificate authorities
//! (`curl.cainfo`); without one every HTTPS request fails with "unable to get local issuer
//! certificate". Crisol builds that file in `etc/php/cacert.pem` from two sources:
//!
//! 1. **Mozilla's bundle**, installed from the catalog as the `cacert` package (so it is
//!    downloaded and verified like everything else, ADR-0005), copied as it is **except** for
//!    the certificates the system distrusts: whatever is in a `Disallowed` store of Windows
//!    (user, machine, policy, enterprise) is left out, matched by its exact DER, so a CA the
//!    company or Microsoft has revoked cannot come back through Mozilla's list. The file says
//!    so in a comment at the top, with the bundle's version and the names left out;
//! 2. the **trusted root certificates of the operating system** that Mozilla's bundle does not
//!    already have, appended after it: that is what makes a company's own CA, or the one of a
//!    proxy that inspects TLS, work without configuration.
//!
//! The operating-system part is behind [`RootCertificates`]; the Windows implementation is in
//! `crisol-platform`. It returns only certificates the system *trusts* (never the
//! `Disallowed` store) together with their usage restrictions, and only the ones fit to
//! authenticate TLS servers are added (see [`SystemRoot::is_fit_for_tls`]): the bundle must
//! not trust more than Windows or Mozilla do.
//!
//! The file is derived like everything under `etc/`: it is rewritten whenever it is generated
//! and left alone when nothing changed.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use crate::ca::LocalCa;
use crate::manage::list_installed;
use crate::paths::CrisolPaths;
use crate::phpini::{write_if_changed, PhpIniError};

/// The service name of Mozilla's bundle in the catalog.
pub const BUNDLE_SERVICE: &str = "cacert";

/// File name of the bundle inside its installation (the catalog's `executable`).
pub const BUNDLE_FILE: &str = "cacert.pem";

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// The OID of "TLS web server authentication" (`serverAuth`).
pub const SERVER_AUTH: &str = "1.3.6.1.5.5.7.3.1";

/// The OID of "any extended key usage": a certificate that lists it is good for every purpose.
pub const ANY_USAGE: &str = "2.5.29.37.0";

/// A root certificate of the operating system with what the system says about its use.
///
/// The Windows `ROOT` store is not a flat list of "roots to trust for TLS": an entry can be
/// limited to code signing or mail, or be partially distrusted (how Microsoft retires a CA
/// without deleting it). Copying only the DER would lose that, so it travels along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemRoot {
    /// The DER encoding of the certificate.
    pub der: Vec<u8>,
    /// The purposes the system allows it for, as dotted OIDs. `None`: no restriction.
    pub usages: Option<Vec<String>>,
    /// The system distrusts it for some purpose or after some date (partial distrust).
    pub partially_distrusted: bool,
}

impl SystemRoot {
    /// A root with no restriction and no distrust.
    #[must_use]
    pub fn unrestricted(der: Vec<u8>) -> Self {
        Self {
            der,
            usages: None,
            partially_distrusted: false,
        }
    }

    /// Whether the root may be trusted to authenticate TLS servers: it is not partially
    /// distrusted, and it has no usage restriction or its restriction includes `serverAuth`.
    #[must_use]
    pub fn is_fit_for_tls(&self) -> bool {
        !self.partially_distrusted
            && self
                .usages
                .as_ref()
                .is_none_or(|u| u.iter().any(|oid| oid == SERVER_AUTH || oid == ANY_USAGE))
    }
}

/// Where the operating system's trusted root certificates come from.
pub trait RootCertificates: Send + Sync {
    /// Every root certificate the system trusts, for the user and for the machine (company
    /// policy included), with its usage restrictions. Certificates the system explicitly
    /// distrusts as a whole must not be in the list; partial distrust is reported in
    /// [`SystemRoot::partially_distrusted`]. Duplicates are fine.
    fn trusted_roots(&self) -> io::Result<Vec<SystemRoot>>;

    /// The certificates (DER) in the system's `Disallowed` stores, of every place. They are
    /// removed from Mozilla's bundle too.
    fn disallowed_roots(&self) -> io::Result<Vec<Vec<u8>>>;
}

/// No operating-system roots: the bundle is Mozilla's alone.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRoots;

impl RootCertificates for NoRoots {
    fn trusted_roots(&self) -> io::Result<Vec<SystemRoot>> {
        Ok(Vec::new())
    }

    fn disallowed_roots(&self) -> io::Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

/// The result of [`merge_bundle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedBundle {
    /// The complete file.
    pub text: String,
    /// How many system certificates were added after Mozilla's.
    pub added: usize,
    /// System certificates left out because they do not look like certificates.
    pub skipped: usize,
}

/// Mozilla's bundle, verbatim, followed by the certificates in `roots` that it does not
/// already contain. Output is deterministic: the additions are sorted, duplicates removed.
#[must_use]
pub fn merge_bundle(bundle: &str, roots: &[Vec<u8>]) -> MergedBundle {
    let known: BTreeSet<Vec<u8>> = pem_certificates(bundle).into_iter().collect();
    let mut new: BTreeSet<&Vec<u8>> = BTreeSet::new();
    let mut skipped = 0;
    for root in roots {
        if !looks_like_a_certificate(root) {
            skipped += 1;
        } else if !known.contains(root) {
            new.insert(root);
        }
    }

    let mut text = bundle.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if !new.is_empty() {
        let _ = write!(
            text,
            "\n##\n## {} root certificate(s) added by Crisol from the system store\n##\n\n",
            new.len()
        );
        for der in &new {
            text.push_str(&to_pem(der));
            text.push('\n');
        }
    }
    MergedBundle {
        text,
        added: new.len(),
        skipped,
    }
}

/// Mozilla's bundle without the certificates in `disallowed`: every other byte, and the
/// order of what remains, stays as it was. An entry is its name header (the lines right above
/// the block, up to a blank line), the block, and the blank line after it. Also returns the
/// DER of what was left out, in the order it appeared.
#[must_use]
pub fn exclude_certificates(
    bundle: &str,
    disallowed: &BTreeSet<Vec<u8>>,
) -> (String, Vec<Vec<u8>>) {
    if disallowed.is_empty() {
        return (bundle.to_owned(), Vec::new());
    }
    let lines: Vec<&str> = bundle.split_inclusive('\n').collect();
    let mut keep = vec![true; lines.len()];
    let mut excluded = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != BEGIN {
            i += 1;
            continue;
        }
        let Some(end) = (i..lines.len()).find(|&j| lines[j].trim() == END) else {
            break; // an unfinished block: leave the rest alone
        };
        let body: String = lines[i + 1..end].iter().map(|l| l.trim()).collect();
        let der = STANDARD.decode(body.as_bytes()).unwrap_or_default();
        if disallowed.contains(&der) {
            let mut start = i;
            while start > 0 && !lines[start - 1].trim().is_empty() && lines[start - 1].trim() != END
            {
                start -= 1;
            }
            let mut stop = end + 1;
            if lines.get(stop).is_some_and(|l| l.trim().is_empty()) {
                stop += 1;
            }
            keep[start..stop].fill(false);
            excluded.push(der);
        }
        i = end + 1;
    }
    let text = lines
        .iter()
        .zip(&keep)
        .filter_map(|(line, keep)| keep.then_some(*line))
        .collect();
    (text, excluded)
}

/// The common name of a DER certificate, or its organisation, or `unnamed certificate`.
#[must_use]
pub fn certificate_name(der: &[u8]) -> String {
    use x509_parser::prelude::{FromDer, X509Certificate};
    X509Certificate::from_der(der)
        .ok()
        .and_then(|(_, cert)| {
            let subject = cert.subject();
            subject
                .iter_common_name()
                .chain(subject.iter_organization())
                .find_map(|attribute| attribute.as_str().ok().map(str::to_owned))
        })
        .unwrap_or_else(|| "unnamed certificate".to_owned())
}

/// The comment at the top of the file Crisol builds: it is no longer Mozilla's file as it was.
fn provenance(version: &str, excluded: &[String]) -> String {
    let mut text = format!(
        "##\n## Built by Crisol from Mozilla's CA bundle, version {version} (the `cacert` package).\n\
         ## It is NOT a verbatim copy: the certificates that the system distrusts (the Windows\n\
         ## `Disallowed` stores) are left out, and the system's own trusted roots follow it.\n"
    );
    if excluded.is_empty() {
        text.push_str("## Left out: none.\n");
    } else {
        let _ = writeln!(text, "## Left out ({}):", excluded.len());
        for name in excluded {
            let _ = writeln!(text, "##   - {name}");
        }
    }
    text.push_str("##\n\n");
    text
}

/// A DER certificate is a SEQUENCE. This only keeps out obvious garbage; it is not a parser.
fn looks_like_a_certificate(der: &[u8]) -> bool {
    der.len() >= 16 && der[0] == 0x30
}

/// The DER of every `CERTIFICATE` block of a PEM text. Text between blocks (the names and
/// comments Mozilla's bundle has) is ignored, and a block that is not valid base64 is skipped.
fn pem_certificates(text: &str) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if line == BEGIN {
            current = Some(String::new());
        } else if line == END {
            if let Some(body) = current.take() {
                if let Ok(der) = STANDARD.decode(body.as_bytes()) {
                    found.push(der);
                }
            }
        } else if let Some(body) = current.as_mut() {
            body.push_str(line);
        }
    }
    found
}

fn to_pem(der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut pem = String::with_capacity(encoded.len() + 100);
    pem.push_str(BEGIN);
    pem.push('\n');
    for chunk in encoded.as_bytes().chunks(64) {
        // Base64 is ASCII.
        pem.push_str(&String::from_utf8_lossy(chunk));
        pem.push('\n');
    }
    pem.push_str(END);
    pem.push('\n');
    pem
}

/// What [`write_php_cacert`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenCacert {
    /// `etc/php/cacert.pem`.
    pub path: PathBuf,
    /// The file was created or its content changed.
    pub changed: bool,
    /// The version of Mozilla's bundle it was built from.
    pub bundle_version: String,
    /// The names of the certificates of Mozilla's bundle that were left out because the system
    /// distrusts them (its `Disallowed` stores).
    pub excluded: Vec<String>,
    /// System certificates added on top of Mozilla's.
    pub added: usize,
    /// System roots left out because their use is restricted to something other than TLS
    /// servers, or because the system partially distrusts them.
    pub omitted: usize,
    /// Things worth telling the user (the system store could not be read, certificates
    /// skipped).
    pub warnings: Vec<String>,
}

/// Builds `etc/php/cacert.pem` from the installed `cacert` package plus the system roots.
///
/// Returns `Ok(None)` when no bundle is installed: there is nothing to build on, and the
/// caller generates the `php.ini` without the CA directives (and says so). If the system
/// roots cannot be read, the file is built from Mozilla's bundle alone and a warning says
/// why: a broken store must not leave PHP without any certificates.
pub fn write_php_cacert(
    paths: &CrisolPaths,
    system: &dyn RootCertificates,
) -> Result<Option<WrittenCacert>, PhpIniError> {
    write_php_cacert_with(paths, system, None)
}

/// [`write_php_cacert`] plus Crisol's own local CA (`certs/ca.pem`, ADR-0013). A [`LocalCa`]
/// has already been validated (privacy of `certs/`, constraints), which is why nothing else
/// can be given here.
///
/// The local CA goes in **whatever the system store says**: PHP must trust `https://shop.test`
/// even if the user never installed the CA in Windows (or declined), and it is not filtered
/// like the system roots are. If the system store also lists it, it is added once.
pub fn write_php_cacert_with(
    paths: &CrisolPaths,
    system: &dyn RootCertificates,
    local_ca: Option<&LocalCa>,
) -> Result<Option<WrittenCacert>, PhpIniError> {
    let Some(bundle) = list_installed(paths)
        .map_err(|e| PhpIniError::Io {
            action: "cannot list the installed bundles in",
            path: paths.bin_dir(),
            source: io::Error::other(e.to_string()),
        })?
        .into_iter()
        .rfind(|i| i.service == BUNDLE_SERVICE)
    else {
        return Ok(None);
    };
    let source = bundle.path.join(BUNDLE_FILE);
    let text = std::fs::read_to_string(&source).map_err(|e| PhpIniError::Io {
        action: "cannot read",
        path: source.clone(),
        source: e,
    })?;

    let mut warnings = Vec::new();
    let roots = match system.trusted_roots() {
        Ok(roots) => roots,
        Err(error) => {
            warnings.push(format!(
                "could not read the system's root certificates ({error}); the CA bundle has only Mozilla's"
            ));
            Vec::new()
        }
    };
    // What the system distrusts leaves Mozilla's bundle too. If the list cannot be read the
    // bundle stays whole, and the warning says so.
    let disallowed: BTreeSet<Vec<u8>> = match system.disallowed_roots() {
        Ok(list) => list.into_iter().collect(),
        Err(error) => {
            warnings.push(format!(
                "could not read the system's distrusted certificates ({error}); Mozilla's bundle was kept whole"
            ));
            BTreeSet::new()
        }
    };
    let (text, excluded_ders) = exclude_certificates(&text, &disallowed);
    let excluded: Vec<String> = excluded_ders.iter().map(|d| certificate_name(d)).collect();
    if !excluded.is_empty() {
        warnings.push(format!(
            "{} certificate(s) of Mozilla's bundle were left out because the system distrusts them: {}",
            excluded.len(),
            excluded.join(", ")
        ));
    }
    let total = roots.len();
    let mut fit: Vec<Vec<u8>> = roots
        .into_iter()
        .filter(SystemRoot::is_fit_for_tls)
        .map(|r| r.der)
        .collect();
    let omitted = total - fit.len();
    fit.extend(local_ca.map(|ca| ca.cert_der().to_vec()));
    let mut merged = merge_bundle(&text, &fit);
    merged.text = format!(
        "{}{}",
        provenance(&bundle.version.to_string(), &excluded),
        merged.text
    );
    if merged.skipped > 0 {
        warnings.push(format!(
            "{} system certificate(s) were not valid and were left out",
            merged.skipped
        ));
    }

    if omitted > 0 {
        warnings.push(format!(
            "{omitted} system root(s) were left out because of their use or a partial distrust"
        ));
    }

    let path = paths.php_cacert();
    let changed = write_if_changed(&path, merged.text.as_bytes())?;
    Ok(Some(WrittenCacert {
        path,
        changed,
        bundle_version: bundle.version.to_string(),
        excluded,
        added: merged.added,
        omitted,
        warnings,
    }))
}

#[cfg(test)]
mod tests;
