//! CA bundle tests. `testdata/mozilla-excerpt.pem` is the header and the first two
//! certificates of the real `cacert-2026-09-25.pem` from curl.se, byte for byte.

use std::fs;
use std::time::SystemTime;

use tempfile::TempDir;

use crate::paths::fake::FakePrivateDirs;

use super::*;

const EXCERPT: &str = include_str!("testdata/mozilla-excerpt.pem");

/// A made-up "certificate": it only has to look like a DER SEQUENCE, since the merge works on
/// bytes.
fn fake(n: u8) -> Vec<u8> {
    let mut der = vec![0x30, 0x12];
    der.extend([n; 18]);
    der
}

fn mozilla_ders() -> Vec<Vec<u8>> {
    pem_certificates(EXCERPT)
}

#[test]
fn the_real_bundle_is_read_ignoring_names_and_comments() {
    let ders = mozilla_ders();
    assert_eq!(ders.len(), 2);
    for der in &ders {
        assert_eq!(der[0], 0x30, "a DER certificate starts with a SEQUENCE");
        assert!(der.len() > 300);
    }
    assert_ne!(ders[0], ders[1]);
}

#[test]
fn pem_survives_a_round_trip_and_wraps_at_sixty_four_columns() {
    for der in mozilla_ders().into_iter().chain([fake(7)]) {
        let pem = to_pem(&der);
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
        assert!(pem.lines().all(|l| l.len() <= 64), "{pem}");
        assert_eq!(pem_certificates(&pem), [der]);
    }
}

#[test]
fn mozilla_s_text_is_kept_verbatim_at_the_start() {
    let merged = merge_bundle(EXCERPT, &[fake(1)]);
    assert!(
        merged.text.starts_with(EXCERPT),
        "the first bytes are Mozilla's, untouched"
    );
    assert_eq!(merged.added, 1);
    assert_eq!(pem_certificates(&merged.text).len(), 3);
}

#[test]
fn nothing_to_add_leaves_the_bundle_exactly_as_it_is() {
    let merged = merge_bundle(EXCERPT, &[]);
    assert_eq!(merged.text, EXCERPT);
    assert_eq!((merged.added, merged.skipped), (0, 0));

    // Roots Mozilla already has are not added again, however often the system lists them.
    let ders = mozilla_ders();
    let same = merge_bundle(
        EXCERPT,
        &[ders[0].clone(), ders[1].clone(), ders[0].clone()],
    );
    assert_eq!(same.text, EXCERPT);
    assert_eq!(same.added, 0);
}

#[test]
fn duplicates_among_the_system_roots_are_added_once_and_in_a_stable_order() {
    let ders = mozilla_ders();
    let a = merge_bundle(
        EXCERPT,
        &[fake(9), fake(3), ders[0].clone(), fake(9), fake(3)],
    );
    let b = merge_bundle(EXCERPT, &[fake(3), fake(9)]);
    assert_eq!(a.added, 2);
    assert_eq!(
        a.text, b.text,
        "same additions, same file, whatever the order they came in"
    );
    let added = &pem_certificates(&a.text)[2..];
    assert_eq!(added, [fake(3), fake(9)], "sorted");
}

#[test]
fn things_that_are_not_certificates_are_left_out_and_counted() {
    let merged = merge_bundle(EXCERPT, &[vec![], vec![1, 2, 3], vec![0x31; 40], fake(5)]);
    assert_eq!((merged.added, merged.skipped), (1, 3));
    assert_eq!(pem_certificates(&merged.text).len(), 3);
}

#[test]
fn a_bundle_without_a_final_newline_gets_one_before_the_additions() {
    let merged = merge_bundle(EXCERPT.trim_end(), &[fake(2)]);
    assert!(merged
        .text
        .contains("-----END CERTIFICATE-----\n\n##\n## 1 root"));
    assert_eq!(pem_certificates(&merged.text).len(), 3);
}

// ---- writing the file -----------------------------------------------------------------

struct Roots(io::Result<Vec<SystemRoot>>);

impl Roots {
    /// Roots with no restriction.
    fn plain(ders: Vec<Vec<u8>>) -> Self {
        Self(Ok(ders.into_iter().map(SystemRoot::unrestricted).collect()))
    }
}

