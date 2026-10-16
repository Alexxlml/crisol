//! Orchestration tests: temporary home, the process and port fakes, a made-up clock.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};

use tempfile::TempDir;

use super::*;
use crate::paths::fake::FakePrivateDirs;
use crate::ports::fake::FakePortProbe;
use crate::ports::PortOwner;
use crate::process::fake::FakeSupervisor;
use crate::project::TrustAllOwners;
use crate::sites::SitesFile;

const HTTP: u16 = 19080;

struct Rig {
    _tmp: TempDir,
    paths: CrisolPaths,
    config: Config,
    catalog: Catalog,
    supervisor: Arc<FakeSupervisor>,
    probe: Arc<FakePortProbe>,
    stack: WebStack,
    dirs: Arc<FakePrivateDirs>,
    t0: Instant,
    /// Control commands started before the test began its own changes (the first start's).
    baseline: usize,
}

impl Rig {
    fn new(php: &[&str]) -> Self {
        Self::with_config(php, |_| {})
    }

    fn with_config(php: &[&str], adjust: impl FnOnce(&mut Config)) -> Self {
        let tmp = TempDir::new().unwrap();
        let paths = CrisolPaths::new(tmp.path().join("home")).unwrap();
        let mut config = Config::default();
        config.services.nginx.http_port = HTTP;
        config.php.base_port = 19100;
        config.php.workers = 2;
        adjust(&mut config);
        for version in php {
            fs::create_dir_all(paths.bin_for("php", version).unwrap()).unwrap();
        }
        let nginx_dir = paths.bin_for("nginx", "1.28.3").unwrap();
        fs::create_dir_all(&nginx_dir).unwrap();
        let supervisor = Arc::new(FakeSupervisor::new());
        let probe = Arc::new(FakePortProbe::new());
        let dirs = Arc::new(FakePrivateDirs::new());
        let stack = WebStack::new(
            paths.clone(),
            &config,
            nginx_dir,
            supervisor.clone(),
            probe.clone(),
            dirs.clone(),
        );
        Self {
            _tmp: tmp,
            paths,
            config,
            catalog: Catalog::parse("schema = 1\nserial = 1\n").unwrap(),
            supervisor,
            probe,
            stack,
            dirs,
            t0: Instant::now(),
            baseline: 0,
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    /// A parked site with an optional `.php-version`.
    fn site(&self, name: &str, php: Option<&str>) {
        let dir = self.paths.www_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        if let Some(php) = php {
            fs::write(dir.join(".php-version"), format!("{php}\n")).unwrap();
        }
    }

    fn plan(&self) -> WebPlan {
        let none = |_: &str| None;
        plan_web(
            &self.paths,
            &self.config,
            &SitesFile::default(),
            &self.catalog,
            &TrustAllOwners,
            &none,
        )
    }

    /// These ports get a listener (what the real processes would do once they are up).
    fn listen(&self, ports: &[u16]) {
        for &port in ports {
            self.probe.occupy(
                port,
                PortOwner {
                    pid: 1,
                    name: Some("x".to_owned()),
                    address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                },
            );
        }
    }

    fn tick(&mut self, ms: u64) -> Vec<WebEvent> {
        let now = self.at(ms);
        self.stack.tick(now)
    }

    /// Every control command (`-t`, `-s reload`) that was started, with its arguments after
    /// `-c <conf> -p <dir>`. Their groups are closed once they finish, so they are found by
    /// what they ran.
    fn controls(&self) -> Vec<(crate::process::ProcessId, Vec<String>)> {
        self.supervisor
            .spawned()
            .into_iter()
            .filter_map(|r| {
                let args: Vec<String> = r
                    .spec
                    .args
                    .iter()
                    .skip(4)
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                (args == ["-t"] || args == ["-s", "reload"]).then_some((r.process, args))
            })
            .collect()
    }

    fn control_commands(&self) -> Vec<Vec<String>> {
        self.controls()
            .into_iter()
            .skip(self.baseline)
            .map(|(_, args)| args)
            .collect()
    }

    /// `nginx -t` of the first start passes.
    fn first_test_passes(&mut self, ms: u64) {
        self.supervisor.exit(self.last_control(), 0).unwrap();
        self.tick(ms);
    }

    fn final_conf(&self, relative: &str) -> PathBuf {
        self.paths.etc_for("nginx").unwrap().join(relative)
    }

    fn last_control(&self) -> crate::process::ProcessId {
        self.controls().pop().unwrap().0
    }

    fn started(&self, group: &str) -> usize {
        self.supervisor
            .spawned()
            .into_iter()
            .filter(|r| self.supervisor.group_name(r.group).as_deref() == Some(group))
            .count()
    }

    /// Starts the stack with the current sites and brings everything up.
    fn run(&mut self) {
        let plan = self.plan();
        self.run_with(&plan);
    }

    fn run_with(&mut self, plan: &WebPlan) {
        self.stack.apply(plan, self.at(0)).unwrap();
        self.stack.start(self.at(0));
        self.first_test_passes(100);
        let mut ports = vec![HTTP];
        for version in plan.php_versions().keys() {
            ports.extend(self.stack.pool_ports(version).unwrap());
        }
        self.listen(&ports);
        self.tick(600);
        assert_eq!(self.stack.nginx_state(), Some(&ServiceState::Running));
        self.baseline = self.controls().len();
    }
}

fn versions(plan: &WebPlan) -> Vec<(String, Option<String>)> {
    plan.sites
        .iter()
        .map(|s| {
            (
                s.site.name.clone(),
                s.php.as_ref().map(|v| v.version.to_string()),
            )
        })
        .collect()
}

// ---- planning ---------------------------------------------------------------------

#[test]
fn each_site_gets_the_php_the_shims_would_run_in_its_folder() {
    let rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("old", Some("8.3"));
    rig.site("new", None);
    rig.site("pinned", Some("8.4"));
    let plan = rig.plan();
    assert_eq!(
        versions(&plan),
        [
            ("new".to_owned(), Some("8.4.26".to_owned())),
            ("old".to_owned(), Some("8.3.35".to_owned())),
            ("pinned".to_owned(), Some("8.4.26".to_owned())),
        ]
    );
    assert_eq!(plan.warnings, Vec::<String>::new());
    assert_eq!(
        plan.php_versions().keys().collect::<Vec<_>>(),
        ["8.3.35", "8.4.26"]
    );
}

#[test]
fn a_site_that_wants_a_php_that_is_not_installed_is_still_served_without_php() {
    let rig = Rig::new(&["8.4.26"]);
    rig.site("legacy", Some("5.6"));
    rig.site("fine", None);
    let plan = rig.plan();
    assert_eq!(
        versions(&plan),
        [
            ("fine".to_owned(), Some("8.4.26".to_owned())),
            ("legacy".to_owned(), None)
        ]
    );
    assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
    assert!(plan.warnings[0].contains("legacy.test") && plan.warnings[0].contains("5.6"));
}

#[test]
fn with_no_php_installed_every_site_is_static() {
    let rig = Rig::new(&[]);
    rig.site("a", None);
    let plan = rig.plan();
    assert_eq!(versions(&plan), [("a".to_owned(), None)]);
    assert_eq!(plan.warnings.len(), 1);
    assert!(plan.php_versions().is_empty());
}

// ---- applying and starting --------------------------------------------------------

#[test]
fn applying_a_plan_to_a_stopped_stack_writes_the_files_and_runs_nothing() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(0)).unwrap();
    assert_eq!(applied.new_pools, ["8.4.26"]);
    assert!(!applied.reload_requested);
    assert!(applied.config_changed);
    assert!(
        !rig.final_conf("nginx.conf").exists() && !rig.final_conf("sites").exists(),
        "nothing is written before Nginx has accepted it"
    );
    assert_eq!(rig.supervisor.spawned().len(), 0);
    assert!(!rig.stack.is_running());
}

