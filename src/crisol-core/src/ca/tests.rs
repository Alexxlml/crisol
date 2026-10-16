use std::fs;
use std::time::Duration as StdDuration;

use tempfile::TempDir;
use x509_parser::extensions::{GeneralName, ParsedExtension};

use x509_parser::prelude::{FromDer, X509Certificate};

use super::*;
use crate::paths::fake::FakePrivateDirs;

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
    let ca = ensure_ca(&paths, &FakePrivateDirs::new(), tld, now())
        .unwrap()
        .ca;
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
    let first = ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
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

    let second = ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
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
    let made = ensure_ca(&paths, &FakePrivateDirs::new(), "internal", now())
        .unwrap()
        .ca;
    let loaded = load_ca(&paths, &FakePrivateDirs::new(), now())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.info(), made.info());
    assert_eq!(loaded.cert_der(), made.cert_der());
    assert_eq!(loaded.key().serialize_der(), made.key().serialize_der());
}

#[test]
fn without_files_there_is_no_ca() {
    let (_tmp, paths) = home();
    assert!(load_ca(&paths, &FakePrivateDirs::new(), now())
        .unwrap()
        .is_none());
    assert!(!paths.certs_dir().exists(), "reading creates nothing");
}

#[test]
fn half_a_ca_is_an_error_and_nothing_is_overwritten() {
    for (keep, other, present, missing) in [
        ("ca.pem", "ca.key", "ca.pem", "ca.key"),
        ("ca.key", "ca.pem", "ca.key", "ca.pem"),
    ] {
        let (_tmp, paths) = home();
        ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
        let kept = paths.certs_dir().join(keep);
        let before = fs::read(&kept).unwrap();
        fs::remove_file(paths.certs_dir().join(other)).unwrap();

        let error = ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap_err();
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
    ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
    let stranger = KeyPair::generate().unwrap();
    fs::write(paths.ca_key(), stranger.serialize_pem()).unwrap();
    assert!(matches!(
        ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap_err(),
        CaError::KeyMismatch
    ));
}

#[test]
fn garbage_is_an_error_not_a_reason_to_start_over() {
    let (_tmp, paths) = home();
    ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
    let good_key = fs::read(paths.ca_key()).unwrap();
    fs::write(paths.ca_cert(), "not a certificate").unwrap();
    assert!(matches!(
        ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap_err(),
        CaError::Invalid(_)
    ));
    assert_eq!(fs::read(paths.ca_key()).unwrap(), good_key);

    ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap_err();
    fs::write(paths.ca_key(), "nor a key").unwrap();
    assert!(matches!(
        load_ca(&paths, &FakePrivateDirs::new(), now()).unwrap_err(),
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
        ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap_err(),
        CaError::NotACa
    ));
}

#[test]
fn an_expired_ca_is_reported() {
    let (_tmp, paths) = home();
    ensure_ca(&paths, &FakePrivateDirs::new(), "test", now()).unwrap();
    let in_eleven_years = now() + StdDuration::from_secs(11 * 365 * 86_400);
    assert!(matches!(
        load_ca(&paths, &FakePrivateDirs::new(), in_eleven_years).unwrap_err(),
        CaError::Expired
    ));
    // Still fine a day before it ends.
    let before_the_end = now() + StdDuration::from_secs(3640 * 86_400);
    assert!(load_ca(&paths, &FakePrivateDirs::new(), before_the_end)
        .unwrap()
        .is_some());
}

#[test]
fn replacing_needs_to_be_asked_for() {
    let (_tmp, paths, old) = made("test");
    assert!(matches!(
        create_ca(&paths, &FakePrivateDirs::new(), "test", now(), false).unwrap_err(),
        CaError::AlreadyExists
    ));
    assert_eq!(
        load_ca(&paths, &FakePrivateDirs::new(), now())
            .unwrap()
            .unwrap()
            .info(),
        old.info(),
        "the refusal changed nothing"
    );

    let new = create_ca(&paths, &FakePrivateDirs::new(), "internal", now(), true).unwrap();
    assert_ne!(new.info().fingerprint, old.info().fingerprint);
    assert!(new.covers("internal") && !old.covers("internal"));
    assert_eq!(
        load_ca(&paths, &FakePrivateDirs::new(), now())
            .unwrap()
            .unwrap()
            .info(),
        new.info()
    );
}

#[test]
fn the_debug_text_never_has_the_key() {
    let (_tmp, _paths, ca) = made("test");
    let text = format!("{ca:?}");
    assert!(text.contains("Crisol Local CA"));
    assert!(!text.contains("PRIVATE") && !text.contains("key"), "{text}");
}

// ---- a CA that was swapped or never was ours (review of phase 6, finding 1) ----------------

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, CustomExtension, DistinguishedName, DnType,
    GeneralSubtree, IsCa, KeyUsagePurpose, NameConstraints,
};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