impl RootCertificates for Roots {
    fn trusted_roots(&self) -> io::Result<Vec<SystemRoot>> {
        match &self.0 {
            Ok(roots) => Ok(roots.clone()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }

    fn disallowed_roots(&self) -> io::Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

/// Roots plus a `Disallowed` list.
struct Distrusting(Roots, io::Result<Vec<Vec<u8>>>);

impl RootCertificates for Distrusting {
    fn trusted_roots(&self) -> io::Result<Vec<SystemRoot>> {
        self.0.trusted_roots()
    }

    fn disallowed_roots(&self) -> io::Result<Vec<Vec<u8>>> {
        match &self.1 {
            Ok(list) => Ok(list.clone()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }
}

/// The file Crisol wrote, without the comment it puts at the top.
fn without_header(text: &str) -> &str {
    text.split_once(
        "##

",
    )
    .map_or(text, |(_, rest)| rest)
}

fn home_with_bundle(versions: &[&str]) -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    for version in versions {
        let dir = paths.bin_for(BUNDLE_SERVICE, version).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(BUNDLE_FILE), format!("{EXCERPT}## {version}\n")).unwrap();
    }
    (tmp, paths)
}

#[test]
fn without_an_installed_bundle_there_is_nothing_to_build() {
    let (_tmp, paths) = home_with_bundle(&[]);
    assert_eq!(write_php_cacert(&paths, &NoRoots).unwrap(), None);
    assert!(!paths.php_cacert().exists());
}

#[test]
fn the_file_is_built_in_the_layout_from_the_bundle_and_the_system() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots::plain(vec![fake(4), mozilla_ders()[1].clone()]);

    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!(written.path, paths.php_cacert());
    assert!(written
        .path
        .ends_with(std::path::Path::new("etc").join("php").join("cacert.pem")));
    assert!(written.changed);
    assert_eq!(
        (written.added, written.bundle_version.as_str()),
        (1, "2026.9.25")
    );
    assert_eq!(written.warnings, Vec::<String>::new());

    let text = fs::read_to_string(&written.path).unwrap();
    assert!(without_header(&text).starts_with(EXCERPT));
    assert_eq!(pem_certificates(&text).len(), 3);
}

#[test]
fn the_newest_installed_bundle_is_used() {
    let (_tmp, paths) = home_with_bundle(&["2026.7.16", "2026.9.25", "2026.8.13"]);
    let written = write_php_cacert(&paths, &NoRoots).unwrap().unwrap();
    assert_eq!(written.bundle_version, "2026.9.25");
    assert!(fs::read_to_string(written.path)
        .unwrap()
        .contains("## 2026.9.25"));
}

#[test]
fn generating_twice_does_not_touch_the_file() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots::plain(vec![fake(4)]);
    let first = write_php_cacert(&paths, &system).unwrap().unwrap();
    let stamp = fs::metadata(&first.path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let second = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert!(!second.changed);
    assert_eq!(
        fs::metadata(&second.path).unwrap().modified().unwrap(),
        stamp
    );

    // A root added to the system later shows up the next time it is generated.
    let more = Roots::plain(vec![fake(4), fake(6)]);
    assert!(write_php_cacert(&paths, &more).unwrap().unwrap().changed);
}

#[test]
fn a_system_store_that_cannot_be_read_still_gives_mozilla_s_bundle() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots(Err(io::Error::other("the store is locked")));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!(written.added, 0);
    assert_eq!(written.warnings.len(), 1);
    assert!(written.warnings[0].contains("the store is locked"));
    assert!(without_header(&fs::read_to_string(written.path).unwrap()).starts_with(EXCERPT));
}

#[test]
fn invalid_system_certificates_are_reported_not_fatal() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots::plain(vec![vec![0, 1], fake(8)]);
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!(written.added, 1);
    assert!(
        written.warnings[0].contains("1 system certificate"),
        "{:?}",
        written.warnings
    );
}

#[test]
fn a_hand_edited_derived_file_is_put_back() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let written = write_php_cacert(&paths, &NoRoots).unwrap().unwrap();
    fs::write(&written.path, "tampered").unwrap();
    assert!(write_php_cacert(&paths, &NoRoots).unwrap().unwrap().changed);
    assert!(without_header(&fs::read_to_string(&written.path).unwrap()).starts_with(EXCERPT));
}

// ---- which system roots are fit for TLS -------------------------------------------------

fn root(n: u8, usages: Option<&[&str]>, partially_distrusted: bool) -> SystemRoot {
    SystemRoot {
        der: fake(n),
        usages: usages.map(|u| u.iter().map(|s| (*s).to_owned()).collect()),
        partially_distrusted,
    }
}

const CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";
const EMAIL: &str = "1.3.6.1.5.5.7.3.4";

#[test]
fn a_root_limited_to_code_signing_or_mail_is_not_added() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots(Ok(vec![
        root(1, Some(&[CODE_SIGNING]), false),
        root(2, Some(&[CODE_SIGNING, EMAIL]), false),
        root(3, Some(&[]), false),
    ]));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!((written.added, written.omitted), (0, 3));
    assert!(
        written.warnings[0].contains("3 system root"),
        "{:?}",
        written.warnings
    );
    let text = fs::read_to_string(&written.path).unwrap();
    assert_eq!(
        pem_certificates(&text).len(),
        2,
        "nothing but Mozilla's bundle"
    );
}