#[test]
fn starting_runs_nginx_and_one_pool_per_php_version_in_use() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("a", Some("8.3"));
    rig.site("b", Some("8.4"));
    rig.run();
    assert_eq!(rig.started("nginx"), 1);
    assert_eq!(rig.started("php-8.3.35"), 2, "two workers each");
    assert_eq!(rig.started("php-8.4.26"), 2);
    assert_eq!(rig.stack.pool_ports("8.3.35"), Some(&[19100, 19101][..]));
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19102, 19103][..]));
    assert_eq!(rig.stack.pool_state("8.4.26"), Some(&ServiceState::Running));
}

#[test]
fn the_nginx_process_gets_the_generated_file_and_the_installation_as_prefix() {
    let mut rig = Rig::new(&[]);
    rig.site("a", None);
    rig.run();
    let record = rig
        .supervisor
        .spawned()
        .into_iter()
        .find(|r| rig.supervisor.group_name(r.group).as_deref() == Some("nginx"))
        .unwrap();
    let args: Vec<String> = record
        .spec
        .args
        .iter()
        .map(|a| a.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(args[0], "-c");
    assert!(args[1].ends_with("etc/nginx/nginx.conf"), "{args:?}");
    assert_eq!(args[2], "-p");
    assert!(args[3].ends_with("bin/nginx/1.28.3"), "{args:?}");
}

// ---- changing sites while running -------------------------------------------------

#[test]
fn a_new_site_with_a_new_php_starts_its_pool_then_tests_then_reloads_in_that_order() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("shop", Some("8.4"));
    rig.run();

    rig.site("legacy", Some("8.3"));
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(applied.new_pools, ["8.3.35"]);
    assert!(applied.reload_requested);
    assert_eq!(rig.started("php-8.3.35"), 2, "the new pool starts at once");
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());

    // The new pool is not up yet: Nginx is not told about it.
    assert_eq!(rig.tick(1200), vec![]);
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());

    // It comes up: the configuration is tested first.
    rig.listen(&[19102, 19103]);
    rig.tick(1700);
    assert_eq!(rig.control_commands(), [["-t"]]);
    // Staged, not real yet: the files Nginx would read on a restart are the old ones.
    assert!(rig.final_conf("../nginx/.next/sites/legacy.conf").is_file());
    assert!(!rig.final_conf("sites/legacy.conf").exists());
    // The test passes: only now it is written and the reload is requested.
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2000);
    assert_eq!(events, vec![]);
    assert_eq!(rig.control_commands(), [vec!["-t"], vec!["-s", "reload"]]);
    assert!(rig.final_conf("sites/legacy.conf").is_file());
    assert!(
        !rig.final_conf(".next").exists(),
        "the staging folder is cleaned up"
    );
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2300);
    assert_eq!(events, vec![WebEvent::Reloaded]);
    // Nginx itself was never restarted.
    assert_eq!(rig.started("nginx"), 1);
}

