//! CA bundle tests. `testdata/mozilla-excerpt.pem` is the header and the first two
//! certificates of the real `cacert-2026-09-25.pem` from curl.se, byte for byte.

use std::fs;

use tempfile::TempDir;

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
    assert!(text.starts_with(EXCERPT));
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
    assert!(fs::read_to_string(written.path)
        .unwrap()
        .starts_with(EXCERPT));
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
    assert!(fs::read_to_string(&written.path)
        .unwrap()
        .starts_with(EXCERPT));
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

#[test]
fn the_local_ca_is_in_the_bundle_even_if_the_system_does_not_have_it() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let local = fake(42);
    let written = write_php_cacert_with(&paths, &NoRoots, Some(&local))
        .unwrap()
        .unwrap();
    assert_eq!(written.added, 1);
    let text = fs::read_to_string(&written.path).unwrap();
    assert!(text.starts_with(EXCERPT));
    assert_eq!(pem_certificates(&text).last(), Some(&local));
    // Without it, the file is exactly Mozilla's: the plain function is unchanged.
    let plain = write_php_cacert(&paths, &NoRoots).unwrap().unwrap();
    assert!(plain.changed && plain.added == 0);
}

#[test]
fn the_local_ca_is_added_once_even_if_the_system_lists_it_and_is_not_filtered_like_them() {
    let (_tmp, paths) = home_with_bundle(&["2026.9.25"]);
    let local = fake(42);
    // The system lists it twice, once restricted to code signing (which would drop a system
    // root): the local CA still ends up in the file, once.
    let system = Roots(Ok(vec![
        SystemRoot::unrestricted(local.clone()),
        root(42, Some(&[CODE_SIGNING]), false),
        SystemRoot::unrestricted(fake(7)),
    ]));
    let written = write_php_cacert_with(&paths, &system, Some(&local))
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
