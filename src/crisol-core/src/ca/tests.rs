use std::fs;
use std::time::Duration as StdDuration;

use tempfile::TempDir;
use x509_parser::extensions::{GeneralName, ParsedExtension};

use x509_parser::prelude::{FromDer, X509Certificate};

use super::*;

fn home() -> (TempDir, CrisolPaths) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    (tmp, paths)
}

fn now() -> SystemTime {
    SystemTime::now()
}

fn parse(der: &[u8]) -> X509Certificate<'_> {
    X509Certificate::from_der(der).unwrap().1
}

fn made(tld: &str) -> (TempDir, CrisolPaths, LocalCa) {
    let (tmp, paths) = home();
    let ca = ensure_ca(&paths, tld, now()).unwrap().ca;
    (tmp, paths, ca)
}

// ---- what the certificate says -----------------------------------------------------

#[test]
fn it_is_a_ca_that_can_only_sign_end_certificates() {
    let (_tmp, _paths, ca) = made("test");
    let cert = parse(ca.cert_der());
    let basic = cert.basic_constraints().unwrap().unwrap();
    assert!(basic.value.ca);
    assert_eq!(basic.value.path_len_constraint, Some(0));
    assert!(basic.critical);

    let usage = cert.key_usage().unwrap().unwrap();
    assert!(usage.value.key_cert_sign() && usage.value.crl_sign());
    assert!(!usage.value.digital_signature());
    assert_eq!(cert.issuer(), cert.subject(), "self-signed");
    assert!(
        cert.verify_signature(None).is_ok(),
        "the signature is valid"
    );
}

#[test]
fn it_can_only_issue_for_the_local_names() {
    let (_tmp, _paths, ca) = made("test");
    let cert = parse(ca.cert_der());
    let constraints = cert
        .extensions()
        .iter()
        .find(|e| matches!(e.parsed_extension(), ParsedExtension::NameConstraints(_)))
        .expect("name constraints");
    assert!(
        constraints.critical,
        "a validator that ignores them must reject the CA"
    );

    assert_eq!(ca.info().permitted_dns, ["test", "localhost"]);
    assert_eq!(ca.info().permitted_ips, ["127.0.0.0/8", "::1/128"]);
    let ParsedExtension::NameConstraints(parsed) = constraints.parsed_extension() else {
        unreachable!()
    };
    assert!(parsed.excluded_subtrees.is_none());
    let dns: Vec<&str> = parsed
        .permitted_subtrees
        .iter()
        .flatten()
        .filter_map(|s| match &s.base {
            GeneralName::DNSName(name) => Some(*name),
            _ => None,
        })
        .collect();
    assert_eq!(dns, ["test", "localhost"]);
}

#[test]
fn a_tld_the_reserved_names_do_not_cover_is_added() {
    let (_tmp, _paths, ca) = made("internal");
    assert_eq!(ca.info().permitted_dns, ["test", "localhost", "internal"]);
    assert!(ca.covers("internal") && ca.covers("test"));
    // A TLD that is already under a permitted name adds nothing.
    let (_tmp, _paths, nested) = made("dev.test");
    assert_eq!(nested.info().permitted_dns, ["test", "localhost"]);
    assert!(nested.covers("dev.test"));
}

#[test]
fn covers_matches_whole_labels() {
    let (_tmp, _paths, ca) = made("test");
    for yes in [
        "test",
        "TEST",
        ".test",
        "dev.test",
        "a.b.test",
        "localhost",
        "x.localhost",
    ] {
        assert!(ca.covers(yes), "{yes}");
    }
    for no in ["", "com", "mytest", "test.com", "dev", "local"] {
        assert!(!ca.covers(no), "{no}");
    }
}