#[test]
fn a_configuration_nginx_rejects_is_reported_and_nothing_is_reloaded() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    let before = fs::read(rig.final_conf("nginx.conf")).unwrap();
    let shop_before = fs::read(rig.final_conf("sites/shop.conf")).unwrap();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    assert_eq!(rig.control_commands(), [["-t"]]);

    rig.supervisor.exit(rig.last_control(), 1).unwrap();
    let events = rig.tick(1300);
    assert_eq!(events.len(), 1);
    match &events[0] {
        WebEvent::ReloadFailed { reason } => {
            assert!(
                reason.contains("rejected") && reason.contains("reload.log"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.control_commands().len(), 1, "no reload was sent");
    assert_eq!(rig.stack.nginx_state(), Some(&ServiceState::Running));
    // What Nginx would read on its next start is exactly what worked before.
    assert_eq!(fs::read(rig.final_conf("nginx.conf")).unwrap(), before);
    assert_eq!(
        fs::read(rig.final_conf("sites/shop.conf")).unwrap(),
        shop_before
    );
    assert!(!rig.final_conf("sites/blog.conf").exists());
}

#[test]
fn a_reload_that_takes_forever_gives_up() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    let events = rig.tick(1100 + 21_000);
    assert!(
        matches!(events.as_slice(), [WebEvent::ReloadFailed { reason }] if reason.contains("did not finish")),
        "{events:?}"
    );
}

#[test]
fn changes_made_during_a_reload_are_merged_into_one_more_reload() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("a", None);
    rig.run();

    rig.site("b", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100); // -t running
    rig.site("c", None);
    rig.site("d", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1200)).unwrap();
    let again = rig.stack.apply(&plan, rig.at(1250)).unwrap();
    assert!(
        again.reload_requested,
        "a round is under way: it runs once more"
    );

    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1400); // -s reload
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(1600);
    assert_eq!(events, vec![WebEvent::Reloaded]);

    // The second round: test, reload, done. Three sites changed, one extra round.
    let events = rig.tick(1700);
    assert_eq!(events, vec![WebEvent::ReloadStarted]);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1900);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(2100);
    assert_eq!(events, vec![WebEvent::Reloaded]);
    assert_eq!(
        rig.control_commands().len(),
        4,
        "two rounds of test + reload, no more"
    );
}