/// The parameters of a CA that passes every check; a test breaks one thing.
fn good_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Crisol Local CA 000000");
    name.push(DnType::OrganizationName, "Crisol");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName("test".to_owned()),
            GeneralSubtree::DnsName("localhost".to_owned()),
        ],
        excluded_subtrees: vec![],
    });
    params
}

/// Writes a CA pair, as another account that can write in `certs/` would, and says what
/// `load_ca` makes of it.
fn load_planted(params: &CertificateParams) -> Result<Option<LocalCa>, CaError> {
    let (_tmp, paths) = home();
    fs::create_dir_all(paths.certs_dir()).unwrap();
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    fs::write(paths.ca_cert(), cert.pem()).unwrap();
    fs::write(paths.ca_key(), key.serialize_pem()).unwrap();
    load_ca(&paths, &FakePrivateDirs::new(), now())
}

fn refused(result: Result<Option<LocalCa>, CaError>, containing: &str) {
    match result {
        Err(CaError::Invalid(why)) => assert!(why.contains(containing), "{why}"),
        Err(other) => assert!(other.to_string().contains(containing), "{other}"),
        Ok(_) => panic!("a CA that should have been refused was accepted ({containing})"),
    }
}

/// `NameConstraints { permitted = { dNSName "test" } }` written by hand (DER).
fn raw_constraints(critical: bool) -> CustomExtension {
    let content = vec![
        0x30, 0x0a, 0xa0, 0x08, 0x30, 0x06, 0x82, 0x04, 0x74, 0x65, 0x73, 0x74,
    ];
    let mut extension = CustomExtension::from_oid_content(&[2, 5, 29, 30], content);
    extension.set_criticality(critical);
    extension
}

#[test]
fn the_parameters_the_tests_start_from_are_accepted() {
    // So that the refusals below are about the one thing each test breaks.
    let ca = load_planted(&good_params()).unwrap().unwrap();
    assert_eq!(ca.info().permitted_dns, ["test", "localhost"]);
}

#[test]
fn a_ca_without_name_constraints_is_refused() {
    let mut params = good_params();
    params.name_constraints = None;
    refused(load_planted(&params), "no name constraints");
}

#[test]
fn a_ca_with_a_subtree_too_many_is_refused() {
    for extra in ["com", "example.com", "dev", "bank.test.com"] {
        let mut params = good_params();
        let constraints = params.name_constraints.as_mut().unwrap();
        constraints
            .permitted_subtrees
            .push(GeneralSubtree::DnsName(extra.to_owned()));
        refused(load_planted(&params), "not a name reserved for local use");
    }
    // Other kinds of name, and addresses beyond loopback.
    let mut email = good_params();
    email
        .name_constraints
        .as_mut()
        .unwrap()
        .permitted_subtrees
        .push(GeneralSubtree::Rfc822Name("test".to_owned()));
    refused(load_planted(&email), "kind of name");
    for (address, prefix) in [([10, 0, 0, 0], 8), ([127, 0, 0, 0], 0)] {
        let mut wide = good_params();
        wide.name_constraints
            .as_mut()
            .unwrap()
            .permitted_subtrees
            .push(GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
                IpAddr::V4(Ipv4Addr::from(address)),
                prefix,
            )));
        refused(load_planted(&wide), "not local");
    }
}

#[test]
fn a_ca_whose_name_constraints_are_not_critical_is_refused() {
    let mut params = good_params();
    params.name_constraints = None;
    params.custom_extensions.push(raw_constraints(false));
    refused(load_planted(&params), "not critical");

    // The same extension marked critical is accepted: the refusal above was about that.
    let mut critical = good_params();
    critical.name_constraints = None;
    critical.custom_extensions.push(raw_constraints(true));
    assert!(load_planted(&critical).unwrap().is_some());
}

#[test]
fn a_ca_that_can_sign_other_authorities_or_not_sign_at_all_is_refused() {
    let mut deep = good_params();
    deep.is_ca = IsCa::Ca(BasicConstraints::Constrained(1));
    refused(load_planted(&deep), "path length");
    let mut unlimited = good_params();
    unlimited.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    refused(load_planted(&unlimited), "path length");
    let mut mute = good_params();
    mute.key_usages = vec![KeyUsagePurpose::CrlSign];
    refused(load_planted(&mute), "keyCertSign");
}

#[test]
fn a_ca_issued_by_somebody_else_is_refused() {
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    let real = ensure_ca(&paths, &dirs, "test", now()).unwrap().ca;
    // A CA with perfect parameters, but signed by the real one rather than by itself.
    let key = KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_pem(real.cert_pem(), real.key()).unwrap();
    let cert = good_params().signed_by(&key, &issuer).unwrap();
    fs::write(paths.ca_cert(), cert.pem()).unwrap();
    fs::write(paths.ca_key(), key.serialize_pem()).unwrap();
    refused(load_ca(&paths, &dirs, now()), "self-signed");
}

