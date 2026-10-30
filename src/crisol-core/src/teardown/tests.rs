use std::path::Path;
use std::time::SystemTime;

use tempfile::TempDir;

use super::*;
use crate::autostart::FakeAutostart;
use crate::ca::ensure_ca;
use crate::paths::fake::FakePrivateDirs;
use crate::setup::{FakeSetup, Leftovers};
use crate::shimsetup::path_add;
use crate::trust::fake::FakeTrustStore;
use crate::userpath::fake::FakeUserPath;

/// A home with its layout and a local CA, and a store that trusts that CA.
struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    autostart: FakeAutostart,
    user_path: FakeUserPath,
    trust: FakeTrustStore,
    ca_der: Vec<u8>,
}

impl World {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let ca = ensure_ca(&paths, &FakePrivateDirs::new(), "test", SystemTime::now())
            .unwrap()
            .ca;
        let ca_der = ca.cert_der().to_vec();
        Self {
            _tmp: tmp,
            paths,
            autostart: FakeAutostart::new(),
            user_path: FakeUserPath::with_user(r"C:\Tools"),
            trust: FakeTrustStore::with(&[&ca_der]),
            ca_der,
        }
    }

    /// Everything Crisol can change, changed.
    fn everything() -> Self {
        let world = Self::new();
        world.autostart.set(r#""C:\Crisol\crisold.exe""#).unwrap();
        path_add(&world.user_path, &world.paths).unwrap();
        world
    }

    fn system<'a>(&'a self, setup: &'a dyn SystemSetup, daemon_running: bool) -> System<'a> {
        System {
            paths: &self.paths,
            autostart: &self.autostart,
            user_path: &self.user_path,
            trust: Ok(&self.trust),
            setup,
            daemon_running,
            programs: Vec::new(),
            remove_programs: &|_| Ok(()),
        }
    }
}

fn ids(plan: &Plan) -> Vec<&'static str> {
    plan.steps.iter().map(Step::id).collect()
}

fn kinds(outcomes: &[Outcome]) -> Vec<(&'static str, OutcomeKind)> {
    outcomes.iter().map(|o| (o.step, o.kind)).collect()
}

#[test]
fn everything_crisol_changed_is_listed_and_then_undone() {
    let world = World::everything();
    let stranger = b"someone else's root".to_vec();
    world.trust.add(&stranger).unwrap();
    let setup = FakeSetup::new();
    let system = world.system(&setup, true);

    let plan = plan(&system, false);
    assert_eq!(
        ids(&plan),
        ["daemon", "autostart", "path", "ca", "helper", "clean"]
    );
    assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    assert!(
        plan.by_hand.is_empty(),
        "the helper does it: {:?}",
        plan.by_hand
    );
    assert!(plan.kept[0].contains("--purge"), "{:?}", plan.kept);
    for step in &plan.steps {
        assert_ne!(step.describe(), "");
    }
    // Looking changed nothing.
    assert!(world.autostart.current().unwrap().is_some());
    assert_eq!(setup.calls(), Vec::<String>::new());

    let mut stops = 0;
    let outcomes = run(&system, &plan, &mut || {
        stops += 1;
        Ok(())
    });
    assert_eq!(stops, 1);
    assert!(all_done(&outcomes), "{outcomes:?}");
    assert_eq!(world.autostart.current().unwrap(), None);
    assert!(
        !path_status(&world.user_path, &world.paths, &|_| false)
            .unwrap()
            .in_path
    );
    assert_eq!(world.trust.certificates().unwrap(), [stranger]);
    assert_eq!(
        setup.calls(),
        [format!("uninstall test {}", world.paths.home().display())]
    );
    assert!(world.paths.home().is_dir(), "the home is kept");

    // Again: nothing is left to do.
    let again = super::plan(&world.system(&setup, false), false);
    assert_eq!(ids(&again), ["helper"], "only the fake helper stays");
}

#[test]
fn every_crisol_ca_goes_the_current_one_and_the_older_ones() {
    let world = World::new();
    let (_tmp, _, older) = {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("old")).unwrap();
        let ca = ensure_ca(&paths, &FakePrivateDirs::new(), "test", SystemTime::now())
            .unwrap()
            .ca;
        (tmp, paths, ca)
    };
    world.trust.add(older.cert_der()).unwrap();
    let setup = FakeSetup::not_installed();
    let plan = plan(&world.system(&setup, false), false);
    assert_eq!(ids(&plan), ["ca", "ca", "clean"]);
}

