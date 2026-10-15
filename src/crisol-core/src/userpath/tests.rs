use std::collections::BTreeSet;

use super::fake::FakeUserPath;
use super::*;

const SHIMS: &str = r"C:\Crisol\shims";

fn shims() -> &'static Path {
    Path::new(SHIMS)
}

fn text(fake: &FakeUserPath) -> String {
    fake.user().map(|v| v.text).unwrap_or_default()
}

#[test]
fn the_entry_goes_first_and_the_rest_is_untouched() {
    let fake = FakeUserPath::with_user(r"C:\Tools;%USERPROFILE%\bin;D:\x");
    assert_eq!(add_shims(&fake, shims()).unwrap(), PathChange::Added);
    assert_eq!(
        text(&fake),
        r"C:\Crisol\shims;C:\Tools;%USERPROFILE%\bin;D:\x"
    );
    assert_eq!(fake.writes(), 1);
    assert_eq!(fake.notifications(), 1);
}

#[test]
fn adding_twice_writes_and_announces_once() {
    let fake = FakeUserPath::with_user(r"C:\Tools");
    add_shims(&fake, shims()).unwrap();
    assert_eq!(
        add_shims(&fake, shims()).unwrap(),
        PathChange::AlreadyPresent
    );
    assert_eq!((fake.writes(), fake.notifications()), (1, 1));
}

#[test]
fn the_same_folder_spelt_differently_counts_as_present() {
    for spelling in [
        r"c:\crisol\SHIMS",
        r"C:\Crisol\shims\",
        r"C:/Crisol/shims",
        r#""C:\Crisol\shims""#,
        r" C:\Crisol\shims ",
    ] {
        let fake = FakeUserPath::with_user(&format!(r"C:\Tools;{spelling}"));
        assert_eq!(
            add_shims(&fake, shims()).unwrap(),
            PathChange::AlreadyPresent,
            "{spelling}"
        );
        assert_eq!(fake.writes(), 0, "{spelling}");
    }
    // A different folder with a similar name is a different folder.
    let fake = FakeUserPath::with_user(r"C:\Crisol\shims2;C:\Crisol");
    assert_eq!(add_shims(&fake, shims()).unwrap(), PathChange::Added);
}

#[test]
fn an_entry_written_with_a_variable_is_recognised_by_its_expansion() {
    let fake = FakeUserPath::with_user(r"%CRISOL%\shims");
    fake.variable("CRISOL", r"C:\Crisol");
    assert_eq!(
        add_shims(&fake, shims()).unwrap(),
        PathChange::AlreadyPresent
    );
    assert_eq!(remove_shims(&fake, shims()).unwrap(), PathChange::Removed);
    assert_eq!(fake.user(), None, "and removing it empties the value");
}

#[test]
fn a_user_without_a_path_gets_one_that_can_hold_variables() {
    let fake = FakeUserPath::new();
    assert_eq!(add_shims(&fake, shims()).unwrap(), PathChange::Added);
    assert_eq!(
        fake.user(),
        Some(PathValue {
            text: SHIMS.to_owned(),
            expandable: true
        })
    );
}

#[test]
fn the_kind_of_value_is_kept() {
    for expandable in [true, false] {
        let fake = FakeUserPath::new();
        fake.set_user(Some(PathValue {
            text: r"C:\Tools".to_owned(),
            expandable,
        }));
        add_shims(&fake, shims()).unwrap();
        assert_eq!(fake.user().unwrap().expandable, expandable);
        remove_shims(&fake, shims()).unwrap();
        assert_eq!(fake.user().unwrap().expandable, expandable);
    }
}

#[test]
fn adding_and_then_removing_gives_back_exactly_the_original_value() {
    // The acceptance of F4.4: the value ends identical to how it started, whatever odd
    // things it had.
    for original in [
        r"C:\Tools",
        r"C:\Tools;D:\More",
        r"C:\Tools;",
        r";C:\Tools",
        r"C:\Tools;;D:\More",
        r"%USERPROFILE%\bin;%SystemRoot%\system32;C:\Program Files\App\",
        r"C:\Tools;c:\tools;C:\TOOLS\",
        r"  C:\Tools ; D:\More  ",
        r"C:\Crisol\shims2",
        "C:\\ünïcödé;D:\\日本語",
    ] {
        let fake = FakeUserPath::with_user(original);
        assert_eq!(add_shims(&fake, shims()).unwrap(), PathChange::Added);
        assert_eq!(remove_shims(&fake, shims()).unwrap(), PathChange::Removed);
        assert_eq!(text(&fake), original, "starting from {original:?}");
        assert!(fake.user().unwrap().expandable);
    }
}

#[test]
fn a_user_who_had_no_path_has_none_again_after_removing() {
    let fake = FakeUserPath::new();
    add_shims(&fake, shims()).unwrap();
    remove_shims(&fake, shims()).unwrap();
    assert_eq!(fake.user(), None);
}

