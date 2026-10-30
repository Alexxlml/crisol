use std::time::SystemTime;

use rcgen::{CertificateParams, KeyPair};
use tempfile::TempDir;

use super::fake::FakeTrustStore;
use super::*;
use crate::ca::{create_ca, ensure_ca};
use crate::paths::fake::FakePrivateDirs;
use crate::paths::CrisolPaths;

fn ca() -> (TempDir, CrisolPaths, LocalCa) {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let ca = ensure_ca(&paths, &FakePrivateDirs::new(), "test", SystemTime::now())
        .unwrap()
        .ca;
    (tmp, paths, ca)
}

/// Some unrelated self-signed certificate: what else a trust store holds.
fn stranger(name: &str) -> Vec<u8> {
    let key = KeyPair::generate().unwrap();
    CertificateParams::new(vec![name.to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap()
        .der()
        .to_vec()
}

#[test]
fn trusting_adds_exactly_the_ca_and_a_second_time_changes_nothing() {
    let (_tmp, paths, ca) = ca();
    let store = FakeTrustStore::new();
    assert_eq!(
        trust_ca(
            &store,
            &ca,
            &paths,
            &crate::paths::fake::FakePrivateDirs::new()
        )
        .unwrap(),
        AddOutcome::Added
    );
    assert!(store.contains(ca.cert_der()).unwrap());
    assert_eq!(store.certificates().unwrap(), [ca.cert_der().to_vec()]);
    assert_eq!(
        trust_ca(
            &store,
            &ca,
            &paths,
            &crate::paths::fake::FakePrivateDirs::new()
        )
        .unwrap(),
        AddOutcome::AlreadyThere
    );
    assert_eq!(store.changes(), (1, 0));
}

#[test]
fn a_refusal_in_the_system_dialog_is_an_outcome_not_an_error() {
    let (_tmp, paths, ca) = ca();
    let store = FakeTrustStore::new();
    store.decline_next();
    assert_eq!(
        trust_ca(
            &store,
            &ca,
            &paths,
            &crate::paths::fake::FakePrivateDirs::new()
        )
        .unwrap(),
        AddOutcome::Declined
    );
    assert!(!store.contains(ca.cert_der()).unwrap());
    assert_eq!(store.changes(), (0, 0));
    // Asking again, and saying yes this time, works.
    assert_eq!(
        trust_ca(
            &store,
            &ca,
            &paths,
            &crate::paths::fake::FakePrivateDirs::new()
        )
        .unwrap(),
        AddOutcome::Added
    );
}

#[test]
fn untrusting_removes_only_that_certificate() {
    let (_tmp, paths, ca) = ca();
    let other = stranger("corporate-proxy.example");
    let store = FakeTrustStore::with(&[&other]);
    trust_ca(
        &store,
        &ca,
        &paths,
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();

    assert!(untrust_ca(&store, &ca).unwrap());
    assert_eq!(
        store.certificates().unwrap(),
        std::slice::from_ref(&other),
        "the rest stays"
    );
    assert!(
        !untrust_ca(&store, &ca).unwrap(),
        "it was not there any more"
    );
    assert_eq!(store.certificates().unwrap(), [other]);
}

#[test]
fn a_store_that_fails_is_an_error_and_nothing_is_claimed() {
    let (_tmp, paths, ca) = ca();
    let store = FakeTrustStore::new();
    store.fail_with("the store is locked");
    assert!(trust_ca(
        &store,
        &ca,
        &paths,
        &crate::paths::fake::FakePrivateDirs::new()
    )
    .is_err());
    assert!(untrust_ca(&store, &ca).is_err());
    assert!(status(&store, &ca).is_err());
    store.recover();
    assert_eq!(store.changes(), (0, 0));
}

#[test]
fn the_status_says_whether_the_ca_is_trusted() {
    let (_tmp, paths, ca) = ca();
    let store = FakeTrustStore::with(&[&stranger("x.example")]);
    let before = status(&store, &ca).unwrap();
    assert_eq!(
        before,
        TrustStatus {
            trusted: false,
            stale: vec![]
        }
    );
    trust_ca(
        &store,
        &ca,
        &paths,
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    assert!(status(&store, &ca).unwrap().trusted);
}

#[test]
fn a_replaced_ca_stays_trusted_until_it_is_removed_and_is_found_by_name() {
    let (_tmp, paths, old) = ca();
    let store = FakeTrustStore::with(&[&stranger("corporate-proxy.example")]);
    trust_ca(
        &store,
        &old,
        &paths,
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();
    let old_name = old.info().common_name.clone();

    let new = create_ca(
        &paths,
        &FakePrivateDirs::new(),
        "test",
        SystemTime::now(),
        true,
    )
    .unwrap();
    trust_ca(
        &store,
        &new,
        &paths,
        &crate::paths::fake::FakePrivateDirs::new(),
    )
    .unwrap();

    let state = status(&store, &new).unwrap();
    assert!(state.trusted);
    assert_eq!(state.stale.len(), 1, "{state:?}");
    assert_eq!(state.stale[0].common_name, old_name);
    assert_eq!(state.stale[0].der, old.cert_der());

    // Asked to, it removes the old one and nothing else.
    let removed = remove_stale(&store, &new).unwrap();
    assert_eq!(removed.len(), 1);
    assert!(!store.contains(old.cert_der()).unwrap());
    assert!(store.contains(new.cert_der()).unwrap());
    assert_eq!(
        store.certificates().unwrap().len(),
        2,
        "the stranger and the new CA"
    );
    assert_eq!(remove_stale(&store, &new).unwrap(), vec![]);
}

#[test]
fn only_real_crisol_cas_are_ever_treated_as_ours() {
    let (_tmp, _paths, current) = ca();
    // A look-alike: a self-signed CA with the right name prefix but a different organization.
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut name = rcgen::DistinguishedName::new();
    name.push(rcgen::DnType::CommonName, "Crisol Local CA 000000");
    name.push(rcgen::DnType::OrganizationName, "Somebody Else");
    params.distinguished_name = name;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let lookalike = params.self_signed(&key).unwrap().der().to_vec();
    // A leaf (not a CA) with the right name and organization.
    let mut leaf_params = CertificateParams::new(vec!["x.test".to_owned()]).unwrap();
    let mut leaf_name = rcgen::DistinguishedName::new();
    leaf_name.push(rcgen::DnType::CommonName, "Crisol Local CA 111111");
    leaf_name.push(rcgen::DnType::OrganizationName, "Crisol");
    leaf_params.distinguished_name = leaf_name;
    let leaf = leaf_params
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap()
        .der()
        .to_vec();

    let store = FakeTrustStore::with(&[&lookalike, &leaf, &stranger("y.example")]);
    assert_eq!(stale_cas(&store, &current).unwrap(), vec![]);
    assert_eq!(remove_stale(&store, &current).unwrap(), vec![]);
    assert_eq!(
        store.certificates().unwrap().len(),
        3,
        "nothing was removed"
    );
}

// ---- `crisol trust` and `crisol untrust` (F11.1) ----------------------------------------------

#[test]
fn preparing_makes_the_ca_once_and_adds_nothing_to_the_store() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let dirs = FakePrivateDirs::new();
    let (_old_tmp, _, old) = ca();
    let store = FakeTrustStore::with(&[old.cert_der()]);
    let now = SystemTime::now();

    let first = prepare_trust(&store, &paths, &dirs, "test", now).unwrap();
    assert!(first.created && !first.already);
    assert_eq!(first.thumbprint, None, "the double shows no thumbprint");
    assert_eq!(first.stale, [old.info().common_name.clone()]);
    assert_eq!(
        store.changes(),
        (0, 0),
        "nothing is asked before the plan is shown"
    );

    trust_ca(&store, &first.ca, &paths, &dirs).unwrap();
    let again = prepare_trust(&store, &paths, &dirs, "test", now).unwrap();
    assert!(!again.created && again.already);
    assert_eq!(again.ca.cert_der(), first.ca.cert_der());
}

#[test]
fn preparing_refuses_an_open_home_and_a_public_suffix() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    paths.ensure_layout().unwrap();
    let dirs = FakePrivateDirs::new();
    let store = FakeTrustStore::new();
    let now = SystemTime::now();
    assert!(matches!(
        prepare_trust(&store, &paths, &dirs, "com", now),
        Err(TrustError::Ca(_))
    ));
    dirs.expose(paths.home());
    assert!(matches!(
        prepare_trust(&store, &paths, &dirs, "test", now),
        Err(TrustError::Home(_))
    ));
    assert!(!paths.ca_cert().exists());
}

#[test]
fn untrusting_takes_out_the_current_ca_or_only_the_older_ones() {
    let (_tmp, paths, current) = ca();
    let (_old_tmp, _, old) = ca();
    let other = stranger("bank.example.com");
    let store = FakeTrustStore::with(&[current.cert_der(), old.cert_der(), &other]);
    let dirs = FakePrivateDirs::new();
    let now = SystemTime::now();

    let stale = untrust_local(&store, &paths, &dirs, true, now).unwrap();
    assert_eq!(stale.removed, [old.info().common_name.clone()]);
    assert_eq!(stale.stale_left, 0);
    assert!(
        store.contains(current.cert_der()).unwrap(),
        "never the current one"
    );

    let plain = untrust_local(&store, &paths, &dirs, false, now).unwrap();
    assert_eq!(plain.removed, [current.info().common_name.clone()]);
    assert_eq!(
        plain.current.as_deref(),
        Some(current.info().common_name.as_str())
    );
    assert_eq!(
        store.certificates().unwrap(),
        std::slice::from_ref(&other),
        "nothing else is touched"
    );
    assert!(paths.ca_cert().is_file(), "certs/ stays");

    let nothing = untrust_local(&store, &paths, &dirs, false, now).unwrap();
    assert_eq!(nothing.removed, Vec::<String>::new());
}

#[test]
fn without_a_ca_untrust_removes_nothing_and_stale_removes_every_crisol_ca() {
    let tmp = TempDir::new().unwrap();
    let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
    let (_old_tmp, _, old) = ca();
    let store = FakeTrustStore::with(&[old.cert_der()]);
    let dirs = FakePrivateDirs::new();
    let now = SystemTime::now();
    let plain = untrust_local(&store, &paths, &dirs, false, now).unwrap();
    assert_eq!(
        (plain.removed.len(), plain.current, plain.stale_left),
        (0, None, 1)
    );
    let stale = untrust_local(&store, &paths, &dirs, true, now).unwrap();
    assert_eq!(stale.removed, [old.info().common_name.clone()]);
}