#[test]
fn a_safe_uninstall_keeps_the_databases_their_passwords_the_settings_and_the_projects() {
    let world = World::new();
    let paths = &world.paths;
    let kept = [
        paths.config_file(),
        paths.sites_file(),
        paths.data_dir().join("mysql").join("8.4").join("ibdata1"),
        paths.secrets_dir().join("mysql.toml"),
        paths.www_dir().join("shop").join("index.php"),
        paths
            .etc_dir()
            .join("mysql")
            .join("overrides")
            .join("mine.ini"),
    ];
    let gone = [
        paths
            .bin_dir()
            .join("php")
            .join("8.4.26")
            .join("php-cgi.exe"),
        paths.logs_dir().join("nginx").join("runtime.log"),
        paths.cache_dir().join("npm").join("x"),
        paths.shims_dir().join("php.exe"),
        paths.state_dir().join("x"),
        paths.etc_dir().join("mysql").join("8.4").join("my.ini"),
        paths.etc_dir().join("nginx").join("nginx.conf"),
    ];
    for file in kept.iter().chain(&gone) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "x").unwrap();
    }
    let removed = std::sync::Mutex::new(Vec::new());
    let remove = |files: &[PathBuf]| {
        removed.lock().unwrap().extend(files.iter().cloned());
        Ok(())
    };
    let setup = FakeSetup::not_installed();
    let mut system = world.system(&setup, false);
    system.programs = vec![PathBuf::from(r"C:\Tools\crisol.exe")];
    system.remove_programs = &remove;

    let plan = plan(&system, false);
    assert_eq!(ids(&plan), ["ca", "clean", "programs"]);
    assert!(
        plan.kept[0].contains("databases (mysql)"),
        "{:?}",
        plan.kept
    );
    let outcomes = run(&system, &plan, &mut || unreachable!());
    assert!(all_done(&outcomes), "{outcomes:?}");
    for file in &kept {
        assert!(file.is_file(), "kept: {}", file.display());
    }
    for file in &gone {
        assert!(!file.exists(), "gone: {}", file.display());
    }
    assert!(!paths.bin_dir().exists() && !paths.certs_dir().exists());
    assert!(
        !paths.etc_dir().join("nginx").exists(),
        "an empty folder goes"
    );
    assert_eq!(
        *removed.lock().unwrap(),
        [PathBuf::from(r"C:\Tools\crisol.exe")]
    );

    // Again: there is nothing more to clean.
    assert_eq!(
        ids(&super::plan(&world.system(&setup, false), false)),
        Vec::<&str>::new()
    );
}

#[test]
fn after_a_no_nothing_is_deleted_and_the_programs_stay_to_run_it_again() {
    let world = World::new();
    world.trust.decline_removals();
    fs::create_dir_all(world.paths.bin_dir().join("php")).unwrap();
    let setup = FakeSetup::not_installed();
    let mut system = world.system(&setup, false);
    system.programs = vec![PathBuf::from(r"C:\Tools\crisol.exe")];
    system.remove_programs = &|_| panic!("the programs must stay");

    let plan = plan(&system, false);
    let outcomes = run(&system, &plan, &mut || unreachable!());
    assert_eq!(
        kinds(&outcomes),
        [
            ("ca", OutcomeKind::Declined),
            ("clean", OutcomeKind::Skipped),
            ("programs", OutcomeKind::Skipped),
        ]
    );
    assert!(world.paths.bin_dir().join("php").is_dir());
    assert!(world.paths.certs_dir().join("ca.pem").is_file());
}

#[test]
fn a_no_is_an_answer_the_rest_goes_on_and_the_home_is_not_deleted() {
    let world = World::everything();
    world.trust.decline_removals();
    let setup = FakeSetup::declining();
    let system = world.system(&setup, false);
    let plan = plan(&system, true);
    assert_eq!(ids(&plan), ["autostart", "path", "ca", "helper", "purge"]);

    let outcomes = run(&system, &plan, &mut || unreachable!());
    assert_eq!(
        kinds(&outcomes),
        [
            ("autostart", OutcomeKind::Done),
            ("path", OutcomeKind::Done),
            ("ca", OutcomeKind::Declined),
            ("helper", OutcomeKind::Declined),
            ("purge", OutcomeKind::Skipped),
        ]
    );
    assert!(!all_done(&outcomes));
    assert!(world.trust.contains(&world.ca_der).unwrap());
    assert!(world.paths.certs_dir().join("ca.pem").is_file());
    assert!(outcomes[3].message.contains("declined"), "{outcomes:?}");
}

#[test]
fn purge_deletes_what_crisol_keeps_in_the_home_and_says_what_is_lost() {
    let world = World::new();
    fs::create_dir_all(world.paths.data_dir().join("mysql").join("8.4")).unwrap();
    fs::create_dir_all(world.paths.www_dir().join("shop")).unwrap();
    fs::write(world.paths.config_file(), "").unwrap();
    let setup = FakeSetup::not_installed();
    let system = world.system(&setup, false);

    let plan = plan(&system, true);
    let notice = plan.purge.clone().unwrap();
    assert_eq!(notice.databases, ["mysql"]);
    assert_eq!(notice.projects, ["shop"]);
    assert_eq!(notice.foreign, Vec::<std::path::PathBuf>::new());
    assert_eq!(ids(&plan), ["ca", "purge"]);
    assert!(
        !plan.kept.iter().any(|k| k.contains("--purge")),
        "{:?}",
        plan.kept
    );

    let outcomes = run(&system, &plan, &mut || unreachable!());
    assert!(all_done(&outcomes), "{outcomes:?}");
    assert!(!world.paths.home().exists());
}