#[test]
fn removing_takes_every_copy_of_the_entry_and_nothing_else() {
    let fake =
        FakeUserPath::with_user(r"C:\Crisol\shims;C:\Tools;c:\crisol\shims\;D:\Crisol\shims");
    assert_eq!(remove_shims(&fake, shims()).unwrap(), PathChange::Removed);
    assert_eq!(text(&fake), r"C:\Tools;D:\Crisol\shims");
}

#[test]
fn removing_what_is_not_there_changes_nothing() {
    let fake = FakeUserPath::with_user(r"C:\Tools");
    assert_eq!(
        remove_shims(&fake, shims()).unwrap(),
        PathChange::NotPresent
    );
    assert_eq!(
        remove_shims(&FakeUserPath::new(), shims()).unwrap(),
        PathChange::NotPresent
    );
    assert_eq!((fake.writes(), fake.notifications()), (0, 0));
}

#[test]
fn a_failed_write_is_an_error_and_announces_nothing() {
    let fake = FakeUserPath::with_user(r"C:\Tools");
    fake.fail_writes();
    let error = add_shims(&fake, shims()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fake.notifications(), 0);
    assert_eq!(text(&fake), r"C:\Tools", "the value is as it was");
}

#[test]
fn it_says_whether_the_shims_are_in_the_path() {
    let fake = FakeUserPath::with_user(r"C:\Tools");
    assert!(!shims_in_path(&fake, shims()).unwrap());
    add_shims(&fake, shims()).unwrap();
    assert!(shims_in_path(&fake, shims()).unwrap());
}

// ---- who would win --------------------------------------------------------------------

fn files(present: &[&str]) -> impl Fn(&Path) -> bool {
    let set: BTreeSet<String> = present.iter().map(|p| p.to_lowercase()).collect();
    move |path: &Path| set.contains(&path.to_string_lossy().to_lowercase())
}

#[test]
fn another_php_in_the_user_path_loses_once_the_shims_are_first() {
    let fake = FakeUserPath::with_user(r"C:\laragon\bin\php\php-8.3;C:\Tools");
    let exists = files(&[r"C:\laragon\bin\php\php-8.3\php.exe"]);

    // Not added yet: after `add_shims` they would be first, so nothing wins over them.
    assert_eq!(shadowing(&fake, shims(), &exists).unwrap(), []);
    add_shims(&fake, shims()).unwrap();
    assert_eq!(shadowing(&fake, shims(), &exists).unwrap(), []);
}

#[test]
fn a_php_in_front_of_the_shims_is_reported_with_the_tool_it_belongs_to() {
    // The user put the shims at the end, behind Herd.
    let fake = FakeUserPath::with_user(r"C:\Users\me\.config\herd\bin;C:\Crisol\shims");
    let exists = files(&[r"C:\Users\me\.config\herd\bin\php.bat"]);
    let shadows = shadowing(&fake, shims(), &exists).unwrap();
    assert_eq!(
        shadows,
        [Shadow {
            program: "php",
            found: PathBuf::from(r"C:\Users\me\.config\herd\bin\php.bat"),
            suite: Some("Herd"),
            in_system_path: false,
        }]
    );
}

#[test]
fn the_machine_path_comes_first_and_cannot_be_beaten() {
    let fake = FakeUserPath::with_user(r"C:\Crisol\shims");
    fake.set_system(r"C:\Windows;C:\xampp\php;C:\Program Files\nodejs");
    let exists = files(&[
        r"C:\xampp\php\php.exe",
        r"C:\Program Files\nodejs\node.exe",
        r"C:\Program Files\nodejs\npm.cmd",
    ]);
    let shadows = shadowing(&fake, shims(), &exists).unwrap();
    let names: Vec<&str> = shadows.iter().map(|s| s.program).collect();
    assert_eq!(names, ["php", "node", "npm"]);
    assert!(shadows.iter().all(|s| s.in_system_path));
    assert_eq!(shadows[0].suite, Some("XAMPP"));
    assert_eq!(shadows[1].suite, None);
}

#[test]
fn variables_in_path_entries_are_expanded_before_looking() {
    let fake = FakeUserPath::with_user(r"%LOCALAPPDATA%\Programs\php;C:\Crisol\shims");
    fake.variable("LOCALAPPDATA", r"C:\Users\me\AppData\Local");
    let exists = files(&[r"C:\Users\me\AppData\Local\Programs\php\php.exe"]);
    assert_eq!(shadowing(&fake, shims(), &exists).unwrap().len(), 1);
}

#[test]
fn nothing_else_installed_means_nothing_to_report() {
    let fake = FakeUserPath::with_user(r"C:\Tools;C:\Crisol\shims");
    fake.set_system(r"C:\Windows");
    assert_eq!(shadowing(&fake, shims(), &files(&[])).unwrap(), []);
    assert_eq!(
        shadowing(&FakeUserPath::new(), shims(), &files(&[])).unwrap(),
        []
    );
}