#[test]
fn a_pool_nobody_uses_any_more_is_stopped_after_the_reload_not_before() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("old", Some("8.3"));
    rig.site("new", Some("8.4"));
    rig.run();

    // The only 8.3 site goes away.
    fs::remove_dir_all(rig.paths.www_dir().join("old")).unwrap();
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(applied.retired_pools, ["8.3.35"]);
    assert!(applied.reload_requested);
    assert_eq!(
        rig.stack.pool_state("8.3.35"),
        Some(&ServiceState::Running),
        "still serving until Nginx stops sending it requests"
    );

    rig.tick(1100);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1300);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    let events = rig.tick(1500);
    assert_eq!(events, vec![WebEvent::Reloaded]);
    assert_eq!(
        rig.stack.pool_state("8.3.35"),
        Some(&ServiceState::Running),
        "the old Nginx workers still use it for a moment after the signal"
    );
    rig.tick(4000);
    assert_eq!(rig.stack.pool_state("8.3.35"), Some(&ServiceState::Running));
    rig.tick(6600);
    assert_eq!(rig.stack.pool_state("8.3.35"), None, "gone from the stack");
    rig.tick(6700);
    assert_eq!(rig.stack.pool_state("8.4.26"), Some(&ServiceState::Running));
    // Nginx and the two 8.4 workers are what is left.
    assert_eq!(rig.supervisor.alive_pids().len(), 3);
}

#[test]
fn a_pool_added_later_never_moves_the_ports_of_a_running_one() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("new", Some("8.4"));
    rig.run();
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19100, 19101][..]));

    // 8.3 sorts before 8.4, but it takes the next free block.
    rig.site("old", Some("8.3"));
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19100, 19101][..]));
    assert_eq!(rig.stack.pool_ports("8.3.35"), Some(&[19102, 19103][..]));
}

#[test]
fn a_change_that_alters_nothing_reloads_nothing() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(1000)).unwrap();
    assert!(!applied.reload_requested);
    assert!(!applied.config_changed);
    assert_eq!(rig.control_commands(), Vec::<Vec<String>>::new());
}

// ---- stopping ---------------------------------------------------------------------

#[test]
fn stopping_stops_everything_and_cancels_a_reload_under_way() {
    let mut rig = Rig::new(&["8.4.26"]);
    rig.site("shop", None);
    rig.run();
    rig.site("blog", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(1000)).unwrap();
    rig.tick(1100);
    assert_eq!(rig.control_commands().len(), 1);

    rig.stack.stop(rig.at(1200));
    assert!(!rig.stack.is_running());
    for ms in [1300, 1600, 2000, 6000, 6100, 11_000] {
        rig.tick(ms);
    }
    assert!(rig.stack.is_stopped());
    assert_eq!(
        rig.supervisor.alive_pids().len(),
        0,
        "nothing is left behind"
    );
    assert_eq!(rig.supervisor.group_count(), 0);
    assert_eq!(rig.control_commands().len(), 1, "no reload after the stop");
}

// ---- review of phase 5 ------------------------------------------------------------

#[test]
fn the_first_start_waits_for_the_test_and_a_rejected_one_leaves_nothing_behind() {
    let mut rig = Rig::new(&[]);
    rig.site("a", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(0)).unwrap();
    rig.stack.start(rig.at(0));
    assert_eq!(rig.control_commands(), [["-t"]]);
    assert_eq!(rig.started("nginx"), 0, "Nginx waits for the test");

    rig.supervisor.exit(rig.last_control(), 1).unwrap();
    let events = rig.tick(200);
    assert!(
        matches!(events.as_slice(), [WebEvent::ReloadFailed { .. }]),
        "{events:?}"
    );
    assert_eq!(
        rig.started("nginx"),
        0,
        "there is no earlier configuration to run"
    );
    assert!(!rig.final_conf("nginx.conf").exists());
}