#[test]
fn a_root_for_server_authentication_or_with_no_restriction_is_added() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots(Ok(vec![
        root(1, None, false),
        root(2, Some(&[SERVER_AUTH]), false),
        root(3, Some(&[CODE_SIGNING, SERVER_AUTH]), false),
        root(4, Some(&[ANY_USAGE]), false),
    ]));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!((written.added, written.omitted), (4, 0));
    assert_eq!(written.warnings, Vec::<String>::new());
}

#[test]
fn a_partially_distrusted_root_is_not_added_even_if_it_allows_tls() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Roots(Ok(vec![
        root(1, None, true),
        root(2, Some(&[SERVER_AUTH]), true),
        root(3, None, false),
    ]));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!((written.added, written.omitted), (1, 2));
    assert_eq!(
        pem_certificates(&fs::read_to_string(&written.path).unwrap())[2..],
        [fake(3)]
    );
}

// ---- the local CA (ADR-0013) -----------------------------------------------------------

/// A real, validated local CA in the home of the test.
fn local_ca(paths: &CrisolPaths) -> crate::ca::LocalCa {
    crate::ca::ensure_ca(
        paths,
        &crate::paths::fake::FakePrivateDirs::new(),
        "test",
        std::time::SystemTime::now(),
    )
    .unwrap()
    .ca
}

#[test]
fn the_local_ca_is_in_the_bundle_even_if_the_system_does_not_have_it() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let ca = local_ca(&paths);
    let local = ca.cert_der().to_vec();
    let written = write_php_cacert_with(&paths, &NoRoots, Some(&ca))
        .unwrap()
        .unwrap();
    assert_eq!(written.added, 1);
    let text = fs::read_to_string(&written.path).unwrap();
    assert!(without_header(&text).starts_with(EXCERPT));
    assert_eq!(pem_certificates(&text).last(), Some(&local));
    // Without it, the file is exactly Mozilla's: the plain function is unchanged.
    let plain = write_php_cacert(&paths, &NoRoots).unwrap().unwrap();
    assert!(plain.changed && plain.added == 0);
}

#[test]
fn the_local_ca_is_added_once_even_if_the_system_lists_it_and_is_not_filtered_like_them() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let ca = local_ca(&paths);
    let local = ca.cert_der().to_vec();
    // The system lists it twice, once restricted to code signing (which would drop a system
    // root): the local CA still ends up in the file, once.
    let restricted = SystemRoot {
        der: local.clone(),
        usages: Some(vec![CODE_SIGNING.to_owned()]),
        partially_distrusted: false,
    };
    let system = Roots(Ok(vec![
        SystemRoot::unrestricted(local.clone()),
        restricted,
        SystemRoot::unrestricted(fake(7)),
    ]));
    let written = write_php_cacert_with(&paths, &system, Some(&ca))
        .unwrap()
        .unwrap();
    let text = fs::read_to_string(&written.path).unwrap();
    let ders = pem_certificates(&text);
    assert_eq!(ders.iter().filter(|d| **d == local).count(), 1);
    assert!(ders.contains(&fake(7)));
    assert_eq!(
        written.omitted, 1,
        "only the restricted system copy is reported"
    );
}

// ---- review of phase 4, finding 6: what the system distrusts leaves Mozilla's bundle too ------

/// A bundle of three entries in Mozilla's format: header lines, a block, a blank line.
fn bundle_of_three() -> (String, [Vec<u8>; 3]) {
    let ders = [fake(1), fake(2), fake(3)];
    let mut text = String::from(
        "##
## Bundle of CA Root Certificates
##

",
    );
    for (der, name) in ders.iter().zip(["First CA", "Second CA", "Third CA"]) {
        let underline = "=".repeat(name.len());
        let _ = writeln!(
            text,
            "{name}
{underline}
{}",
            to_pem(der)
        );
    }
    (text, ders)
}

#[test]
fn a_disallowed_certificate_leaves_the_bundle_and_the_rest_stays_in_order() {
    let (text, [one, two, three]) = bundle_of_three();
    let disallowed: BTreeSet<Vec<u8>> = [two.clone()].into();
    let (kept, excluded) = exclude_certificates(&text, &disallowed);
    assert_eq!(excluded, vec![two]);
    assert_eq!(pem_certificates(&kept), vec![one, three]);
    assert!(
        kept.contains(
            "First CA
======="
        ) && kept.contains("Third CA")
    );
    assert!(
        !kept.contains("Second CA"),
        "its name header goes with it: {kept}"
    );
    // Everything else is byte for byte what it was: removing the entry from the original
    // text by hand gives exactly this.
    let by_hand = text.replacen(
        &format!(
            "Second CA
=========
{}
",
            to_pem(&fake(2))
        ),
        "",
        1,
    );
    assert_eq!(kept, by_hand);

    // Nothing disallowed, nothing changes; other certificates in the list are ignored.
    let (same, none) = exclude_certificates(&text, &BTreeSet::new());
    assert_eq!((same.as_str(), none.len()), (text.as_str(), 0));
    let (same, none) = exclude_certificates(&text, &[fake(9)].into());
    assert_eq!((same.as_str(), none.len()), (text.as_str(), 0));
}

