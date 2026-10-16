use std::fs;
use std::time::Duration as StdDuration;

use rcgen::{CertificateParams, Issuer, KeyPair};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tempfile::TempDir;
use webpki::{anchor_from_trusted_cert, EndEntityCert, KeyUsage};
use x509_parser::extensions::ParsedExtension;

use super::*;
use crate::ca::{create_ca, ensure_ca};
use crate::paths::fake::FakePrivateDirs;

fn now() -> SystemTime {
    SystemTime::now()
}

fn days(n: u64) -> StdDuration {
    StdDuration::from_secs(n * 86_400)
}

fn hosts() -> Vec<String> {
    vec!["shop.test".to_owned(), "app.test".to_owned()]
}

fn world() -> (TempDir, CrisolPaths, LocalCa) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let ca = ensure_ca(&paths, &FakePrivateDirs::new(), "test", now())
        .unwrap()
        .ca;
    (tmp, paths, ca)
}

fn issue_for(paths: &CrisolPaths, ca: &LocalCa, hosts: &[String], at: SystemTime) -> SiteCert {
    ensure_site_cert(paths, ca, hosts, at).unwrap()
}

fn leaf_der(paths: &CrisolPaths) -> Vec<u8> {
    let pem = fs::read_to_string(paths.site_cert()).unwrap();
    x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .unwrap()
        .1
        .contents
}

/// Validates `leaf` against **only** the CA, for `host`, at `at`: what a client that trusts
/// nothing but `certs/ca.pem` does (the webpki verifier enforces name constraints).
fn verify(ca: &LocalCa, leaf: &[u8], host: &str, at: SystemTime) -> Result<(), webpki::Error> {
    let ca_der = CertificateDer::from(ca.cert_der().to_vec());
    let anchor = anchor_from_trusted_cert(&ca_der).unwrap();
    let leaf_der = CertificateDer::from(leaf.to_vec());
    let end_entity = EndEntityCert::try_from(&leaf_der).unwrap();
    let time = UnixTime::since_unix_epoch(at.duration_since(SystemTime::UNIX_EPOCH).unwrap());
    end_entity.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        &[anchor],
        &[],
        time,
        KeyUsage::server_auth(),
        None,
        None,
    )?;
    end_entity.verify_is_valid_for_subject_name(&ServerName::try_from(host).unwrap())
}

// ---- what the certificate says ---------------------------------------------------------

#[test]
fn the_names_are_the_secured_sites_and_the_local_ones() {
    assert_eq!(
        expected_names(&hosts()),
        ["app.test", "shop.test", "localhost", "127.0.0.1", "::1"]
    );
    // Order and case do not matter; repeats and a trailing dot are dropped.
    assert_eq!(
        expected_names(&[
            "B.test.".to_owned(),
            "a.test".to_owned(),
            "b.test".to_owned()
        ]),
        ["a.test", "b.test", "localhost", "127.0.0.1", "::1"]
    );
    assert_eq!(expected_names(&[]), ["localhost", "127.0.0.1", "::1"]);
}

#[test]
fn it_is_a_server_certificate_and_has_no_wildcard() {
    let (_tmp, paths, ca) = world();
    let made = issue_for(&paths, &ca, &hosts(), now());
    assert_eq!(made.changed, Some(Renewal::Missing));
    assert_eq!(made.cert, paths.site_cert());

    let der = leaf_der(&paths);
    let cert = X509Certificate::from_der(&der).unwrap().1;
    let expected: BTreeSet<String> = ["app.test", "shop.test", "localhost", "127.0.0.1", "::1"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(san_set(&cert), expected);
    assert!(
        san_set(&cert).iter().all(|n| !n.contains('*')),
        "rustls-webpki refuses a certificate with `*.test`"
    );

    let basic = cert.basic_constraints().unwrap();
    assert!(basic.is_none_or(|b| !b.value.ca), "not a CA");
    let usage = cert.key_usage().unwrap().unwrap();
    assert!(usage.value.digital_signature() && !usage.value.key_cert_sign());
    let eku = cert
        .extensions()
        .iter()
        .find_map(|e| match e.parsed_extension() {
            ParsedExtension::ExtendedKeyUsage(eku) => Some(eku),
            _ => None,
        })
        .expect("extended key usage");
    assert!(eku.server_auth && !eku.client_auth && !eku.code_signing);
    assert_eq!(
        cert.issuer(),
        X509Certificate::from_der(ca.cert_der())
            .unwrap()
            .1
            .subject()
    );
}

#[test]
fn it_lasts_a_year_starting_yesterday() {
    let (_tmp, paths, ca) = world();
    let at = now();
    issue_for(&paths, &ca, &hosts(), at);
    let der = leaf_der(&paths);
    let cert = X509Certificate::from_der(&der).unwrap().1;
    let day = 86_400;
    assert!((cert.validity().not_before.timestamp() - (unix(at) - day)).abs() < 5);
    let span = cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp();
    assert!((span - 366 * day).abs() < 5, "{span}");
}

#[test]
fn it_has_its_own_key_not_the_cas() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let site_key = fs::read_to_string(paths.site_key()).unwrap();
    assert!(site_key.contains("PRIVATE KEY"));
    assert_ne!(site_key, fs::read_to_string(paths.ca_key()).unwrap());
    assert!(!fs::read_to_string(paths.site_cert())
        .unwrap()
        .contains("PRIVATE"));
}