#[test]
fn a_rejected_new_configuration_at_start_falls_back_to_the_one_that_worked() {
    let mut rig = Rig::new(&[]);
    rig.site("a", None);
    rig.run(); // writes the first, good configuration
    rig.stack.stop(rig.at(1000));
    for ms in [1100, 6000, 6100, 11_000] {
        rig.tick(ms);
    }
    assert!(rig.stack.is_stopped());
    rig.probe.release(HTTP); // the processes are gone, so are their listeners
    let good = fs::read(rig.final_conf("sites/a.conf")).unwrap();

    rig.site("b", None);
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(12_000)).unwrap();
    rig.stack.start(rig.at(12_000));
    rig.supervisor.exit(rig.last_control(), 1).unwrap();
    let events = rig.tick(12_200);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WebEvent::ReloadFailed { .. })),
        "{events:?}"
    );
    assert_eq!(
        rig.started("nginx"),
        1,
        "started again with the previous files"
    );
    assert_eq!(fs::read(rig.final_conf("sites/a.conf")).unwrap(), good);
    assert!(!rig.final_conf("sites/b.conf").exists());
}

#[test]
fn a_reload_only_waits_for_the_pools_it_created_not_for_one_that_is_restarting() {
    let mut rig = Rig::new(&["8.3.35", "8.4.26"]);
    rig.site("a", Some("8.4"));
    rig.run();
    // The only pool crashes completely and is waiting to be restarted.
    let pool_workers: Vec<_> = rig
        .supervisor
        .spawned()
        .into_iter()
        .filter(|r| rig.supervisor.group_name(r.group).as_deref() == Some("php-8.4.26"))
        .map(|r| r.process)
        .collect();
    for worker in pool_workers {
        rig.supervisor.exit(worker, 1).unwrap();
    }
    rig.tick(700);
    assert_eq!(
        rig.stack.pool_state("8.4.26"),
        Some(&ServiceState::Starting)
    );

    // A new site for the same PHP: nothing new to wait for.
    rig.site("b", Some("8.4"));
    let plan = rig.plan();
    let applied = rig.stack.apply(&plan, rig.at(800)).unwrap();
    assert!(applied.new_pools.is_empty() && applied.reload_requested);
    rig.tick(900);
    assert_eq!(rig.control_commands(), [["-t"]], "tested at once");
}

#[test]
fn pool_ports_skip_the_ports_of_the_other_services() {
    let mut rig = Rig::with_config(&["8.3.35", "8.4.26"], |config| {
        // Where the second pool would go.
        config.services.mysql.port = 19102;
    });
    rig.site("a", Some("8.3"));
    rig.site("b", Some("8.4"));
    let plan = rig.plan();
    rig.stack.apply(&plan, rig.at(0)).unwrap();
    assert_eq!(rig.stack.pool_ports("8.3.35"), Some(&[19100, 19101][..]));
    assert_eq!(rig.stack.pool_ports("8.4.26"), Some(&[19104, 19105][..]));
}

#[test]
fn the_terminal_override_does_not_reach_the_sites_and_resolution_warnings_are_kept() {
    let rig = Rig::new(&["7.4.33", "8.4.26"]);
    rig.site("shop", None);
    rig.site("odd", Some("lts/*"));
    let only_here = |name: &str| (name == "CRISOL_PHP").then(|| "7.4".to_owned());
    let plan = plan_web(
        &rig.paths,
        &rig.config,
        &SitesFile::default(),
        &rig.catalog,
        &TrustAllOwners,
        &only_here,
    );
    // Highest installed, not the 7.4 of the terminal the daemon happened to start from.
    assert_eq!(
        versions(&plan),
        [
            ("odd".to_owned(), Some("8.4.26".to_owned())),
            ("shop".to_owned(), Some("8.4.26".to_owned()))
        ]
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.starts_with("odd.test: ignoring") && w.contains(".php-version")),
        "{:?}",
        plan.warnings
    );
}

// ---- HTTPS (F6.2) ------------------------------------------------------------------

impl Rig {
    fn secure(&self, name: &str) {
        crate::sites::set_secure(&self.paths, name, true).unwrap();
    }

    fn plan_with_secure(&self) -> WebPlan {
        let none = |_: &str| None;
        plan_web(
            &self.paths,
            &self.config,
            &SitesFile::load(&self.paths).unwrap(),
            &self.catalog,
            &TrustAllOwners,
            &none,
        )
    }
}