#[test]
fn the_written_file_leaves_out_a_distrusted_mozilla_root_names_it_and_says_it_is_not_verbatim() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let dir = paths.bin_for(BUNDLE_SERVICE, "2026.9.25").unwrap();
    fs::create_dir_all(&dir).unwrap();
    // The real excerpt: two real certificates with real names.
    fs::write(dir.join(BUNDLE_FILE), EXCERPT).unwrap();
    let ders = mozilla_ders();
    let first = certificate_name(&ders[0]);
    assert_ne!(first, "unnamed certificate");

    let system = Distrusting(Roots::plain(vec![fake(5)]), Ok(vec![ders[0].clone()]));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    let text = fs::read_to_string(&written.path).unwrap();
    assert_eq!(written.excluded, vec![first.clone()]);
    assert_eq!(pem_certificates(&text), vec![ders[1].clone(), fake(5)]);
    assert_eq!(written.added, 1);
    // One warning that counts them and names them.
    assert!(
        written
            .warnings
            .iter()
            .any(|w| w.contains("1 certificate(s) of Mozilla's bundle") && w.contains(&first)),
        "{:?}",
        written.warnings
    );
    // The comment at the top: not verbatim, which version, and who was left out.
    assert!(text.starts_with(
        "##
## Built by Crisol from Mozilla's CA bundle, version 2026.9.25"
    ));
    assert!(text.contains("NOT a verbatim copy"), "{text}");
    assert!(text.contains(&format!("##   - {first}")), "{text}");
}

#[test]
fn nothing_distrusted_still_says_where_the_file_comes_from_and_leaves_the_bundle_whole() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let written = write_php_cacert(&paths, &NoRoots).unwrap().unwrap();
    let text = fs::read_to_string(&written.path).unwrap();
    assert_eq!(written.excluded, Vec::<String>::new());
    assert!(
        text.contains("version 2026.9.25") && text.contains("Left out: none."),
        "{text}"
    );
    assert!(without_header(&text).starts_with(EXCERPT));
    assert_eq!(pem_certificates(&text).len(), 2);
}

#[test]
fn the_local_ca_still_goes_in_even_if_it_is_in_the_distrusted_list() {
    // The local CA is Crisol's own and is validated on its own terms (ADR-0013): a stray entry
    // in a Disallowed store does not take it out of the file PHP uses for the local sites.
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let ca_home = TempDir::new().unwrap();
    let ca_paths = CrisolPaths::new(ca_home.path().join("ca-home")).unwrap();
    let private = FakePrivateDirs::new();
    let ca = crate::ca::ensure_ca(&ca_paths, &private, "test", SystemTime::now())
        .unwrap()
        .ca;
    let local = ca.cert_der().to_vec();
    let system = Distrusting(
        Roots::plain(vec![]),
        Ok(vec![local.clone(), mozilla_ders()[1].clone()]),
    );
    let written = write_php_cacert_with(&paths, &system, Some(&ca))
        .unwrap()
        .unwrap();
    let ders = pem_certificates(&fs::read_to_string(&written.path).unwrap());
    assert_eq!(ders.last(), Some(&local), "the local CA is there");
    assert!(
        !ders.contains(&mozilla_ders()[1]),
        "the Mozilla root that was distrusted is not"
    );
    assert_eq!(written.excluded.len(), 1);
}

#[test]
fn a_distrusted_list_that_cannot_be_read_keeps_the_bundle_whole_and_says_so() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let system = Distrusting(Roots::plain(vec![]), Err(io::Error::other("store busy")));
    let written = write_php_cacert(&paths, &system).unwrap().unwrap();
    assert_eq!(written.excluded, Vec::<String>::new());
    assert!(written
        .warnings
        .iter()
        .any(|w| w.contains("store busy") && w.contains("kept whole")));
    assert_eq!(
        pem_certificates(&fs::read_to_string(&written.path).unwrap()).len(),
        2
    );
}

#[test]
fn a_certificate_without_a_readable_name_is_called_so() {
    assert_eq!(certificate_name(&fake(1)), "unnamed certificate");
    assert_ne!(certificate_name(&mozilla_ders()[0]), "unnamed certificate");
}