// ---- a client that trusts only the CA ---------------------------------------------------

#[test]
fn a_client_that_trusts_only_the_ca_accepts_the_secured_and_the_local_names() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let leaf = leaf_der(&paths);
    for host in ["app.test", "shop.test", "localhost", "127.0.0.1", "::1"] {
        verify(&ca, &leaf, host, now()).unwrap_or_else(|e| panic!("{host}: {e:?}"));
    }
}

#[test]
fn it_does_not_vouch_for_sites_that_were_not_secured_or_for_other_names() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let leaf = leaf_der(&paths);
    for host in [
        "other.test",
        "example.com",
        "a.b.test",
        "app.test.com",
        "mytest",
    ] {
        assert!(verify(&ca, &leaf, host, now()).is_err(), "{host}");
    }
}

#[test]
fn it_is_not_valid_before_it_starts_or_after_it_ends() {
    let (_tmp, paths, ca) = world();
    let at = now();
    issue_for(&paths, &ca, &hosts(), at);
    let leaf = leaf_der(&paths);
    assert!(verify(&ca, &leaf, "app.test", at + days(364)).is_ok());
    assert!(verify(&ca, &leaf, "app.test", at + days(367)).is_err());
    assert!(verify(&ca, &leaf, "app.test", at - days(3)).is_err());
}

