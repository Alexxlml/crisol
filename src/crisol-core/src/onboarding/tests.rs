use std::fs;
use std::time::SystemTime;

use tempfile::TempDir;

use super::*;
use crate::autostart::FakeAutostart;
use crate::paths::fake::FakePrivateDirs;
use crate::setup::FakeSetup;
use crate::shimsetup::path_status;
use crate::startmenu::FakeStartMenu;
use crate::trust::fake::FakeTrustStore;
use crate::userpath::fake::FakeUserPath;

/// A home, a folder with the programs, and fakes for everything Windows.
struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    programs: PathBuf,
    dirs: FakePrivateDirs,
    autostart: FakeAutostart,
    user_path: FakeUserPath,
    trust: FakeTrustStore,
    start_menu: FakeStartMenu,
}

impl World {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let programs = tmp.path().join("programs");
        fs::create_dir(&programs).unwrap();
        for name in ["crisol-shim.exe", "crisold.exe", "crisol-gui.exe"] {
            fs::write(programs.join(name), name).unwrap();
        }
        Self {
            _tmp: tmp,
            paths,
            programs,
            dirs: FakePrivateDirs::new(),
            autostart: FakeAutostart::new(),
            user_path: FakeUserPath::with_user(r"C:\Tools"),
            trust: FakeTrustStore::new(),
            start_menu: FakeStartMenu::new(),
        }
    }

    fn system<'a>(&'a self, setup: &'a dyn SystemSetup) -> System<'a> {
        System {
            paths: &self.paths,
            dirs: &self.dirs,
            autostart: &self.autostart,
            user_path: &self.user_path,
            trust: Ok(&self.trust),
            setup,
            shim_exe: Some(self.programs.join("crisol-shim.exe")),
            daemon_exe: Some(self.programs.join("crisold.exe")),
            start_menu: &self.start_menu,
            gui_exe: Some(self.programs.join("crisol-gui.exe")),
            reserved_name: &|_| false,
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
fn a_fresh_install_lists_every_step_in_order_and_changes_nothing() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let plan = plan(&world.system(&setup), SystemTime::now());

    assert_eq!(
        ids(&plan),
        ["shims", "path", "startmenu", "trust", "helper", "autostart"]
    );
    assert!(plan.problems.is_empty() && plan.done.is_empty());
    let asked: Vec<_> = plan
        .steps
        .iter()
        .filter(|s| s.optional())
        .map(Step::id)
        .collect();
    assert_eq!(asked, ["trust", "helper", "autostart"]);
    // Only read: no shim, no PATH entry, no CA, no sign-in entry, no call to the helper.
    assert!(shims_present(&world.paths).iter().all(|(_, p)| !p));
    assert!(
        !path_status(&world.user_path, &world.paths, &|_| false)
            .unwrap()
            .in_path
    );
    assert!(!world.paths.ca_cert().exists());
    assert_eq!(world.autostart.current().unwrap(), None);
    assert_eq!(world.trust.changes(), (0, 0));
    assert_eq!(setup.calls(), Vec::<String>::new());
}

#[test]
fn running_it_all_sets_everything_up_and_a_second_look_finds_nothing_left() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let system = world.system(&setup);
    let first = plan(&system, SystemTime::now());
    let outcomes = run(&system, &first, &mut |_| true, SystemTime::now());

    assert_eq!(
        kinds(&outcomes),
        [
            ("shims", OutcomeKind::Done),
            ("path", OutcomeKind::Done),
            ("startmenu", OutcomeKind::Done),
            ("trust", OutcomeKind::Done),
            ("helper", OutcomeKind::Done),
            ("autostart", OutcomeKind::Done),
        ],
        "{outcomes:#?}"
    );
    assert_eq!(setup.calls(), ["install test"]);
    assert_eq!(world.trust.changes(), (1, 0));
    assert!(
        path_status(&world.user_path, &world.paths, &|_| false)
            .unwrap()
            .in_path
    );
    assert!(world.autostart.current().unwrap().is_some());

    // The helper fake does not remember; with a helper installed nothing else is left.
    let installed = FakeSetup::new();
    let again = plan(&world.system(&installed), SystemTime::now());
    assert!(again.is_empty(), "{:?}", ids(&again));
    assert_eq!(again.done.len(), 6, "{:?}", again.done);
}

