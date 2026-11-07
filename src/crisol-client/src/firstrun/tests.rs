use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use crisol_core::autostart::{Autostart, FakeAutostart};
use crisol_core::paths::fake::FakePrivateDirs;
use crisol_core::paths::CrisolPaths;
use crisol_core::setup::{FakeSetup, SystemSetup};
use crisol_core::teardown::OutcomeKind;
use crisol_core::trust::fake::FakeTrustStore;
use crisol_core::userpath::fake::FakeUserPath;
use tempfile::TempDir;

use super::*;

/// A home, a folder with the programs, and fakes for everything Windows.
struct World {
    _tmp: TempDir,
    paths: CrisolPaths,
    programs: PathBuf,
    dirs: FakePrivateDirs,
    autostart: FakeAutostart,
    user_path: FakeUserPath,
    trust: FakeTrustStore,
}

impl World {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        paths.ensure_layout().unwrap();
        let programs = tmp.path().join("programs");
        fs::create_dir(&programs).unwrap();
        for name in ["crisol-shim.exe", "crisold.exe"] {
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
        }
    }

    fn setup_system<'a>(&'a self, setup: &'a dyn SystemSetup) -> onboarding::System<'a> {
        onboarding::System {
            paths: &self.paths,
            dirs: &self.dirs,
            autostart: &self.autostart,
            user_path: &self.user_path,
            trust: Ok(&self.trust),
            setup,
            shim_exe: Some(self.programs.join("crisol-shim.exe")),
            daemon_exe: Some(self.programs.join("crisold.exe")),
            reserved_name: &|_| false,
        }
    }

    fn teardown_system<'a>(&'a self, setup: &'a dyn SystemSetup) -> teardown::System<'a> {
        teardown::System {
            paths: &self.paths,
            autostart: &self.autostart,
            user_path: &self.user_path,
            trust: Ok(&self.trust),
            setup,
            daemon_running: true,
            programs: Vec::new(),
            remove_programs: &|_| Ok(()),
            winget: false,
        }
    }

    /// Everything Crisol can change, changed.
    fn set_up(&self) {
        let setup = FakeSetup::not_installed();
        let accepted = ["trust", "helper", "autostart"].map(str::to_owned);
        run_setup_on(&self.setup_system(&setup), &accepted, SystemTime::now()).unwrap();
    }
}

fn ids(view: &SetupView) -> Vec<&'static str> {
    view.steps.iter().map(|s| s.id).collect()
}

fn kinds(outcomes: &[Outcome]) -> Vec<(&'static str, OutcomeKind)> {
    outcomes.iter().map(|o| (o.step, o.kind)).collect()
}

#[test]
fn a_fresh_install_shows_every_step_and_which_switches_start_on() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let plan = onboarding::plan(&world.setup_system(&setup), SystemTime::now());
    let view = setup_view(&plan);

    assert_eq!(
        ids(&view),
        ["shims", "path", "trust", "helper", "autostart"]
    );
    let switch = |id: &str| {
        let step = view.steps.iter().find(|s| s.id == id).unwrap();
        (step.optional, step.on_by_default)
    };
    // What makes the sites work starts on; what changes the sign-in does not; the rest has no
    // switch: the button is its agreement.
    assert_eq!(switch("trust"), (true, true));
    assert_eq!(switch("helper"), (true, true));
    assert_eq!(switch("autostart"), (true, false));
    assert_eq!(switch("shims"), (false, false));
    assert_eq!(switch("path"), (false, false));
    assert!(view.can_run());
    assert!(view.steps.iter().all(|s| !s.description.is_empty()));
    // Looking changed nothing.
    assert_eq!(setup.calls(), Vec::<String>::new());
    assert_eq!(world.autostart.current().unwrap(), None);
}

#[test]
fn only_the_switches_that_are_on_are_done_and_a_no_is_a_result() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let accepted = vec!["helper".to_owned()];
    let done = run_setup_on(&world.setup_system(&setup), &accepted, SystemTime::now()).unwrap();

    assert_eq!(
        kinds(&done.outcomes),
        [
            ("shims", OutcomeKind::Done),
            ("path", OutcomeKind::Done),
            ("trust", OutcomeKind::Declined),
            ("helper", OutcomeKind::Done),
            ("autostart", OutcomeKind::Declined),
        ]
    );
    assert!(!done.failed, "a no is not a failure: {done:?}");
    assert_eq!(setup.calls().len(), 1);
    assert_eq!(world.autostart.current().unwrap(), None);
}

#[test]
fn a_name_the_page_makes_up_is_not_a_step() {
    let world = World::new();
    let setup = FakeSetup::not_installed();
    let accepted = vec!["format-disk".to_owned(), "uninstall".to_owned()];
    let done = run_setup_on(&world.setup_system(&setup), &accepted, SystemTime::now()).unwrap();

    // Nothing optional ran: only what the core's own plan lists can be done.
    assert_eq!(setup.calls(), Vec::<String>::new());
    assert!(done
        .outcomes
        .iter()
        .filter(|o| ["trust", "helper", "autostart"].contains(&o.step))
        .all(|o| o.kind == OutcomeKind::Declined));
}

