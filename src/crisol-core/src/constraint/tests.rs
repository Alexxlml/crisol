use super::*;

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn ok(flavor: Flavor, constraint: &str, version: &str) -> bool {
    Constraint::parse(constraint, flavor)
        .unwrap_or_else(|e| panic!("{e}"))
        .matches(&v(version))
}

/// `(constraint, version, expected)` rows, checked in both flavors unless noted.
const SHARED: &[(&str, &str, bool)] = &[
    // caret
    ("^8.1", "8.1.0", true),
    ("^8.1", "8.4.26", true),
    ("^8.1", "9.0.0", false),
    ("^8.1", "8.0.30", false),
    ("^8.1.2", "8.1.1", false),
    ("^8.1.2", "8.9.0", true),
    ("^0.2.3", "0.2.9", true),
    ("^0.2.3", "0.3.0", false),
    ("^0.0.3", "0.0.3", true),
    ("^0.0.3", "0.0.4", false),
    ("^0", "0.9.9", true),
    ("^0", "1.0.0", false),
    // comparisons
    (">=8.2", "8.2.0", true),
    (">=8.2", "8.1.9", false),
    (">=8.2.0", "9.1.0", true),
    ("<8.3", "8.2.99", true),
    ("<8.3", "8.3.0", false),
    (">=7.4 <8.3", "8.2.5", true),
    (">=7.4 <8.3", "8.3.1", false),
    (">=7.4,<8.3", "7.4.0", true),
    (">= 7.4 , < 8.3", "8.0.0", true),
    ("!=8.2.1", "8.2.1", false),
    ("!=8.2.1", "8.2.2", true),
    ("=8.2.1", "8.2.1", true),
    ("8.2.1", "8.2.2", false),
    // wildcards and partial versions
    ("8.2.*", "8.2.7", true),
    ("8.2.*", "8.3.0", false),
    ("8.*", "8.9.9", true),
    ("8.x", "9.0.0", false),
    ("8.2", "8.2.14", true),
    ("8.2", "8.3.0", false),
    ("8", "8.0.1", true),
    ("*", "1.0.0", true),
    ("", "1.0.0", true),
    ("x", "0.0.1", true),
    ("v8.2.*", "8.2.0", true),
    // alternatives
    ("^8.1 || ^8.3", "8.2.0", true),
    ("^7.4 || ^8.1", "8.2.0", true),
    ("^7.4 || ^8.1", "9.0.0", false),
    ("^7.4||^8.1", "7.9.0", true),
    // hyphen ranges
    ("7.4 - 8.1", "8.1.9", true),
    ("7.4 - 8.1", "8.2.0", false),
    ("7.4.0 - 8.1.2", "8.1.3", false),
    ("7.4.0 - 8.1.2", "8.1.2", true),
    // stability flags are not versions
    ("^8.1@dev", "8.2.0", true),
    // pre-releases are never picked by a range
    ("^8.1", "8.5.0-rc1", false),
    (">=8.1", "9.0.0-beta1", false),
];

#[test]
fn both_flavors_agree_on_the_common_syntax() {
    for flavor in [Flavor::Composer, Flavor::Npm] {
        for (constraint, version, expected) in SHARED {
            assert_eq!(
                ok(flavor, constraint, version),
                *expected,
                "{flavor}: `{constraint}` against {version}"
            );
        }
    }
}

#[test]
fn the_tilde_differs_between_composer_and_npm() {
    // ~1.2: composer keeps the major, npm keeps the minor.
    assert!(ok(Flavor::Composer, "~8.2", "8.9.0"));
    assert!(!ok(Flavor::Npm, "~8.2", "8.9.0"));
    assert!(ok(Flavor::Npm, "~8.2", "8.2.9"));
    assert!(!ok(Flavor::Composer, "~8.2", "9.0.0"));
    // With three parts they agree.
    for flavor in [Flavor::Composer, Flavor::Npm] {
        assert!(ok(flavor, "~8.2.1", "8.2.9"), "{flavor}");
        assert!(!ok(flavor, "~8.2.1", "8.3.0"), "{flavor}");
        assert!(!ok(flavor, "~8.2.1", "8.2.0"), "{flavor}");
        assert!(ok(flavor, "~8", "8.9.0"), "{flavor}");
        assert!(!ok(flavor, "~8", "9.0.0"), "{flavor}");
    }
}

#[test]
fn partial_greater_than_and_at_most_differ() {
    // npm reads `>8.1` as "after the whole 8.1.x line", composer as "after 8.1.0".
    assert!(ok(Flavor::Composer, ">8.1", "8.1.5"));
    assert!(!ok(Flavor::Npm, ">8.1", "8.1.5"));
    assert!(ok(Flavor::Npm, ">8.1", "8.2.0"));
    assert!(ok(Flavor::Npm, "<=8.1", "8.1.5"));
    assert!(!ok(Flavor::Composer, "<=8.1", "8.1.5"));
}

#[test]
fn what_cannot_be_understood_is_an_error_that_names_the_text() {
    for bad in [
        "^^8",
        "~a.b",
        ">>8",
        "8.1.x.y.z.q-",
        "8.1 -",
        "- 8.1",
        "!=8.1",
        "^",
        "<",
        "8.1.beta",
    ] {
        let error = Constraint::parse(bad, Flavor::Composer).unwrap_err();
        assert_eq!(error.text, bad);
        assert!(error.to_string().contains(bad), "{error}");
    }
}

#[test]
fn constraints_from_real_projects_are_understood() {
    // Seen in the wild: Laravel, Symfony, WordPress plugins, Node tools.
    for (flavor, constraint) in [
        (Flavor::Composer, "^8.2"),
        (Flavor::Composer, ">=8.1.0"),
        (Flavor::Composer, "^7.4|^8.0"),
        (Flavor::Composer, ">=7.2.5 <8.4"),
        (Flavor::Composer, "~8.1.0"),
        (Flavor::Npm, ">=18.0.0"),
        (Flavor::Npm, "^18.17.0 || >=20.5.0"),
        (Flavor::Npm, ">=18 <23"),
        (Flavor::Npm, "20.x"),
        (Flavor::Npm, "22"),
    ] {
        assert!(
            Constraint::parse(constraint, flavor).is_ok(),
            "{constraint}"
        );
    }
}
