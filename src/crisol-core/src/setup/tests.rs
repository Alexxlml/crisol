use super::*;

const PROGRAM: &[u8] = b"MZ pretend this is crisol-elevate.exe";

#[test]
fn the_manifest_round_trips_and_names_the_hash_of_the_program() {
    let manifest = Manifest::of("0.1.0", PROGRAM);
    assert_eq!(manifest.sha256.len(), 64);
    assert_eq!(manifest.sha256, sha256_hex(PROGRAM));
    let text = manifest.to_text();
    assert!(text.starts_with("# Written by the installer"));
    assert_eq!(Manifest::parse(&text).unwrap(), manifest);
    // Upper-case hex and spacing are read the same.
    let loose = format!(
        "version=0.1.0\n  sha256 =  {}\n",
        manifest.sha256.to_uppercase()
    );
    assert_eq!(Manifest::parse(&loose).unwrap(), manifest);
}

#[test]
fn a_manifest_that_cannot_be_understood_vouches_for_nothing() {
    let good = sha256_hex(b"x");
    for bad in [
        "",
        "version = 0.1.0\n",
        &format!("sha256 = {good}\n"),
        &format!("version = 0.1.0\nsha256 = {}\n", &good[..63]),
        &format!("version = 0.1.0\nsha256 = {}z\n", &good[..63]),
        &format!("version = 0.1.0\nsha256 = {good}\nextra = 1\n"),
        "just words\n",
    ] {
        assert!(Manifest::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_helper_is_accepted_only_if_the_manifest_hash_and_version_all_agree() {
    let text = Manifest::of("0.1.0", PROGRAM).to_text();
    assert_eq!(check(Some(&text), PROGRAM, "0.1.0"), Ok(()));

    assert_eq!(
        check(None, PROGRAM, "0.1.0"),
        Err(HelperProblem::NoManifest)
    );
    assert!(matches!(
        check(Some("garbage"), PROGRAM, "0.1.0"),
        Err(HelperProblem::DamagedManifest(_))
    ));
    // The program is not the one that was installed (swapped, or damaged).
    let error = check(Some(&text), b"MZ something else", "0.1.0").unwrap_err();
    assert!(matches!(&error, HelperProblem::Changed { recorded, actual } if recorded != actual));
    // Installed by another version of Crisol.
    let error = check(Some(&text), PROGRAM, "0.2.0").unwrap_err();
    assert_eq!(
        error,
        HelperProblem::OtherVersion {
            installed: "0.1.0".to_owned(),
            expected: "0.2.0".to_owned()
        }
    );
    for problem in [
        HelperProblem::NoManifest,
        error,
        HelperProblem::DamagedManifest("x".to_owned()),
        HelperProblem::Changed {
            recorded: "a".to_owned(),
            actual: "b".to_owned(),
        },
    ] {
        let text = problem.to_string();
        assert!(text.contains("crisol system install"), "{text}");
        assert!(!text.contains("  "), "{text}");
    }
}

#[test]
fn the_setup_operations_take_one_valid_suffix_and_nothing_else() {
    let parse = |args: &[&str]| SetupOp::parse(args);
    assert_eq!(
        parse(&["install-self", "test"]).unwrap().unwrap(),
        SetupOp::InstallSelf {
            suffix: "test".to_owned()
        }
    );
    assert_eq!(
        parse(&["uninstall-self", ".Test"]).unwrap().unwrap(),
        SetupOp::UninstallSelf {
            suffix: "test".to_owned()
        }
    );
    // Not a setup operation: the caller goes on to the everyday ones.
    assert!(parse(&["nrpt-add", "test", "127.77.0.53"]).is_none());
    assert!(parse(&[]).is_none());
    // A setup operation with something wrong in it is an error, not "someone else's".
    for bad in [
        vec!["install-self"],
        vec!["install-self", "test", "extra"],
        vec!["uninstall-self"],
        vec!["install-self", "com"],
        vec!["install-self", "te st"],
        vec!["install-self", "test; calc"],
    ] {
        assert!(parse(&bad).unwrap().is_err(), "{bad:?}");
    }
    for op in [
        SetupOp::InstallSelf {
            suffix: "test".to_owned(),
        },
        SetupOp::UninstallSelf {
            suffix: "localhost".to_owned(),
        },
    ] {
        assert_eq!(SetupOp::parse(&op.to_args()).unwrap().unwrap(), op);
    }
}

#[test]
fn the_fake_setup_records_what_was_asked() {
    let setup = FakeSetup::new();
    setup.install("test").unwrap();
    setup.uninstall("test").unwrap();
    assert_eq!(setup.calls(), ["install test", "uninstall test"]);
}