#[test]
fn a_wildcard_under_a_single_label_tld_is_refused_by_rustls_webpki() {
    // The reason there is no `*.test` in the certificate: this verifier rejects the whole
    // certificate as soon as it has one, even for a host named explicitly in it.
    let (_tmp, _paths, ca) = world();
    let issuer = Issuer::from_ca_cert_pem(ca.cert_pem(), ca.key()).unwrap();
    let key = KeyPair::generate().unwrap();
    let with_wildcard = CertificateParams::new(vec!["app.test".to_owned(), "*.test".to_owned()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let error = verify(&ca, with_wildcard.der(), "app.test", now()).unwrap_err();
    assert!(
        matches!(error, webpki::Error::MalformedDnsIdentifier),
        "{error:?}"
    );
}

#[test]
fn a_stolen_ca_key_cannot_vouch_for_a_public_site() {
    // The point of the name constraints (ADR-0013): even a certificate that the CA signs
    // correctly for another domain is refused by a client that trusts the CA.
    let (_tmp, _paths, ca) = world();
    let key = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec!["bank.example.com".to_owned()]).unwrap();
    let issuer = Issuer::from_ca_cert_pem(ca.cert_pem(), ca.key()).unwrap();
    let forged = params.signed_by(&key, &issuer).unwrap();

    let error = verify(&ca, forged.der(), "bank.example.com", now()).unwrap_err();
    assert!(
        matches!(error, webpki::Error::NameConstraintViolation),
        "{error:?}"
    );
    // The same CA, same signing code, for a local name: accepted. So the refusal is the
    // constraint, not a broken setup.
    let local = CertificateParams::new(vec!["x.test".to_owned()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    assert!(verify(&ca, local.der(), "x.test", now()).is_ok());
}

// ---- when it is made again -------------------------------------------------------------

#[test]
fn a_current_certificate_is_kept() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let (cert, key) = (
        fs::read(paths.site_cert()).unwrap(),
        fs::read(paths.site_key()).unwrap(),
    );
    // The same sites in another order: still the same certificate.
    let mut shuffled = hosts();
    shuffled.reverse();
    let again = issue_for(&paths, &ca, &shuffled, now() + days(300));
    assert_eq!(again.changed, None);
    assert_eq!(fs::read(paths.site_cert()).unwrap(), cert);
    assert_eq!(fs::read(paths.site_key()).unwrap(), key);
    let names: Vec<_> = fs::read_dir(paths.certs_dir()).unwrap().flatten().collect();
    assert_eq!(names.len(), 4, "ca and sites, no temporary files");
}

#[test]
fn securing_or_unsecuring_a_site_makes_a_new_certificate() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let mut more = hosts();
    more.push("blog.test".to_owned());
    let added = issue_for(&paths, &ca, &more, now());
    assert_eq!(added.changed, Some(Renewal::Names));
    verify(&ca, &leaf_der(&paths), "blog.test", now()).unwrap();

    let removed = issue_for(&paths, &ca, &hosts(), now());
    assert_eq!(removed.changed, Some(Renewal::Names));
    assert!(verify(&ca, &leaf_der(&paths), "blog.test", now()).is_err());
}

#[test]
fn it_is_renewed_thirty_days_before_it_ends() {
    let (_tmp, paths, ca) = world();
    let at = now();
    issue_for(&paths, &ca, &hosts(), at);
    // 366 days of validity from yesterday: 335 days from now it has 30 left.
    assert_eq!(
        issue_for(&paths, &ca, &hosts(), at + days(330)).changed,
        None,
        "35 days left"
    );
    let first = fs::read(paths.site_cert()).unwrap();
    let renewed = issue_for(&paths, &ca, &hosts(), at + days(340));
    assert_eq!(renewed.changed, Some(Renewal::Expiring));
    assert_ne!(fs::read(paths.site_cert()).unwrap(), first);
    // And one that already expired is replaced too.
    let late = issue_for(&paths, &ca, &hosts(), at + days(800));
    assert_eq!(late.changed, Some(Renewal::Expiring));
    verify(&ca, &leaf_der(&paths), "app.test", at + days(800)).unwrap();
}

#[test]
fn a_site_the_ca_cannot_issue_for_is_refused_and_nothing_is_written() {
    let (_tmp, paths, ca) = world();
    let error = ensure_site_cert(
        &paths,
        &ca,
        &["app.test".to_owned(), "bank.example.com".to_owned()],
        now(),
    )
    .unwrap_err();
    assert!(
        matches!(error, CaError::NotCovered(ref h) if h == "bank.example.com"),
        "{error}"
    );
    assert!(!paths.site_cert().exists() && !paths.site_key().exists());
}

#[test]
fn a_new_ca_makes_the_old_certificate_stale() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());
    let new_ca = create_ca(&paths, &FakePrivateDirs::new(), "test", now(), true).unwrap();
    let again = issue_for(&paths, &new_ca, &hosts(), now());
    assert_eq!(again.changed, Some(Renewal::OtherCa));
    verify(&new_ca, &leaf_der(&paths), "app.test", now()).unwrap();
}

#[test]
fn damaged_or_missing_files_are_made_again_without_a_fuss() {
    let (_tmp, paths, ca) = world();
    issue_for(&paths, &ca, &hosts(), now());

    fs::write(paths.site_cert(), "garbage").unwrap();
    assert_eq!(
        issue_for(&paths, &ca, &hosts(), now()).changed,
        Some(Renewal::Unreadable)
    );
    fs::remove_file(paths.site_key()).unwrap();
    assert_eq!(
        issue_for(&paths, &ca, &hosts(), now()).changed,
        Some(Renewal::Unreadable),
        "half a pair"
    );
    // A key that is not the certificate's.
    fs::write(
        paths.site_key(),
        KeyPair::generate().unwrap().serialize_pem(),
    )
    .unwrap();
    assert_eq!(
        issue_for(&paths, &ca, &hosts(), now()).changed,
        Some(Renewal::Unreadable)
    );
    assert_eq!(issue_for(&paths, &ca, &hosts(), now()).changed, None);
    verify(&ca, &leaf_der(&paths), "app.test", now()).unwrap();
}

#[test]
fn the_ca_files_are_never_touched() {
    let (_tmp, paths, ca) = world();
    let (pem, key) = (
        fs::read(paths.ca_cert()).unwrap(),
        fs::read(paths.ca_key()).unwrap(),
    );
    issue_for(&paths, &ca, &hosts(), now());
    issue_for(&paths, &ca, &[], now() + days(400));
    assert_eq!(fs::read(paths.ca_cert()).unwrap(), pem);
    assert_eq!(fs::read(paths.ca_key()).unwrap(), key);
}