#[test]
fn securing_a_site_makes_the_ca_and_the_certificate_and_the_config_uses_them() {
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.site("blog", None);
    rig.secure("shop");
    let plan = rig.plan_with_secure();
    let applied = rig.stack.apply(&plan, rig.at(0)).unwrap();
    assert!(applied.ca_created, "the user has to trust it");
    assert_eq!(applied.certificate, Some(crate::tls::Renewal::Missing));
    assert!(rig.paths.ca_cert().is_file() && rig.paths.site_cert().is_file());
    assert_eq!(applied.warnings.len(), 0, "{:?}", applied.warnings);

    // Start: the test passes, the config reaches the disk with the TLS block.
    rig.stack.start(rig.at(0));
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(100);
    let shop = fs::read_to_string(rig.final_conf("sites/shop.conf")).unwrap();
    assert!(
        shop.contains("ssl_certificate") && shop.contains("return 301"),
        "{shop}"
    );
    let blog = fs::read_to_string(rig.final_conf("sites/blog.conf")).unwrap();
    assert!(!blog.contains("ssl"), "only the secured site: {blog}");

    // The second time nothing is made again.
    let again = rig.stack.apply(&plan, rig.at(200)).unwrap();
    assert!(!again.ca_created && again.certificate.is_none());
    assert!(!again.config_changed);
}

#[test]
fn without_a_secured_site_no_ca_and_no_certificate_are_made() {
    let mut rig = Rig::new(&[]);
    rig.site("blog", None);
    let plan = rig.plan_with_secure();
    let applied = rig.stack.apply(&plan, rig.at(0)).unwrap();
    assert!(!applied.ca_created && applied.certificate.is_none());
    assert!(!rig.paths.certs_dir().exists());
}

#[test]
fn securing_another_site_issues_a_new_certificate_and_reloads() {
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.site("blog", None);
    rig.secure("shop");
    rig.run_with(&rig.plan_with_secure());
    let before = fs::read(rig.paths.site_cert()).unwrap();

    rig.secure("blog");
    let applied = rig
        .stack
        .apply(&rig.plan_with_secure(), rig.at(1000))
        .unwrap();
    assert_eq!(applied.certificate, Some(crate::tls::Renewal::Names));
    assert!(!applied.ca_created);
    assert!(applied.reload_requested);
    assert_ne!(fs::read(rig.paths.site_cert()).unwrap(), before);
}

#[test]
fn a_certificate_made_again_reloads_nginx_even_if_the_config_text_is_the_same() {
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.secure("shop");
    rig.run_with(&rig.plan_with_secure());

    // The certificate files vanish: they are made again, with the same paths in the config.
    fs::remove_file(rig.paths.site_cert()).unwrap();
    fs::remove_file(rig.paths.site_key()).unwrap();
    let applied = rig
        .stack
        .apply(&rig.plan_with_secure(), rig.at(1000))
        .unwrap();
    assert_eq!(applied.certificate, Some(crate::tls::Renewal::Missing));
    assert!(
        !applied.config_changed,
        "the text of the config did not change"
    );
    assert!(
        applied.reload_requested,
        "but Nginx still has the old certificate in memory"
    );

    rig.tick(1100);
    assert_eq!(rig.control_commands(), [["-t"]]);
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(1300);
    assert_eq!(rig.control_commands(), [vec!["-t"], vec!["-s", "reload"]]);
}

#[test]
fn a_broken_ca_leaves_the_secured_sites_on_http_and_says_why() {
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.secure("shop");
    fs::create_dir_all(rig.paths.certs_dir()).unwrap();
    fs::write(rig.paths.ca_cert(), "garbage").unwrap();
    fs::write(rig.paths.ca_key(), "garbage").unwrap();
    let applied = rig.stack.apply(&rig.plan_with_secure(), rig.at(0)).unwrap();
    assert!(
        applied
            .warnings
            .iter()
            .any(|w| w.contains("served over HTTP")),
        "{:?}",
        applied.warnings
    );
    assert_eq!(
        fs::read(rig.paths.ca_cert()).unwrap(),
        b"garbage",
        "an unusable CA is never replaced behind the user's back"
    );
}