#[test]
fn the_name_says_which_ca_it_is() {
    let (_tmp, _paths, ca) = made("test");
    let name = &ca.info().common_name;
    let id = name.strip_prefix("Crisol Local CA ").expect(name);
    assert_eq!(id.len(), 6);
    assert!(
        id.chars()
            .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)),
        "{id}"
    );
    let cert = parse(ca.cert_der());
    let org: Vec<_> = cert
        .subject()
        .iter_organization()
        .filter_map(|o| o.as_str().ok())
        .collect();
    assert_eq!(org, ["Crisol"]);

    // Two CAs never share a name or a serial.
    let (_tmp2, _p2, other) = made("test");
    assert_ne!(ca.info().common_name, other.info().common_name);
    assert_ne!(ca.info().fingerprint, other.info().fingerprint);
}

#[test]
fn the_key_is_ecdsa_p256_and_has_identifiers() {
    let (_tmp, _paths, ca) = made("test");
    let cert = parse(ca.cert_der());
    assert_eq!(
        cert.public_key().algorithm.algorithm.to_id_string(),
        "1.2.840.10045.2.1",
        "id-ecPublicKey"
    );
    assert_eq!(
        cert.signature_algorithm.algorithm.to_id_string(),
        "1.2.840.10045.4.3.2",
        "ecdsa-with-SHA256"
    );
    assert!(cert
        .get_extension_unique(&x509_parser::oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
        .unwrap()
        .is_some());
    let serial = cert.raw_serial();
    assert_eq!(serial.len(), 16);
    assert!(serial[0] & 0x80 == 0, "positive");
}

#[test]
fn it_is_valid_from_yesterday_for_ten_years() {
    let start = now();
    let (_tmp, _paths, ca) = made("test");
    let info = ca.info();
    let day = 86_400;
    let issued = unix(start);
    assert!((info.not_before - (issued - day)).abs() < 5, "{info:?}");
    let span = info.not_after - info.not_before;
    assert!((span - (3651 * day)).abs() < 5, "{span}");
}

// ---- files and reuse ----------------------------------------------------------------

#[test]
fn it_is_made_once_and_the_second_call_changes_nothing() {
    let (_tmp, paths) = home();
    let first = ensure_ca(&paths, "test", now()).unwrap();
    assert!(first.created);
    assert!(
        paths.ca_cert().ends_with("certs/ca.pem") || paths.ca_cert().ends_with(r"certs\ca.pem")
    );
    let (pem, key) = (
        fs::read(paths.ca_cert()).unwrap(),
        fs::read(paths.ca_key()).unwrap(),
    );
    assert!(String::from_utf8_lossy(&key).contains("PRIVATE KEY"));
    assert!(
        !String::from_utf8_lossy(&pem).contains("PRIVATE"),
        "the certificate file has no key"
    );

    let second = ensure_ca(&paths, "test", now()).unwrap();
    assert!(!second.created);
    assert_eq!(second.ca.info(), first.ca.info());
    assert_eq!(fs::read(paths.ca_cert()).unwrap(), pem);
    assert_eq!(fs::read(paths.ca_key()).unwrap(), key);
    // No temporary files stay behind.
    let names: Vec<String> = fs::read_dir(paths.certs_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn the_reloaded_ca_is_the_one_that_was_made() {
    let (_tmp, paths) = home();
    let made = ensure_ca(&paths, "internal", now()).unwrap().ca;
    let loaded = load_ca(&paths, now()).unwrap().unwrap();
    assert_eq!(loaded.info(), made.info());
    assert_eq!(loaded.cert_der(), made.cert_der());
    assert_eq!(loaded.key().serialize_der(), made.key().serialize_der());
}

#[test]
fn without_files_there_is_no_ca() {
    let (_tmp, paths) = home();
    assert!(load_ca(&paths, now()).unwrap().is_none());
    assert!(!paths.certs_dir().exists(), "reading creates nothing");
}

#[test]
fn half_a_ca_is_an_error_and_nothing_is_overwritten() {
    for (keep, other, present, missing) in [
        ("ca.pem", "ca.key", "ca.pem", "ca.key"),
        ("ca.key", "ca.pem", "ca.key", "ca.pem"),
    ] {
        let (_tmp, paths) = home();
        ensure_ca(&paths, "test", now()).unwrap();
        let kept = paths.certs_dir().join(keep);
        let before = fs::read(&kept).unwrap();
        fs::remove_file(paths.certs_dir().join(other)).unwrap();

        let error = ensure_ca(&paths, "test", now()).unwrap_err();
        assert!(
            matches!(error, CaError::Incomplete { present: p, missing: m } if p == present && m == missing),
            "{error}"
        );
        assert_eq!(fs::read(&kept).unwrap(), before, "what is there is kept");
        assert!(!paths.certs_dir().join(other).exists());
    }
}

#[test]
fn a_key_that_is_not_the_certificates_is_refused() {
    let (_tmp, paths) = home();
    ensure_ca(&paths, "test", now()).unwrap();
    let stranger = KeyPair::generate().unwrap();
    fs::write(paths.ca_key(), stranger.serialize_pem()).unwrap();
    assert!(matches!(
        ensure_ca(&paths, "test", now()).unwrap_err(),
        CaError::KeyMismatch
    ));
}

#[test]
fn garbage_is_an_error_not_a_reason_to_start_over() {
    let (_tmp, paths) = home();
    ensure_ca(&paths, "test", now()).unwrap();
    let good_key = fs::read(paths.ca_key()).unwrap();
    fs::write(paths.ca_cert(), "not a certificate").unwrap();
    assert!(matches!(
        ensure_ca(&paths, "test", now()).unwrap_err(),
        CaError::Invalid(_)
    ));
    assert_eq!(fs::read(paths.ca_key()).unwrap(), good_key);

    ensure_ca(&paths, "test", now()).unwrap_err();
    fs::write(paths.ca_key(), "nor a key").unwrap();
    assert!(matches!(
        load_ca(&paths, now()).unwrap_err(),
        CaError::Invalid(_)
    ));
}

#[test]
fn a_certificate_that_is_not_a_ca_is_refused() {
    let (_tmp, paths) = home();
    fs::create_dir_all(paths.certs_dir()).unwrap();
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["x.test".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    fs::write(paths.ca_cert(), leaf.pem()).unwrap();
    fs::write(paths.ca_key(), key.serialize_pem()).unwrap();
    assert!(matches!(
        ensure_ca(&paths, "test", now()).unwrap_err(),
        CaError::NotACa
    ));
}

#[test]
fn an_expired_ca_is_reported() {
    let (_tmp, paths) = home();
    ensure_ca(&paths, "test", now()).unwrap();
    let in_eleven_years = now() + StdDuration::from_secs(11 * 365 * 86_400);
    assert!(matches!(
        load_ca(&paths, in_eleven_years).unwrap_err(),
        CaError::Expired
    ));
    // Still fine a day before it ends.
    let before_the_end = now() + StdDuration::from_secs(3640 * 86_400);
    assert!(load_ca(&paths, before_the_end).unwrap().is_some());
}

#[test]
fn replacing_needs_to_be_asked_for() {
    let (_tmp, paths, old) = made("test");
    assert!(matches!(
        create_ca(&paths, "test", now(), false).unwrap_err(),
        CaError::AlreadyExists
    ));
    assert_eq!(
        load_ca(&paths, now()).unwrap().unwrap().info(),
        old.info(),
        "the refusal changed nothing"
    );

    let new = create_ca(&paths, "internal", now(), true).unwrap();
    assert_ne!(new.info().fingerprint, old.info().fingerprint);
    assert!(new.covers("internal") && !old.covers("internal"));
    assert_eq!(load_ca(&paths, now()).unwrap().unwrap().info(), new.info());
}

#[test]
fn the_debug_text_never_has_the_key() {
    let (_tmp, _paths, ca) = made("test");
    let text = format!("{ca:?}");
    assert!(text.contains("Crisol Local CA"));
    assert!(!text.contains("PRIVATE") && !text.contains("key"), "{text}");
}