#[test]
fn a_ca_made_by_crisol_passes_its_own_checks_for_every_special_use_tld() {
    for tld in [
        "test",
        "localhost",
        "example",
        "invalid",
        "local",
        "internal",
        "home.arpa",
        "dev.test",
        "a.b.local",
    ] {
        let (_tmp, paths) = home();
        let dirs = FakePrivateDirs::new();
        let made = ensure_ca(&paths, &dirs, tld, now()).unwrap();
        assert!(made.created, "{tld}");
        let again = ensure_ca(&paths, &dirs, tld, now()).unwrap();
        assert!(!again.created && again.ca.covers(tld), "{tld}");
    }
}

#[test]
fn which_names_are_special_use() {
    for yes in [
        "test",
        "TEST",
        "dev.test",
        "localhost",
        "a.localhost",
        "example",
        "invalid",
        "local",
        "internal",
        "home.arpa",
        "x.home.arpa",
        ".test.",
    ] {
        assert!(is_special_use(yes), "{yes}");
    }
    for no in [
        "",
        "dev",
        "app",
        "com",
        "arpa",
        "mytest",
        "test.com",
        "example.com",
        "lan",
        "localdomain",
    ] {
        assert!(!is_special_use(no), "{no}");
    }
}

// ---- who can write in certs/ ---------------------------------------------------------------

#[test]
fn certs_is_made_private_even_when_the_home_is_new_or_was_not_made_private() {
    // A home that does not exist: Crisol makes it, and certs/, private.
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    ensure_ca(&paths, &dirs, "test", now()).unwrap();
    assert_eq!(
        dirs.created(),
        [paths.home().to_path_buf(), paths.certs_dir()]
    );

    // A home that already exists (made some other way) is left as it is; certs/ is private.
    let (_tmp, paths) = home();
    fs::create_dir_all(paths.home()).unwrap();
    let dirs = FakePrivateDirs::new();
    ensure_ca(&paths, &dirs, "test", now()).unwrap();
    assert_eq!(dirs.created(), [paths.certs_dir()]);
}

#[test]
fn a_certs_folder_open_to_other_accounts_stops_everything() {
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    ensure_ca(&paths, &dirs, "test", now()).unwrap();

    dirs.expose(&paths.certs_dir());
    let error = load_ca(&paths, &dirs, now()).err().unwrap();
    assert!(
        matches!(error, CaError::Exposed { ref path } if *path == paths.certs_dir()),
        "{error}"
    );
    let text = error.to_string();
    assert!(
        text.contains("other accounts") && text.contains("icacls"),
        "{text}"
    );
    assert!(ensure_ca(&paths, &dirs, "test", now()).is_err());
    assert!(
        create_ca(&paths, &dirs, "test", now(), true).is_err(),
        "not even to replace it"
    );

    // Closing it again is all it takes.
    dirs.close(&paths.certs_dir());
    assert!(load_ca(&paths, &dirs, now()).unwrap().is_some());
}

#[test]
fn a_single_open_file_is_enough_to_refuse() {
    for file in ["ca.key", "ca.pem"] {
        let (_tmp, paths) = home();
        let dirs = FakePrivateDirs::new();
        ensure_ca(&paths, &dirs, "test", now()).unwrap();
        dirs.expose(&paths.certs_dir().join(file));
        let error = load_ca(&paths, &dirs, now()).err().unwrap();
        assert!(
            matches!(error, CaError::Exposed { ref path } if path.ends_with(file)),
            "{file}: {error}"
        );
    }
}

#[test]
fn a_certs_folder_that_exists_and_is_open_gets_no_ca_written_into_it() {
    let (_tmp, paths) = home();
    fs::create_dir_all(paths.certs_dir()).unwrap();
    let dirs = FakePrivateDirs::new();
    dirs.expose(&paths.certs_dir());
    assert!(matches!(
        ensure_ca(&paths, &dirs, "test", now()).err().unwrap(),
        CaError::Exposed { .. }
    ));
    assert!(!paths.ca_key().exists() && !paths.ca_cert().exists());
}

#[test]
fn nothing_can_be_trusted_when_certs_is_open() {
    use crate::trust::fake::FakeTrustStore;
    use crate::trust::trust_ca;
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    let store = FakeTrustStore::new();
    ensure_ca(&paths, &dirs, "test", now()).unwrap();
    dirs.expose(&paths.certs_dir());
    // To trust a CA one has to hold a `LocalCa`, and it cannot be loaded now.
    match load_ca(&paths, &dirs, now()) {
        Ok(Some(loaded)) => {
            trust_ca(&store, &loaded).unwrap();
        }
        Ok(None) => {}
        Err(error) => assert!(matches!(error, CaError::Exposed { .. })),
    }
    assert_eq!(store.changes(), (0, 0));
}