#[test]
fn a_setup_that_is_done_has_nothing_to_show_and_nothing_to_run() {
    let world = World::new();
    world.set_up();
    let setup = FakeSetup::new();

    let plan = onboarding::plan(&world.setup_system(&setup), SystemTime::now());
    let view = setup_view(&plan);
    assert!(view.steps.is_empty() && !view.can_run(), "{view:?}");
    assert_ne!(view.done, Vec::<String>::new());

    let again = run_setup_on(&world.setup_system(&setup), &[], SystemTime::now()).unwrap();
    assert!(again.outcomes.is_empty() && !again.failed);
}

#[test]
fn what_stops_the_setup_is_shown_with_its_fix_and_nothing_is_offered() {
    let world = World::new();
    fs::remove_file(world.programs.join("crisol-shim.exe")).unwrap();
    let setup = FakeSetup::not_installed();
    let system = world.setup_system(&setup);
    let view = setup_view(&onboarding::plan(&system, SystemTime::now()));

    assert_ne!(view.problems, Vec::new());
    assert!(!view.can_run(), "{view:?}");
    let error = run_setup_on(&system, &["helper".to_owned()], SystemTime::now()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(setup.calls(), Vec::<String>::new());
}

// ---- uninstall ---------------------------------------------------------------------------

fn count_stops(count: &mut u32) -> impl FnMut() -> Result<(), String> + '_ {
    move || {
        *count += 1;
        Ok(())
    }
}

#[test]
fn the_uninstall_view_says_whether_the_home_goes_with_it() {
    let world = World::new();
    let setup = FakeSetup::new();
    let system = world.teardown_system(&setup);

    let safe = uninstall_view(teardown::plan(&system, false));
    assert!(!safe.deletes_home);
    assert!(safe.plan.purge.is_none());

    let purge = uninstall_view(teardown::plan(&system, true));
    assert!(purge.deletes_home);
    let notice = purge.plan.purge.as_ref().unwrap();
    assert_eq!(notice.home, world.paths.home());
}

#[test]
fn the_uninstall_view_is_the_plan_of_the_command_line_in_json() {
    let world = World::new();
    let setup = FakeSetup::new();
    let view = uninstall_view(teardown::plan(&world.teardown_system(&setup), true));
    let json = serde_json::to_value(&view).unwrap();

    assert!(json["steps"].is_array());
    assert_eq!(json["deletes_home"], true);
    assert!(json["purge"]["databases"].is_array());
    assert!(json["by_hand"].is_array() && json["kept"].is_array() && json["problems"].is_array());
}

#[test]
fn a_purge_is_refused_until_the_backup_is_confirmed_and_nothing_is_touched() {
    let world = World::new();
    world.set_up();
    let setup = FakeSetup::new();
    let system = world.teardown_system(&setup);

    let mut stopped = 0;
    let error = run_uninstall_on(&system, true, false, &mut count_stops(&mut stopped)).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert_eq!(stopped, 0, "the daemon was not stopped");
    assert_eq!(setup.calls(), Vec::<String>::new());
    assert!(world.paths.home().exists());
    assert!(world.autostart.current().unwrap().is_some());
}

#[test]
fn a_safe_uninstall_needs_no_backup_and_leaves_the_user_s_files() {
    let world = World::new();
    world.set_up();
    fs::write(world.paths.config_file(), "[php]\n").unwrap();
    let setup = FakeSetup::new();
    let system = world.teardown_system(&setup);

    let mut stopped = 0;
    let done = run_uninstall_on(&system, false, false, &mut count_stops(&mut stopped)).unwrap();
    assert_eq!(stopped, 1);
    assert!(done.done, "{:?}", done.outcomes);
    assert!(done.close_app);
    assert_eq!(world.autostart.current().unwrap(), None);
    assert!(world.paths.config_file().exists());
}

#[test]
fn a_purge_with_the_backup_confirmed_deletes_the_home_and_the_app_closes() {
    let world = World::new();
    world.set_up();
    let setup = FakeSetup::new();
    let system = world.teardown_system(&setup);

    let mut stopped = 0;
    let done = run_uninstall_on(&system, true, true, &mut count_stops(&mut stopped)).unwrap();
    assert!(done.done && done.close_app, "{:?}", done.outcomes);
    assert!(!world.paths.home().exists());
}

#[test]
fn an_uninstall_that_left_something_keeps_the_app_open_to_read_what_failed() {
    let world = World::new();
    world.set_up();
    let setup = FakeSetup::declining();
    let system = world.teardown_system(&setup);

    let mut stopped = 0;
    let done = run_uninstall_on(&system, false, false, &mut count_stops(&mut stopped)).unwrap();
    assert!(!done.done);
    assert!(!done.close_app);
    let helper = done.outcomes.iter().find(|o| o.step == "helper").unwrap();
    assert_eq!(helper.kind, OutcomeKind::Declined);
}