#[test]
fn a_home_with_anything_else_in_it_is_never_deleted() {
    let world = World::new();
    let theirs = world.paths.home().join("Documents");
    fs::create_dir(&theirs).unwrap();
    let setup = FakeSetup::not_installed();
    let system = world.system(&setup, false);

    let plan = plan(&system, true);
    assert_eq!(ids(&plan), ["ca"], "no purge step");
    assert_eq!(
        ids(&super::plan(&system, false)),
        ["ca"],
        "nor a safe clean: it deletes nothing either"
    );
    assert_eq!(
        plan.purge.as_ref().unwrap().foreign,
        std::slice::from_ref(&theirs)
    );
    assert!(
        plan.problems.iter().any(|p| p.contains("Documents")),
        "{:?}",
        plan.problems
    );
    run(&system, &plan, &mut || unreachable!());
    assert!(theirs.is_dir());
    assert!(world.paths.data_dir().is_dir());
}

#[test]
fn without_the_helper_what_needs_an_administrator_is_told_as_commands() {
    let world = World::new();
    let quiet = FakeSetup::not_installed();
    let plan_quiet = plan(&world.system(&quiet, false), false);
    assert_eq!(plan_quiet.by_hand.len(), 1, "{:?}", plan_quiet.by_hand);
    assert!(plan_quiet.by_hand[0].contains("Remove-MpPreference -ExclusionPath"));

    let left = FakeSetup::not_installed_with(Leftovers {
        nrpt_rule: true,
        hosts_block: Some(Path::new(r"C:\Windows\System32\drivers\etc\hosts").to_path_buf()),
    });
    let plan_left = plan(&world.system(&left, false), false);
    let text = plan_left.by_hand.join("\n");
    assert!(text.contains("Remove-DnsClientNrptRule"), "{text}");
    assert!(text.contains("Comment -eq 'Crisol'"), "{text}");
    assert!(text.contains("# BEGIN CRISOL"), "{text}");
    assert!(text.contains(r"drivers\etc\hosts"), "{text}");
    assert!(!ids(&plan_left).contains(&"helper"));
}

/// A helper whose installation cannot be looked at.
struct UnknownHelper;

impl SystemSetup for UnknownHelper {
    fn install(&self, _suffix: &str) -> io::Result<crate::setup::SetupReport> {
        unreachable!()
    }
    fn uninstall(&self, _suffix: &str, _home: &Path) -> io::Result<crate::setup::SetupReport> {
        unreachable!()
    }
    fn installed(&self) -> io::Result<bool> {
        Err(io::Error::other("access denied"))
    }
}

#[test]
fn what_could_not_be_checked_keeps_the_home_and_the_programs() {
    // Review of phase 11, finding 1: a CA or a helper that could not be looked at may still
    // be in place, and the programs are needed to take it out.
    let unknown = UnknownHelper;
    let installed = FakeSetup::new();
    let absent = FakeSetup::not_installed();
    let cases: [(&str, &dyn SystemSetup, bool); 3] = [
        ("helper", &unknown, false),
        ("trust", &installed, true),
        ("foreign", &absent, false),
    ];
    for (case, setup, store_fails) in cases {
        for purge in [false, true] {
            let world = World::new();
            fs::create_dir_all(world.paths.bin_dir().join("php")).unwrap();
            if case == "foreign" {
                fs::create_dir(world.paths.home().join("Documents")).unwrap();
            }
            let mut system = world.system(setup, false);
            if store_fails {
                system.trust = Err("access denied".to_owned());
            }
            system.programs = vec![PathBuf::from(r"C:\Tools\crisol.exe")];
            system.remove_programs = &|_| panic!("the programs must stay");

            let plan = plan(&system, purge);
            assert!(!plan.problems.is_empty(), "{case}");
            let outcomes = run(&system, &plan, &mut || unreachable!());
            let deleting: Vec<_> = kinds(&outcomes)
                .into_iter()
                .filter(|(step, _)| ["clean", "purge", "programs"].contains(step))
                .collect();
            assert!(
                deleting.iter().any(|(step, _)| *step == "programs"),
                "{case}"
            );
            assert!(
                deleting
                    .iter()
                    .all(|(_, kind)| *kind == OutcomeKind::Skipped),
                "{case} {purge}: {outcomes:?}"
            );
            let programs = outcomes.iter().find(|o| o.step == "programs").unwrap();
            assert!(
                programs.message.contains("could not be checked"),
                "{case}: {}",
                programs.message
            );
            assert!(world.paths.bin_dir().join("php").is_dir(), "{case} {purge}");
            assert!(
                world.paths.certs_dir().join("ca.pem").is_file(),
                "{case} {purge}"
            );
        }
    }
}

#[test]
fn a_trust_store_that_cannot_be_opened_is_told_with_the_way_by_hand() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let mut system = world.system(&setup, false);
    system.trust = Err("access denied".to_owned());
    let plan = plan(&system, false);
    assert!(!ids(&plan).contains(&"ca"));
    assert!(
        plan.problems[0].contains("certmgr.msc"),
        "{:?}",
        plan.problems
    );
}