// ---- the copy Node reads -------------------------------------------------------------------

#[test]
fn node_gets_a_copy_of_the_validated_ca_in_a_private_folder() {
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    let ca = ensure_ca(&paths, &dirs, "test", now()).unwrap().ca;
    let path = write_node_ca(&paths, &dirs, &ca).unwrap();
    assert_eq!(path, paths.node_ca().unwrap());
    let shown = path.to_string_lossy().replace('\\', "/");
    assert!(shown.ends_with("etc/node/ca.pem"), "{shown}");
    assert_eq!(fs::read_to_string(&path).unwrap(), ca.cert_pem());
    assert_eq!(
        dirs.created().last().map(PathBuf::as_path),
        path.parent(),
        "the folder was made private"
    );

    // Again: nothing is written.
    let stamp = fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    write_node_ca(&paths, &dirs, &ca).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), stamp);

    // A copy that was tampered with is put back.
    fs::write(&path, "tampered").unwrap();
    write_node_ca(&paths, &dirs, &ca).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), ca.cert_pem());
}

// ---- only local TLDs (review of phase 6, finding 2) ------------------------------------

#[test]
fn no_ca_is_made_for_a_tld_that_could_be_a_real_domain() {
    for tld in ["dev", "app", "com", "io", "lan", "test.com", "arpa"] {
        let (_tmp, paths) = home();
        let dirs = FakePrivateDirs::new();
        let error = ensure_ca(&paths, &dirs, tld, now()).err().unwrap();
        assert!(
            matches!(error, CaError::TldNotSpecialUse(ref t) if t == tld),
            "{tld}: {error}"
        );
        let text = error.to_string();
        assert!(
            text.contains("served over HTTP") && text.contains(".test"),
            "{text}"
        );
        assert!(!paths.certs_dir().exists(), "{tld}: nothing was created");
        assert_eq!(dirs.created(), Vec::<PathBuf>::new());
        assert!(
            create_ca(&paths, &dirs, tld, now(), true).is_err(),
            "not even to replace"
        );
    }
}

#[test]
fn a_tld_under_a_special_use_name_is_fine() {
    let (_tmp, paths) = home();
    let made = ensure_ca(&paths, &FakePrivateDirs::new(), "dev.test", now()).unwrap();
    assert!(made.ca.covers("dev.test") && made.ca.covers("shop.dev.test"));
    assert!(!made.ca.covers("dev"));
}

// ---- review of phase 6, V1: the copy for Node is checked like certs/ -------------------

#[test]
fn node_gets_no_copy_when_its_folder_or_file_is_open_to_other_accounts() {
    let (_tmp, paths) = home();
    let dirs = FakePrivateDirs::new();
    let ca = ensure_ca(&paths, &dirs, "test", now()).unwrap().ca;
    let path = write_node_ca(&paths, &dirs, &ca).unwrap();
    let folder = path.parent().unwrap().to_path_buf();

    // The folder was opened (or somebody else made it): nothing is written, and what is there
    // is left alone.
    fs::write(&path, "planted by somebody else").unwrap();
    dirs.expose(&folder);
    let error = write_node_ca(&paths, &dirs, &ca).err().unwrap();
    assert!(
        matches!(error, CaError::Exposed { ref path } if *path == folder),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "planted by somebody else"
    );

    // Only the file open: the same.
    dirs.close(&folder);
    dirs.expose(&path);
    let error = write_node_ca(&paths, &dirs, &ca).err().unwrap();
    assert!(
        matches!(error, CaError::Exposed { path: ref p } if *p == path),
        "{error}"
    );

    // Closed again, the good copy goes back.
    dirs.close(&path);
    write_node_ca(&paths, &dirs, &ca).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), ca.cert_pem());
}

// ---- review of phase 6, V2: no stretches of spaces inside messages ---------------------

#[test]
fn no_error_message_has_two_spaces_in_a_row() {
    let errors = [
        CaError::Invalid("x".to_owned()),
        CaError::Incomplete {
            present: "ca.pem",
            missing: "ca.key",
        },
        CaError::KeyMismatch,
        CaError::NotACa,
        CaError::Expired,
        CaError::AlreadyExists,
        CaError::NotCovered("a.test".to_owned()),
        CaError::TldNotSpecialUse("dev".to_owned()),
        CaError::Exposed {
            path: PathBuf::from("C:/Crisol/certs"),
        },
    ];
    for error in errors {
        let text = error.to_string();
        assert!(!text.contains("  "), "{text}");
    }
    let text = CaError::TldNotSpecialUse("dev".to_owned()).to_string();
    assert!(text.contains("local, internal, home.arpa"), "{text}");
}