#[test]
fn a_certs_folder_open_to_other_accounts_keeps_the_secured_sites_on_http_and_says_why() {
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.secure("shop");
    let first = rig.stack.apply(&rig.plan_with_secure(), rig.at(0)).unwrap();
    assert!(first.ca_created, "made in a private certs folder");
    assert!(rig.dirs.created().contains(&rig.paths.certs_dir()));

    // Somebody opens the folder: from now on the CA is not used for anything.
    rig.dirs.expose(&rig.paths.certs_dir());
    let applied = rig
        .stack
        .apply(&rig.plan_with_secure(), rig.at(1000))
        .unwrap();
    assert!(
        applied
            .warnings
            .iter()
            .any(|w| w.contains("served over HTTP") && w.contains("other accounts")),
        "{:?}",
        applied.warnings
    );
    assert!(applied.certificate.is_none() && !applied.ca_created);
}

// ---- only local TLDs (review of phase 6, finding 2) ------------------------------------

#[test]
fn with_tld_dev_the_secured_sites_are_served_over_http_with_a_warning_and_no_ca_is_made() {
    let mut rig = Rig::with_config(&[], |config| config.sites.tld = "dev".to_owned());
    rig.site("shop", None);
    rig.secure("shop");
    let applied = rig.stack.apply(&rig.plan_with_secure(), rig.at(0)).unwrap();
    assert!(!applied.ca_created && applied.certificate.is_none());
    assert!(
        applied
            .warnings
            .iter()
            .any(|w| w.contains("served over HTTP") && w.contains("not a name reserved")),
        "{:?}",
        applied.warnings
    );
    assert!(
        !rig.paths.certs_dir().exists(),
        "no CA, no certificate, not even the folder"
    );
    assert_eq!(rig.dirs.created(), Vec::<PathBuf>::new());

    rig.stack.start(rig.at(0));
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(100);
    let shop = fs::read_to_string(rig.final_conf("sites/shop.conf")).unwrap();
    assert!(shop.contains("server_name shop.dev;"), "{shop}");
    assert!(
        shop.contains("listen 127.0.0.1:19080;") && !shop.contains("ssl"),
        "{shop}"
    );
}

#[test]
fn with_tld_dev_test_https_works() {
    let mut rig = Rig::with_config(&[], |config| config.sites.tld = "dev.test".to_owned());
    rig.site("shop", None);
    rig.secure("shop");
    let applied = rig.stack.apply(&rig.plan_with_secure(), rig.at(0)).unwrap();
    assert!(applied.ca_created);
    assert_eq!(applied.warnings, Vec::<String>::new());
    assert_eq!(applied.certificate, Some(crate::tls::Renewal::Missing));

    rig.stack.start(rig.at(0));
    rig.supervisor.exit(rig.last_control(), 0).unwrap();
    rig.tick(100);
    let shop = fs::read_to_string(rig.final_conf("sites/shop.conf")).unwrap();
    assert!(
        shop.contains("server_name shop.dev.test;") && shop.contains("ssl_certificate"),
        "{shop}"
    );
}

#[test]
fn an_existing_ca_does_not_get_a_certificate_for_a_tld_it_cannot_issue_for() {
    // The CA was made for `test`; the configuration later says `.example.com`-like things
    // are not allowed, and `.internal` is special-use but not covered by this CA.
    let mut rig = Rig::new(&[]);
    rig.site("shop", None);
    rig.secure("shop");
    rig.stack.apply(&rig.plan_with_secure(), rig.at(0)).unwrap();
    let cert_before = fs::read(rig.paths.site_cert()).unwrap();

    let mut other = Rig::with_config(&[], |config| config.sites.tld = "internal".to_owned());
    // Same home: reuse the CA that exists there.
    other.paths = rig.paths.clone();
    other.stack = WebStack::new(
        rig.paths.clone(),
        &other.config,
        rig.paths.bin_for("nginx", "1.28.3").unwrap(),
        other.supervisor.clone(),
        other.probe.clone(),
        rig.dirs.clone(),
    );
    other.site("shop", None);
    let applied = other
        .stack
        .apply(&other.plan_with_secure(), other.at(0))
        .unwrap();
    assert!(
        applied
            .warnings
            .iter()
            .any(|w| w.contains("served over HTTP")),
        "{:?}",
        applied.warnings
    );
    assert!(applied.certificate.is_none() && !applied.ca_created);
    assert_eq!(
        fs::read(rig.paths.site_cert()).unwrap(),
        cert_before,
        "nothing new was issued"
    );
}