#[test]
fn what_is_asked_alone_can_be_refused_and_the_rest_still_happens() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let system = world.system(&setup);
    let plan = plan(&system, SystemTime::now());
    let mut asked = Vec::new();
    let outcomes = run(
        &system,
        &plan,
        &mut |step| {
            asked.push(step.id());
            step.id() == "autostart"
        },
        SystemTime::now(),
    );

    assert_eq!(asked, ["trust", "helper", "autostart"]);
    assert_eq!(
        kinds(&outcomes),
        [
            ("shims", OutcomeKind::Done),
            ("path", OutcomeKind::Done),
            ("startmenu", OutcomeKind::Done),
            ("trust", OutcomeKind::Declined),
            ("helper", OutcomeKind::Declined),
            ("autostart", OutcomeKind::Done),
        ]
    );
    // A refusal changes nothing: no CA made, nothing trusted, no administrator prompt.
    assert!(!world.paths.ca_cert().exists());
    assert_eq!(world.trust.changes(), (0, 0));
    assert_eq!(setup.calls(), Vec::<String>::new());
    assert!(outcomes[3].message.contains("crisol trust"));
}

#[test]
fn windows_saying_no_to_the_certificate_is_a_result_and_not_a_failure() {
    let world = World::new();
    world.trust.decline_next();
    let setup = FakeSetup::new();
    let system = world.system(&setup);
    let plan = plan(&system, SystemTime::now());
    let outcomes = run(&system, &plan, &mut |_| true, SystemTime::now());

    let trust = outcomes.iter().find(|o| o.step == "trust").unwrap();
    assert_eq!(trust.kind, OutcomeKind::Declined, "{trust:?}");
    assert!(
        outcomes.iter().all(|o| o.kind != OutcomeKind::Failed),
        "{outcomes:#?}"
    );
    // The CA was made and is kept, so `crisol trust` has it ready.
    assert!(world.paths.ca_cert().exists());
}

#[test]
fn programs_in_a_folder_others_can_write_stop_everything_before_anything_changes() {
    let world = World::new();
    world.dirs.expose(&world.programs);
    let setup = FakeSetup::not_installed();
    let system = world.system(&setup);
    let plan = plan(&system, SystemTime::now());

    assert!(plan.is_blocked() && plan.steps.is_empty());
    // One folder holds both programs: it is said once, with the command that closes it.
    assert_eq!(plan.problems.len(), 1, "{:?}", plan.problems);
    assert!(plan.problems[0]
        .fix
        .as_deref()
        .is_some_and(|fix| fix.contains("icacls")));

    let mut asked = false;
    let agreed = run_if_agreed(
        &system,
        &plan,
        &mut |_| {
            asked = true;
            true
        },
        &mut |_| true,
        SystemTime::now(),
    );
    assert_eq!(agreed, Agreed::Blocked);
    assert!(!asked, "nobody is asked to agree to what cannot be done");
    assert_eq!(
        run(&system, &plan, &mut |_| true, SystemTime::now()),
        Vec::new()
    );
    assert!(shims_present(&world.paths).iter().all(|(_, p)| !p));
}

#[test]
fn a_missing_program_is_said_by_name() {
    let world = World::new();
    fs::remove_file(world.programs.join("crisold.exe")).unwrap();
    let setup = FakeSetup::new();
    let plan = plan(&world.system(&setup), SystemTime::now());

    assert!(plan.is_blocked());
    assert!(
        plan.problems[0].message.contains("crisold"),
        "{:?}",
        plan.problems
    );
}

#[test]
fn nobody_is_asked_when_everything_is_in_place_and_a_no_changes_nothing() {
    let world = World::new();
    let setup = FakeSetup::new();
    let system = world.system(&setup);
    let first = plan(&system, SystemTime::now());
    run(&system, &first, &mut |_| true, SystemTime::now());

    let second = plan(&system, SystemTime::now());
    let agreed = run_if_agreed(
        &system,
        &second,
        &mut |_| panic!("nothing to agree to"),
        &mut |_| panic!("nothing to ask"),
        SystemTime::now(),
    );
    assert_eq!(agreed, Agreed::NothingToDo);

    // And a no to the whole plan leaves a fresh install untouched.
    let fresh = World::new();
    let setup = FakeSetup::not_installed();
    let system = fresh.system(&setup);
    let plan = plan(&system, SystemTime::now());
    let agreed = run_if_agreed(
        &system,
        &plan,
        &mut |_| false,
        &mut |_| true,
        SystemTime::now(),
    );
    assert_eq!(agreed, Agreed::No);
    assert!(shims_present(&fresh.paths).iter().all(|(_, p)| !p));
}

#[test]
fn after_an_update_the_shims_of_the_old_program_are_listed_again_and_brought_up_to_date() {
    let world = World::new();
    let setup = FakeSetup::new();
    let system = world.system(&setup);
    let first = plan(&system, SystemTime::now());
    run(&system, &first, &mut |_| true, SystemTime::now());
    assert!(plan(&system, SystemTime::now()).is_empty());

    // What `winget upgrade` does: a new `crisol-shim.exe` in the same place.
    fs::write(world.programs.join("crisol-shim.exe"), "crisol-shim.exe 2").unwrap();
    let after = plan(&system, SystemTime::now());
    assert_eq!(ids(&after), ["shims"]);
    assert!(matches!(after.steps[0], Step::Shims { refresh: true, .. }));
    assert!(after.steps[0].describe().starts_with("Update the commands"));

    let outcomes = run(&system, &after, &mut |_| true, SystemTime::now());
    assert_eq!(kinds(&outcomes), [("shims", OutcomeKind::Done)]);
    assert!(plan(&system, SystemTime::now()).is_empty());
}

#[test]
fn another_crisold_in_the_sign_in_entry_is_replaced_and_said() {
    let world = World::new();
    world.autostart.set(r#""C:\old\crisold.exe""#).unwrap();
    let setup = FakeSetup::new();
    let system = world.system(&setup);
    let plan = plan(&system, SystemTime::now());

    assert!(ids(&plan).contains(&"autostart"));
    assert!(plan.notes.iter().any(|n| n.contains(r"C:\old\crisold.exe")));
    let outcomes = run(&system, &plan, &mut |_| true, SystemTime::now());
    let autostart = outcomes.iter().find(|o| o.step == "autostart").unwrap();
    assert!(autostart.message.contains("replaced"), "{autostart:?}");
}

#[test]
fn the_plan_and_its_steps_are_json_the_gui_can_read() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let plan = plan(&world.system(&setup), SystemTime::now());
    let json = serde_json::to_value(&plan).unwrap();

    assert_eq!(json["steps"][0]["step"], "shims");
    assert_eq!(json["steps"][0]["optional"], false);
    assert_eq!(json["steps"][3]["step"], "trust");
    assert_eq!(json["steps"][3]["optional"], true);
    assert_eq!(json["problems"], serde_json::json!([]));
}

#[test]
fn the_start_menu_entry_is_made_once_and_remade_when_it_opens_another_window() {
    let world = World::new();
    let setup = FakeSetup::new();
    let gui = world.programs.join("crisol-gui.exe");

    // None yet: it is a step that is not asked alone (it only touches the user's own folder).
    let first = plan(&world.system(&setup), SystemTime::now());
    let step = first.steps.iter().find(|s| s.id() == "startmenu").unwrap();
    assert!(!step.optional());
    assert!(matches!(step, Step::StartMenu { refresh: false, target, .. } if *target == gui));

    run(
        &world.system(&setup),
        &first,
        &mut |_| true,
        SystemTime::now(),
    );
    assert_eq!(world.start_menu.target().unwrap(), Some(gui.clone()));
    let again = plan(&world.system(&setup), SystemTime::now());
    assert!(!ids(&again).contains(&"startmenu"));
    assert!(again.done.iter().any(|d| d.contains("Start menu")));

    // An update moved the window: the old entry opens a program that is not there any more.
    world
        .start_menu
        .create(Path::new(r"C:\old\crisol-gui.exe"))
        .unwrap();
    let moved = plan(&world.system(&setup), SystemTime::now());
    assert!(moved
        .steps
        .iter()
        .any(|s| matches!(s, Step::StartMenu { refresh: true, .. })));
}

#[test]
fn a_package_without_the_window_has_no_start_menu_step() {
    let world = World::new();
    let setup = FakeSetup::new();
    fs::remove_file(world.programs.join("crisol-gui.exe")).unwrap();
    let plan = plan(&world.system(&setup), SystemTime::now());
    assert!(!ids(&plan).contains(&"startmenu"));
    assert_eq!(plan.problems, []);
}

#[test]
fn step_ids_names_every_step_once() {
    let folder = PathBuf::from(r"C:\Crisol\shims");
    let steps = [
        Step::Shims {
            folder: folder.clone(),
            refresh: false,
        },
        Step::Path { folder },
        Step::StartMenu {
            path: PathBuf::from("Crisol.lnk"),
            target: PathBuf::from("crisol-gui.exe"),
            refresh: false,
        },
        Step::Trust,
        Step::Helper {
            suffix: "test".to_owned(),
        },
        Step::Autostart {
            daemon: PathBuf::from("crisold.exe"),
        },
    ];
    for step in &steps {
        // No wildcard: a new step does not compile here until it is in `steps` and `STEP_IDS`.
        match step {
            Step::Shims { .. }
            | Step::Path { .. }
            | Step::StartMenu { .. }
            | Step::Trust
            | Step::Helper { .. }
            | Step::Autostart { .. } => {}
        }
    }
    let ids: Vec<_> = steps.iter().map(Step::id).collect();
    assert_eq!(ids, STEP_IDS);
}
